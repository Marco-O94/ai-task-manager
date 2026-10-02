//! The autopilot (round of 2026-10-01): one scheduler task per Core that starts the queued
//! tasks (`auto`, `after_id`) of the projects that have it on, as slots free up, and the chain
//! that follows a turn of such a task: verification ([`verify`]), fix follow-ups, merge. Its
//! whole state is in the DB (`tasks.auto`, `tasks.after_id`, `attempts.verify_*`), so a
//! restart rebuilds it (a fix waiting for a slot included, `attempts.verify_pending`); only
//! the verifications running now live in memory.

mod verify;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use atm_types::{
    AppError, AttemptIdReq, AttemptState, AttemptView, ErrorCode, Id, MergeAttemptReq,
    MergeOutcome, ProcessStatus, SendFollowUpReq, StartAttemptReq, StopReason, Task, TaskKind,
    VerifyState, merge_message,
};
use tokio::sync::Notify;

use crate::db::{AttemptCtx, ProjectRow};
use crate::{Core, Inner, guard};

/// Safety tick of the scheduler, besides its wake-ups.
const TICK: Duration = Duration::from_secs(60);
/// Title of every notification of the autopilot.
const TITLE: &str = "Autopilota";
/// Verifications in a row when the HEAD moves between a passed one and its merge.
const MAX_VERIFY_ROUNDS: usize = 3;

#[derive(Default)]
pub(crate) struct Autopilot {
    /// Wakes the scheduler: a permit is kept when it is busy, so no wake-up is lost.
    wake: Arc<Notify>,
    /// Attempts whose verification runs now (`TaskCard::verifying`, `merge_blocked`), with
    /// the process group of its command once spawned (killed by `shutdown`, a discard, a
    /// delete).
    verifying: Mutex<HashMap<Id, Option<i32>>>,
}

impl Autopilot {
    /// Asks the scheduler for a pass: a slot, a task or a project setting changed.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn is_verifying(&self, attempt_id: &str) -> bool {
        guard(&self.verifying).contains_key(attempt_id)
    }

    /// Kills the command of the attempts' running verifications (`None`: every one), with
    /// what it started: the verification ends as failed and lets the attempt's lock go.
    pub(crate) async fn kill_verifies(&self, attempt_ids: Option<&[Id]>) {
        let groups: Vec<i32> = guard(&self.verifying)
            .iter()
            .filter(|(id, _)| attempt_ids.is_none_or(|ids| ids.contains(id)))
            .filter_map(|(_, pgid)| *pgid)
            .collect();
        if !groups.is_empty() {
            let kill = move || groups.into_iter().for_each(verify::kill_group);
            let _ = tokio::task::spawn_blocking(kill).await;
        }
    }
}

/// Spawns the scheduler of `inner`: a pass at once, then at every wake-up and every [`TICK`],
/// until the core closes or is gone.
pub(crate) fn spawn(inner: &Arc<Inner>) {
    let (weak, wake) = (Arc::downgrade(inner), Arc::clone(&inner.autopilot.wake));
    tokio::spawn(run(weak, wake));
}

async fn run(inner: Weak<Inner>, wake: Arc<Notify>) {
    loop {
        // Not held while waiting: a dropped Core goes away.
        let Some(s) = inner.upgrade() else { return };
        if s.closing.load(Ordering::SeqCst) {
            return;
        }
        s.schedule().await;
        drop(s);
        let _ = tokio::time::timeout(TICK, wake.notified()).await;
    }
}

/// The follow-up of one turn, as a boxed `Send` future: it may launch a turn, whose
/// supervisor spawns this again, and the compiler cannot prove that cycle `Send` through an
/// opaque type.
fn after_turn(
    inner: Arc<Inner>,
    attempt_id: Id,
    process_id: Id,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move { inner.after_turn(&attempt_id, &process_id).await })
}

/// The follow-up prompt of a failed verification (English: it goes to the agent). fake-claude
/// plays `fix_on_resume` for its first words.
fn fix_prompt(command: &str, how: &str, tail: &str) -> String {
    let output = if tail.is_empty() {
        "It printed nothing.".to_owned()
    } else {
        format!("The end of its output:\n\n```\n{tail}\n```")
    };
    format!(
        "The project's verification command `{command}` {how} in this worktree. {output}\n\n\
         Fix the cause, run the command again to check it passes, and commit your changes. Do \
         not push."
    )
}

/// The "Risolvi con l'agente" follow-up (spec §8.7), the same text the UI sends
/// (`ui/src/views/diff.rs`).
fn conflict_prompt(target: &str, files: &[String]) -> String {
    format!(
        "This branch conflicts with `{target}` in: {}. Run `git merge {target}`, resolve every \
         conflict preserving both intents, run the project's tests if available, and commit the \
         merge. Do not push.",
        files.join(", ")
    )
}

/// What comes after a passed verification with «Merge automatico».
enum Merged {
    Done,
    /// The HEAD is no longer the verified one: verify again.
    Moved,
}

impl Inner {
    /// Called by every turn's supervisor once the slot is free (both paths): wakes the
    /// scheduler and runs the autopilot's follow-up of the turn in the background. A fix still
    /// waiting for a slot is moot (a turn just ran: its end is verified again).
    pub(crate) fn turn_ended(self: &Arc<Self>, attempt_id: Id, process_id: Id) {
        if let Err(e) = self
            .db
            .set_verify_pending(&attempt_id, None, crate::now_ms())
        {
            eprintln!("autopilot {attempt_id}: {e}");
        }
        self.autopilot.wake();
        tokio::spawn(after_turn(Arc::clone(self), attempt_id, process_id));
    }

    /// The task's autopilot is off from now on (`auto = 0`), with a notification saying why.
    fn let_go(&self, ctx: &AttemptCtx, body: &str) {
        if let Err(e) = self.db.set_task_auto(&ctx.task.id, false, crate::now_ms()) {
            eprintln!("autopilot {}: {e}", ctx.task.id);
        }
        self.notify(Some(&ctx.task.id), TITLE, body);
        self.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
    }

    /// Startup: a verification cut by the app closing (now `error`).
    pub(crate) fn stale_verify(&self, attempt_id: &str) {
        if let Ok(ctx) = self.db.attempt_ctx(attempt_id) {
            let body = format!(
                "La verifica di «{}» è stata interrotta dalla chiusura dell'app",
                ctx.task.title
            );
            self.notify(Some(&ctx.task.id), TITLE, &body);
        }
    }

    /// One pass: while slots are free, sends the fixes waiting for one (an attempt under way
    /// goes before the queue), then starts the candidates of every project with the autopilot
    /// on (`Db::autopilot_candidates`) as [`Inner::start_queued`] does. Nothing starts while
    /// the core is paused for the usage limit (announced by the turn that hit it) or closing.
    async fn schedule(self: &Arc<Self>) {
        let projects = match self.db.projects() {
            Ok(projects) => projects,
            Err(e) => return eprintln!("autopilot: {e}"),
        };
        // The autopilot's projects, and those with tasks a plan handed over (`launch`).
        let launching = match self.db.projects_with_launch() {
            Ok(ids) => ids,
            Err(e) => return eprintln!("autopilot: {e}"),
        };
        let projects: Vec<_> = projects
            .into_iter()
            .filter(|p| p.autopilot || launching.contains(&p.id))
            .collect();
        if projects.is_empty() {
            return;
        }
        if guard(&self.paused).is_some() {
            return;
        }
        match self.db.pending_fixes() {
            Ok(fixes) => {
                for (attempt_id, prompt) in fixes {
                    match self.autopilot_ctx(&attempt_id) {
                        Some(ctx) => {
                            if !self.deliver_fix(&ctx, prompt).await {
                                return;
                            }
                        }
                        None => self.clear_pending(&attempt_id),
                    }
                }
            }
            Err(e) => eprintln!("autopilot: {e}"),
        }
        for project in projects {
            let tasks = match self.db.autopilot_candidates(&project.id, project.autopilot) {
                Ok(tasks) => tasks,
                Err(e) => {
                    eprintln!("autopilot {}: {e}", project.id);
                    continue;
                }
            };
            for task in tasks {
                match self.start_queued(&project, &task).await {
                    Ok(_) => {}
                    // No slot, paused or closing: the queue waits for the next wake-up.
                    Err(e)
                        if matches!(
                            e.code,
                            ErrorCode::ConcurrencyLimit | ErrorCode::UsageLimited | ErrorCode::Busy
                        ) =>
                    {
                        return;
                    }
                    // Started meanwhile (by the user or an agent).
                    Err(e) if e.code == ErrorCode::Conflict => {}
                    Err(e) => {
                        let now = crate::now_ms();
                        if let Err(e) = self
                            .db
                            .set_task_auto(&task.id, false, now)
                            .and_then(|()| self.db.set_task_launch(&task.id, false, now))
                        {
                            eprintln!("autopilot {}: {e}", task.id);
                        }
                        let body = format!("Impossibile avviare «{}»: {}", task.title, e.message);
                        self.notify(Some(&task.id), TITLE, &body);
                        self.emit_changed(Some(&project.id), Some(&task.id));
                    }
                }
            }
        }
    }

    /// Starts a queued task with the project's defaults, or, when an agent gave it to the
    /// autopilot (`tasks.auto_by`), as that agent's `start_task` would: started by it (so the
    /// chain stops at depth 2, spec §10), a sub-task in its mode, its sub-agent limits.
    async fn start_queued(
        self: &Arc<Self>,
        project: &ProjectRow,
        task: &Task,
    ) -> Result<AttemptView, AppError> {
        let mut req = StartAttemptReq {
            task_id: task.id.clone(),
            target_branch: project.default_target_branch.clone(),
            permission_mode: project.default_permission_mode,
            model: None,
            effort: None,
            subagent_model: None,
            max_subagents: None,
        };
        let by = self.db.task_auto_by(&task.id)?;
        // A starter gone since (its task deleted) still counts for the depth.
        if let Some(caller) = by.as_deref().and_then(|id| self.db.attempt(id).ok()) {
            if task.parent_id.is_some() {
                req.permission_mode = caller.permission_mode;
            }
            req.subagent_model = caller.subagent_model;
            req.max_subagents = caller.max_subagents;
        }
        let core = Core {
            inner: Arc::clone(self),
        };
        core.start_attempt_by(req, by).await
    }

    /// The end of a turn of an autopilot task (the task's `auto` and its project's autopilot
    /// on, attempt active): completed → verification, then merge or fix; anything else (a
    /// failure, the user's Stop, a limit) → the autopilot lets the task go, never retrying.
    async fn after_turn(self: &Arc<Self>, attempt_id: &str, process_id: &str) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        // A plan's turn has an end of its own, never the autopilot's (round 2026-10-02).
        if let Ok(ctx) = self.db.attempt_ctx(attempt_id)
            && ctx.task.kind == TaskKind::Plan
        {
            return self.plan_ended(ctx, process_id).await;
        }
        let Some(ctx) = self.autopilot_ctx(attempt_id) else {
            return;
        };
        let Ok(process) = self.db.process(process_id) else {
            return;
        };
        if process.status != ProcessStatus::Completed {
            let why = match process.stop_reason {
                Some(StopReason::UserStop) => "fermato dall'utente",
                Some(StopReason::UsageLimit) => "limite d'uso raggiunto",
                Some(StopReason::AuthFailure) => "accesso a Claude Code non valido",
                _ => "turno non riuscito",
            };
            let body = format!(
                "«{}» si è fermato ({why}): l'autopilota lo lascia a te",
                ctx.task.title
            );
            return self.let_go(&ctx, &body);
        }
        for _ in 0..MAX_VERIFY_ROUNDS {
            let Some(verdict) = self.verify(attempt_id).await else {
                return;
            };
            // The settings as they are now: the user may have turned things off meanwhile.
            let Some(ctx) = self.autopilot_ctx(attempt_id) else {
                return;
            };
            match verdict.state {
                VerifyState::Passed if ctx.project.autopilot_merge => {
                    match self.merge_verified(&ctx, &verdict.head).await {
                        Merged::Done => return,
                        Merged::Moved => continue,
                    }
                }
                VerifyState::Passed => {
                    let body = format!("«{}» è verificato e pronto per il merge", ctx.task.title);
                    return self.notify(Some(&ctx.task.id), TITLE, &body);
                }
                VerifyState::Failed => {
                    let command = ctx.project.verify_command.as_deref().unwrap_or_default();
                    let prompt = fix_prompt(command, &verdict.how, &verdict.tail);
                    return self.send_fix(&ctx, prompt, false).await;
                }
                VerifyState::Error | VerifyState::Running => {
                    let body = format!(
                        "La verifica di «{}» non è riuscita a partire: {}",
                        ctx.task.title, verdict.how
                    );
                    return self.let_go(&ctx, &body);
                }
            }
        }
        if let Some(ctx) = self.autopilot_ctx(attempt_id) {
            let body = format!(
                "HEAD di «{}» continua a cambiare: merge non eseguito",
                ctx.task.title
            );
            self.let_go(&ctx, &body);
        }
    }

    /// The attempt's context while the autopilot drives it: active, its task `auto`, its
    /// project's autopilot on.
    fn autopilot_ctx(&self, attempt_id: &str) -> Option<AttemptCtx> {
        let ctx = self.db.attempt_ctx(attempt_id).ok()?;
        (ctx.attempt.state == AttemptState::Active && ctx.task.auto && ctx.project.autopilot)
            .then_some(ctx)
    }

    /// A fix follow-up (`prompt`, for `conflicts` or a failed verification) when the budget
    /// allows (`verify_fixes` < `autopilot_max_fixes`), else the task is let go. None while a
    /// turn runs (the user's follow-up, sent during the verification): its end comes back to
    /// [`Inner::after_turn`].
    async fn send_fix(self: &Arc<Self>, ctx: &AttemptCtx, prompt: String, conflicts: bool) {
        let (fixes, max) = (ctx.attempt.verify_fixes, ctx.project.autopilot_max_fixes);
        if fixes >= max {
            let body = if conflicts {
                format!(
                    "Conflitti di «{}» ancora presenti dopo {fixes} tentativi di correzione",
                    ctx.task.title
                )
            } else {
                format!(
                    "Verifica di «{}» fallita dopo {fixes} tentativi di correzione",
                    ctx.task.title
                )
            };
            return self.let_go(ctx, &body);
        }
        if self.turn(&ctx.attempt.id).is_none() {
            self.deliver_fix(ctx, prompt).await;
        }
    }

    /// Sends a fix follow-up, counted in `verify_fixes` (an unsent one is not). Without a slot
    /// (`ConcurrencyLimit`), paused (`UsageLimited`) or closing, it waits in
    /// `verify_pending` for the scheduler, which returns `false` then: no slot for the queue.
    /// A turn running (`Busy`) owns the attempt; any other error lets the task go (unless the
    /// user took it back meanwhile).
    async fn deliver_fix(self: &Arc<Self>, ctx: &AttemptCtx, prompt: String) -> bool {
        let id = &ctx.attempt.id;
        // Counted before the turn can end and be verified; given back if it is not sent.
        if let Err(e) = self.db.add_verify_fix(id, crate::now_ms()) {
            eprintln!("autopilot {id}: {e}");
            return true;
        }
        let core = Core {
            inner: Arc::clone(self),
        };
        let req = SendFollowUpReq {
            attempt_id: id.clone(),
            prompt: prompt.clone(),
            permission_mode: None,
            fresh_session: false,
        };
        let Err(e) = core.send_follow_up(req).await else {
            self.clear_pending(id);
            return true;
        };
        if let Err(e) = self.db.undo_verify_fix(id, crate::now_ms()) {
            eprintln!("autopilot {id}: {e}");
        }
        let waits = matches!(
            e.code,
            ErrorCode::ConcurrencyLimit | ErrorCode::UsageLimited
        ) || self.closing.load(Ordering::SeqCst);
        if waits {
            if let Err(e) = self
                .db
                .set_verify_pending(id, Some(&prompt), crate::now_ms())
            {
                eprintln!("autopilot {id}: {e}");
            }
            return false;
        }
        self.clear_pending(id);
        // Not after a discard, a delete or the autopilot turned off meanwhile.
        if e.code != ErrorCode::Busy && self.autopilot_ctx(id).is_some() {
            let body = format!(
                "Impossibile mandare la correzione a «{}»: {}",
                ctx.task.title, e.message
            );
            self.let_go(ctx, &body);
        }
        true
    }

    fn clear_pending(&self, attempt_id: &str) {
        if let Err(e) = self
            .db
            .set_verify_pending(attempt_id, None, crate::now_ms())
        {
            eprintln!("autopilot {attempt_id}: {e}");
        }
    }

    /// «Merge automatico» after a passed verification of `head`: conflicts go back to the
    /// agent (`conflict_prompt`, counted as a fix), a HEAD that moved is verified again, else
    /// the squash merge of `head` alone with the default message ([`Core::merge_attempt_at`]).
    async fn merge_verified(self: &Arc<Self>, ctx: &AttemptCtx, head: &str) -> Merged {
        let core = Core {
            inner: Arc::clone(self),
        };
        let a = &ctx.attempt;
        let req = AttemptIdReq {
            attempt_id: a.id.clone(),
        };
        let status = match core.get_branch_status(req).await {
            Ok(status) => status,
            Err(e) => {
                let body = format!("Merge di «{}» non riuscito: {}", ctx.task.title, e.message);
                self.let_go(ctx, &body);
                return Merged::Done;
            }
        };
        // A turn started meanwhile: its end comes back here.
        if self.turn(&a.id).is_some() {
            return Merged::Done;
        }
        if !status.conflicts.is_empty() {
            let prompt = conflict_prompt(&a.target_branch, &status.conflicts);
            self.send_fix(ctx, prompt, true).await;
            return Merged::Done;
        }
        let req = MergeAttemptReq {
            attempt_id: a.id.clone(),
            message: merge_message(&ctx.task.title, &ctx.task.description, &a.id),
        };
        let body = match core.merge_attempt_at(req, head).await {
            Ok(None) => return Merged::Moved,
            Ok(Some(MergeOutcome::Merged { .. })) => {
                let body = format!("«{}» è mergiato in {}", ctx.task.title, a.target_branch);
                self.notify(Some(&ctx.task.id), TITLE, &body);
                return Merged::Done;
            }
            Ok(Some(MergeOutcome::Conflicts { files })) => {
                let prompt = conflict_prompt(&a.target_branch, &files);
                self.send_fix(ctx, prompt, true).await;
                return Merged::Done;
            }
            // A turn started, or the user merged or discarded it meanwhile.
            Err(e) if e.code == ErrorCode::Busy => return Merged::Done,
            Err(_) if self.autopilot_ctx(&a.id).is_none() => return Merged::Done,
            Ok(Some(MergeOutcome::NothingToMerge)) => {
                format!("«{}» non ha modifiche da mergiare", ctx.task.title)
            }
            Err(e) => format!("Merge di «{}» non riuscito: {}", ctx.task.title, e.message),
        };
        self.let_go(ctx, &body);
        Merged::Done
    }
}
