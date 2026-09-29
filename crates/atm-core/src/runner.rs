//! Turn lifecycle: preflight, spawn, stdout routing, approvals, stop sequence, finalize
//! (spec §7.7–§7.9). Owner: M3-CORE. This file holds the pieces with a fixed contract
//! (timings, classification table, capped logs), the registry of running turns, the turn
//! planning shared by `start_attempt` and `send_follow_up`, and the startup recovery; the
//! turn driver itself is in `runner/turn.rs`.

mod turn;

pub use turn::{API_KEY_STOP_NOTICE, CHANGED_AT_START_NOTICE};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atm_types::{
    AppError, ApprovalDecision, AuthState, ConfigPolicy, ErrorCode, Id, Level, PermissionMode,
    ProcessStatus, Settings, StopReason,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use crate::claude::{self, ChildEnv, Discovered, TurnArgs};
use crate::db::{AttemptCtx, AttemptRow, ProcessFinish, ProcessRow};
use crate::git::{self, ConfigSnapshot, Git};
use crate::live::LiveMsg;
use crate::normalize::{EntryOp, Normalizer, TurnResult};
use crate::wire::Pending;
use crate::{Inner, attempt_key, claude_not_found, guard, new_id, now_ms};

/// Wait for the `initialize` response (spec §7.4 step 1).
pub const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// From `result` to process exit (spec §7.4 step 3).
pub const EXIT_AFTER_RESULT: Duration = Duration::from_secs(30);
/// Budget of `Core::shutdown` (spec §7.9).
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(8);
/// Raw log caps (spec §7.5).
pub const MAX_STDOUT_LOG: u64 = 64 << 20;
pub const MAX_STDERR_LOG: u64 = 8 << 20;

/// Startup recovery (spec §7.9): SIGTERM of a verified orphan group, SIGKILL after this.
const ORPHAN_TERM: Duration = Duration::from_secs(3);
const ORPHAN_POLL: Duration = Duration::from_millis(50);
const INTERRUPTED_NOTICE: &str = "Esecuzione interrotta dal riavvio dell'app";
/// Policy Trusted, but the worktree's `.claude/**`/`.mcp.json` (or a file they run) differ
/// from the approved ones while the commit the attempt started from still has them (spec
/// §8.9): the turn runs Isolated. The Notice goes on with ` File diversi: <paths>.`
pub const UNTRUSTED_WORKTREE_NOTICE: &str = "La configurazione Claude di questo worktree \
    (.claude/, .mcp.json e i file che esegue) è diversa da quella approvata: il turno gira \
    Isolato, senza hook, server MCP e impostazioni del repository.";
/// Policy Trusted, but the worktree's configuration cannot be fingerprinted (a limit, a link
/// out of the worktree, a change during the check): Isolated. Goes on with ` Motivo: <error>`.
pub const UNVERIFIABLE_WORKTREE_NOTICE: &str = "La configurazione Claude di questo worktree \
    non si può verificare: il turno gira Isolato, senza hook, server MCP e impostazioni del \
    repository.";
/// Policy Trusted, but the commit the attempt started from (its target branch's tip then) has
/// a configuration other than the approved one: the branch moved on since the approval, or
/// the attempt targets another branch. Goes on with ` Commit di partenza: <sha> (<branch>).`
pub const UNTRUSTED_BASE_NOTICE: &str = "La configurazione Claude del commit da cui è partito \
    questo attempt (.claude/, .mcp.json e i file che esegue) non è quella approvata, per \
    esempio perché il branch target è andato avanti dopo l'approvazione: il turno gira \
    Isolato. Per fidarti della nuova, approvala di nuovo nella sicurezza del progetto.";
/// Policy Trusted, but the configuration of the commit the attempt started from cannot be
/// fingerprinted. Goes on with ` Motivo: <error>`.
pub const UNVERIFIABLE_BASE_NOTICE: &str = "La configurazione Claude del commit da cui è \
    partito questo attempt non si può verificare: il turno gira Isolato.";
/// Policy Trusted, but the worktree's settings would bill the agents outside the subscription
/// (`git::BILLING_SETTINGS_KEYS`, `git::BILLING_ENV_VARS`): Isolated, whatever the
/// fingerprint (an approval older than the check included). Goes on with ` <what sets it>.`
pub const BILLING_WORKTREE_NOTICE: &str = "La configurazione Claude di questo worktree farebbe \
    fatturare l'agente via API o da un altro provider invece che con l'abbonamento: il turno \
    gira Isolato, senza hook, server MCP e impostazioni del repository.";
/// Paths named at most by [`UNTRUSTED_WORKTREE_NOTICE`].
const MAX_NOTICE_PATHS: usize = 5;
/// A running turn stopped because the project's bypass opt-in or Trusted policy was revoked.
pub const REVOKED_NOTICE: &str = "Turno fermato: la modalità Autonoma o la configurazione \
    Attendibile del progetto è stata revocata.";

/// Phases of the stop sequence (spec §7.9): interrupt → wait `interrupt`; close stdin →
/// wait `eof`; SIGTERM the group → wait `term`; SIGKILL and reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopTimings {
    pub interrupt: Duration,
    pub eof: Duration,
    pub term: Duration,
}

impl StopTimings {
    /// User stop: 5/3/3 s (≤ 13 s in total).
    pub const NORMAL: Self = Self {
        interrupt: Duration::from_secs(5),
        eof: Duration::from_secs(3),
        term: Duration::from_secs(3),
    };
    /// App shutdown: 2/2/2 s.
    pub const SHUTDOWN: Self = Self {
        interrupt: Duration::from_secs(2),
        eof: Duration::from_secs(2),
        term: Duration::from_secs(2),
    };

    /// Each phase at its shorter value (a shutdown during a user stop compresses it).
    fn min(self, other: Self) -> Self {
        Self {
            interrupt: self.interrupt.min(other.interrupt),
            eof: self.eof.min(other.eof),
            term: self.term.min(other.term),
        }
    }
}

/// Why the app stopped a turn; the final state depends on this flag, not on the `result`
/// subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    User,
    Shutdown,
}

impl StopCause {
    pub fn stop_reason(self) -> StopReason {
        match self {
            Self::User => StopReason::UserStop,
            Self::Shutdown => StopReason::AppShutdown,
        }
    }
}

/// What the runner observed about one turn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnOutcome {
    pub result: Option<TurnResult>,
    /// `None` = terminated by a signal or never reaped.
    pub exit_code: Option<i32>,
    pub stop: Option<StopCause>,
    pub spawn_error: bool,
    pub init_timeout: bool,
    /// `result` arrived but the process did not exit within [`EXIT_AFTER_RESULT`].
    pub exit_timeout: bool,
    /// Killed at `system/init`, which reported an API key while the passthrough is off
    /// (`turn::API_KEY_STOP_NOTICE`).
    pub api_key_stop: bool,
    /// Killed at `system/init` of a Trusted turn whose configuration changed while the CLI
    /// started (`turn::CHANGED_AT_START_NOTICE`).
    pub config_stop: bool,
}

/// The classification table of spec §7.7 → final `(status, stop_reason)`. A classified
/// usage or billing limit is `usage_limit`, a login failure `auth_failure`; a process that
/// exits 0 without any `result` is failed without a reason, and so is one killed because it
/// would bill through an API key or its configuration changed while it started
/// (`processes.error` says why).
pub fn classify(outcome: &TurnOutcome) -> (ProcessStatus, Option<StopReason>) {
    use atm_types::LimitKind;
    if outcome.spawn_error {
        return (ProcessStatus::Failed, Some(StopReason::SpawnError));
    }
    if outcome.api_key_stop || outcome.config_stop {
        return (ProcessStatus::Failed, None);
    }
    if let Some(cause) = outcome.stop {
        return (ProcessStatus::Killed, Some(cause.stop_reason()));
    }
    if outcome.init_timeout {
        return (ProcessStatus::Failed, Some(StopReason::InitTimeout));
    }
    match &outcome.result {
        Some(result) => {
            let status = if result.is_error {
                ProcessStatus::Failed
            } else {
                ProcessStatus::Completed
            };
            let reason = if outcome.exit_timeout {
                Some(StopReason::ExitTimeout)
            } else if !result.is_error {
                None
            } else {
                match result.limit {
                    Some(LimitKind::UsageLimit | LimitKind::Billing) => {
                        Some(StopReason::UsageLimit)
                    }
                    Some(LimitKind::AuthFailure) => Some(StopReason::AuthFailure),
                    Some(LimitKind::RateLimit) | None => None,
                }
            };
            (status, reason)
        }
        None if outcome.exit_code == Some(0) => (ProcessStatus::Failed, None),
        None => (ProcessStatus::Failed, Some(StopReason::Crash)),
    }
}

/// `<data_dir>/logs/<attempt_id>/<process_id>` (spec §4); files inside are 0600.
pub fn log_dir(data_dir: &Path, attempt_id: &str, process_id: &str) -> PathBuf {
    data_dir.join("logs").join(attempt_id).join(process_id)
}

/// Append-only log file (`stdout.jsonl`, `stderr.log`) that stops writing at `cap` bytes.
#[derive(Debug)]
pub struct CappedLog {
    file: tokio::fs::File,
    written: u64,
    cap: u64,
    full: bool,
}

impl CappedLog {
    /// Creates the file with mode 0600.
    pub async fn create(path: &Path, cap: u64) -> Result<CappedLog, AppError> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .await
            .map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
        Ok(CappedLog {
            file,
            written: 0,
            cap,
            full: false,
        })
    }

    /// Appends `line` and `\n` while under the cap. Returns `true` exactly once, when the cap
    /// first drops a line (the caller then emits a warning Notice).
    pub async fn write_line(&mut self, line: &[u8]) -> std::io::Result<bool> {
        if self.full {
            return Ok(false);
        }
        let len = line.len() as u64 + 1;
        if self.written + len > self.cap {
            self.full = true;
            return Ok(true);
        }
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line);
        buf.push(b'\n');
        self.file.write_all(&buf).await?;
        self.file.flush().await?;
        self.written += len;
        Ok(false)
    }
}

/// A running turn in the registry: what the services need to see and to ask of it.
pub(crate) struct TurnHandle {
    pub(crate) task_id: Id,
    pub(crate) project_id: Id,
    /// Approvals waiting for the user, by `approval_id` (in memory only, spec §7.8).
    pending: Mutex<HashMap<Id, Pending>>,
    /// What the turn's argv grants, set at launch (`None` while the slot is only reserved).
    caps: Mutex<Option<TurnCaps>>,
    cmd: mpsc::UnboundedSender<Cmd>,
    done: watch::Receiver<bool>,
}

/// What a launched turn runs with (M6): what a revocation must stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnCaps {
    /// `--allow-dangerously-skip-permissions`.
    pub(crate) bypass: bool,
    /// Without the isolation flags: the repository's configuration is loaded.
    pub(crate) trusted: bool,
}

impl TurnCaps {
    fn of(argv: &[String]) -> TurnCaps {
        TurnCaps {
            bypass: argv
                .iter()
                .any(|a| a == "--allow-dangerously-skip-permissions"),
            trusted: !argv.iter().any(|a| a == "--strict-mcp-config"),
        }
    }
}

enum Cmd {
    Stop(StopCause, StopTimings),
    Notice(Level, String),
    Respond {
        approval_id: Id,
        decision: ApprovalDecision,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
}

pub(crate) fn not_pending() -> AppError {
    AppError::not_found("L'approvazione non è più in attesa")
}

fn closing() -> AppError {
    AppError::busy("L'app si sta chiudendo")
}

impl TurnHandle {
    pub(crate) fn pending_count(&self) -> u32 {
        guard(&self.pending).len() as u32
    }

    /// Starts (or compresses) the stop sequence; returns at once.
    pub(crate) fn stop(&self, cause: StopCause, timings: StopTimings) {
        let _ = self.cmd.send(Cmd::Stop(cause, timings));
    }

    /// Adds a Notice to the turn's transcript; returns at once.
    pub(crate) fn notice(&self, level: Level, text: &str) {
        let _ = self.cmd.send(Cmd::Notice(level, text.to_owned()));
    }

    pub(crate) fn caps(&self) -> Option<TurnCaps> {
        *guard(&self.caps)
    }

    /// Answers a pending `can_use_tool` through the turn. Errors: `NotFound`.
    pub(crate) async fn respond(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), AppError> {
        if !guard(&self.pending).contains_key(approval_id) {
            return Err(not_pending());
        }
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::Respond {
            approval_id: approval_id.to_owned(),
            decision,
            reply,
        };
        self.cmd.send(cmd).map_err(|_| not_pending())?;
        answer.await.unwrap_or_else(|_| Err(not_pending()))
    }

    /// Waits until the turn is finalized and out of the registry.
    pub(crate) async fn finished(&self) {
        let mut done = self.done.clone();
        let _ = done.wait_for(|done| *done).await;
    }
}

/// What the turn task and its supervisor own of the registry entry.
struct TurnParts {
    handle: Arc<TurnHandle>,
    cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    /// Set by the supervisor once the turn is finalized (or cleaned up after a panic).
    done_tx: watch::Sender<bool>,
}

/// A reserved place in the registry (it counts against `max_running`); released on drop
/// unless the turn is launched.
pub(crate) struct Slot {
    inner: Arc<Inner>,
    attempt_id: Id,
    parts: Option<TurnParts>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(parts) = &self.parts {
            self.inner.release(&self.attempt_id, &parts.handle);
        }
    }
}

/// Spec §7.7 step 2, the checks shared by every turn, with the slot they reserved.
pub(crate) struct Preflight {
    claude: Discovered,
    /// Warnings for the transcript (auth not verifiable).
    notices: Vec<String>,
    slot: Slot,
}

/// Inputs of [`Inner::plan_turn`].
pub(crate) struct TurnRequest<'a> {
    pub(crate) ctx: &'a AttemptCtx,
    pub(crate) settings: &'a Settings,
    pub(crate) preflight: &'a Preflight,
    pub(crate) seq: u32,
    /// `processes.prompt`: the user's text.
    pub(crate) prompt: String,
    /// The message sent on stdin (the first turn and `fresh_session` add the task).
    pub(crate) stdin_prompt: String,
    pub(crate) mode: PermissionMode,
    pub(crate) resume: bool,
    pub(crate) head_before: Option<String>,
    /// `Db::next_entry_idx` of the attempt.
    pub(crate) next_idx: u32,
}

/// Everything a turn task needs; `process` is the row inserted before the spawn.
pub(crate) struct TurnPlan {
    pub(crate) ctx: AttemptCtx,
    pub(crate) process: ProcessRow,
    stdin_prompt: String,
    argv: Vec<String>,
    env: ChildEnv,
    notices: Vec<String>,
    git: Git,
    next_idx: u32,
    /// `Settings::allow_env_api_key` when the turn was planned (its environment has the key
    /// or not): an API key reported at `system/init` stops the turn unless it is on.
    allow_api_key: bool,
}

/// The Notice of a Trusted turn whose worktree does not have the approved configuration: the
/// commit the attempt started from (`base`) has another one or cannot be verified (then the
/// approval is what must change), else the worktree differs from it (with the paths) or
/// cannot be verified.
fn untrusted_notice(
    approved: Option<&str>,
    worktree: &Result<ConfigSnapshot, AppError>,
    base: &Result<ConfigSnapshot, AppError>,
    attempt: &AttemptRow,
) -> String {
    let short = attempt.base_commit.get(..7).unwrap_or(&attempt.base_commit);
    match (worktree, base) {
        (_, Err(e)) => format!("{UNVERIFIABLE_BASE_NOTICE} Motivo: {}", e.message),
        (_, Ok(b)) if approved != Some(b.fingerprint.as_str()) || !b.billing.is_empty() => {
            format!(
                "{UNTRUSTED_BASE_NOTICE} Commit di partenza: {short} ({}).",
                attempt.target_branch
            )
        }
        (Err(e), Ok(_)) => format!("{UNVERIFIABLE_WORKTREE_NOTICE} Motivo: {}", e.message),
        (Ok(w), Ok(b)) => {
            let paths = git::differing_paths(b, w);
            let mut named = paths
                .iter()
                .take(MAX_NOTICE_PATHS)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if paths.len() > MAX_NOTICE_PATHS {
                named.push_str(&format!(" e altri {}", paths.len() - MAX_NOTICE_PATHS));
            }
            format!("{UNTRUSTED_WORKTREE_NOTICE} File diversi: {named}.")
        }
    }
}

impl Inner {
    /// CLI found · logged in (Unknown → allowed with a Notice) · not paused · a free slot.
    /// Errors: `ClaudeNotFound`, `NotLoggedIn`, `UsageLimited`, `ConcurrencyLimit`, `Busy`.
    pub(crate) async fn preflight(
        self: &Arc<Self>,
        attempt_id: &str,
        task_id: &str,
        project_id: &str,
        settings: &Settings,
    ) -> Result<Preflight, AppError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(closing());
        }
        let probe = self.probe(false).await;
        let claude = probe.claude.clone().ok_or_else(claude_not_found)?;
        let mut notices = Vec::new();
        match &probe.auth {
            AuthState::LoggedIn { .. } => {}
            AuthState::LoggedOut => {
                return Err(AppError::new(
                    ErrorCode::NotLoggedIn,
                    "Accedi con Claude Code per avviare l'agente",
                ));
            }
            AuthState::Unknown { reason } => notices.push(format!(
                "Stato dell'accesso non verificabile ({reason}): avvio comunque"
            )),
        }
        if let Some(text) = guard(&self.paused).clone() {
            return Err(AppError::new(
                ErrorCode::UsageLimited,
                format!("Agenti in pausa per il limite d'uso: {text}"),
            ));
        }
        let slot = self.reserve(attempt_id, task_id, project_id, settings.max_running)?;
        Ok(Preflight {
            claude,
            notices,
            slot,
        })
    }

    /// Errors: `Busy` (the attempt has a turn), `ConcurrencyLimit` (`max_running` reached).
    fn reserve(
        self: &Arc<Self>,
        attempt_id: &str,
        task_id: &str,
        project_id: &str,
        max_running: u32,
    ) -> Result<Slot, AppError> {
        let (cmd, cmd_rx) = mpsc::unbounded_channel();
        let (done_tx, done) = watch::channel(false);
        let handle = Arc::new(TurnHandle {
            task_id: task_id.to_owned(),
            project_id: project_id.to_owned(),
            pending: Mutex::default(),
            caps: Mutex::default(),
            cmd,
            done,
        });
        let mut turns = guard(&self.turns);
        // `Core::shutdown` sets it under this lock (the preflight may have taken seconds).
        if self.closing.load(Ordering::SeqCst) {
            return Err(closing());
        }
        if turns.contains_key(attempt_id) {
            return Err(crate::busy_turn());
        }
        if turns.len() >= max_running as usize {
            return Err(AppError::new(
                ErrorCode::ConcurrencyLimit,
                format!(
                    "Già {} agenti in esecuzione: il massimo è {max_running}",
                    turns.len()
                ),
            ));
        }
        turns.insert(attempt_id.to_owned(), Arc::clone(&handle));
        Ok(Slot {
            inner: Arc::clone(self),
            attempt_id: attempt_id.to_owned(),
            parts: Some(TurnParts {
                handle,
                cmd_rx,
                done_tx,
            }),
        })
    }

    /// Removes `handle` from the registry (only that one: a later turn keeps its entry).
    fn release(&self, attempt_id: &str, handle: &Arc<TurnHandle>) {
        let mut turns = guard(&self.turns);
        if turns
            .get(attempt_id)
            .is_some_and(|t| Arc::ptr_eq(t, handle))
        {
            turns.remove(attempt_id);
        }
    }

    /// Why a Trusted turn of `ctx` must not load the repository's configuration (the Notice),
    /// or `None` when its worktree has the approved one (M6, spec §8.9): settings that would
    /// bill outside the subscription come first, whatever the fingerprint (an approval older
    /// than that check included); the commit the attempt started from is read only when the
    /// worktree does not match ([`untrusted_notice`]).
    pub(crate) async fn untrusted_reason(&self, git: &Git, ctx: &AttemptCtx) -> Option<String> {
        let (project, a) = (&ctx.project, &ctx.attempt);
        let approved = project.trusted_fingerprint.as_deref();
        let here = self.config_snapshot(Path::new(&a.worktree_path)).await;
        if let Ok(w) = &here {
            if !w.billing.is_empty() {
                return Some(format!(
                    "{BILLING_WORKTREE_NOTICE} {}.",
                    w.billing.join("; ")
                ));
            }
            if Some(w.fingerprint.as_str()) == approved {
                return None;
            }
        }
        let repo = Path::new(&project.repo_path);
        let base = self.commit_config(git, repo, &a.base_commit).await;
        Some(untrusted_notice(approved, &here, &base, a))
    }

    /// Argv (spec §7.3), environment and process row of one turn. Policy Trusted with a
    /// fingerprint that no longer matches the worktree (or cannot be computed), or a worktree
    /// whose settings bill outside the subscription, runs Isolated, with a Notice that says
    /// why: what differs, whether the commit the attempt started from is not the approved
    /// configuration, or the error (M6, spec §8.9).
    pub(crate) async fn plan_turn(&self, r: TurnRequest<'_>) -> TurnPlan {
        let a = &r.ctx.attempt;
        let worktree = Path::new(&a.worktree_path);
        let mut notices = r.preflight.notices.clone();
        let project = &r.ctx.project;
        let tools = self.tools(false).await;
        let isolated = match project.config_policy {
            ConfigPolicy::Isolated => true,
            ConfigPolicy::Trusted => match self.untrusted_reason(&tools.git, r.ctx).await {
                None => false,
                Some(notice) => {
                    eprintln!("attempt {}: runs Isolated: {notice}", a.id);
                    notices.push(notice);
                    true
                }
            },
        };
        let argv = claude::build_argv(&TurnArgs {
            claude: r.preflight.claude.path.clone(),
            permission_mode: r.mode,
            allow_bypass: r.ctx.project.allow_bypass,
            session_id: a.session_id.clone(),
            resume: r.resume,
            isolated,
            allow_rules: a.allow_rules.clone(),
            model: a.model.clone(),
            effort: a.effort,
            append_prompt: claude::append_prompt(worktree, &a.branch, &a.target_branch),
        });
        let env = self
            .child_env(&tools, r.settings)
            .for_attempt(worktree, &a.id);
        let process = ProcessRow {
            id: new_id(),
            attempt_id: a.id.clone(),
            seq: r.seq,
            prompt: r.prompt,
            permission_mode: r.mode,
            session_id: a.session_id.clone(),
            resumed: r.resume,
            status: ProcessStatus::Running,
            stop_reason: None,
            error: None,
            cli_version: Some(r.preflight.claude.version.clone()),
            argv_json: serde_json::to_string(&argv).unwrap_or_else(|_| "[]".into()),
            pid: None,
            app_instance_id: self.app_instance_id.clone(),
            exit_code: None,
            result_subtype: None,
            is_error: None,
            cost_usd_estimate: None,
            num_turns: None,
            duration_ms: None,
            head_before: r.head_before,
            head_after: None,
            started_at: now_ms(),
            finished_at: None,
        };
        TurnPlan {
            ctx: r.ctx.clone(),
            process,
            stdin_prompt: r.stdin_prompt,
            argv,
            env,
            notices,
            git: tools.git.clone(),
            next_idx: r.next_idx,
            allow_api_key: r.settings.allow_env_api_key,
        }
    }

    /// Announces the running count (the slot is in it already), then spawns the turn task
    /// on the reserved slot; its process row is committed.
    pub(crate) async fn launch(self: &Arc<Self>, preflight: Preflight, plan: TurnPlan) {
        self.emit_env().await;
        let mut slot = preflight.slot;
        let Some(TurnParts {
            handle,
            cmd_rx,
            done_tx,
        }) = slot.parts.take()
        else {
            return;
        };
        let caps = TurnCaps::of(&plan.argv);
        *guard(&handle.caps) = Some(caps);
        // A revocation committed since the project was read (before the slot existed) is seen
        // here; one committed after this read sees the caps set above and stops the turn.
        if let Ok(row) = self.db.project(&plan.ctx.project.id)
            && ((caps.bypass && !row.allow_bypass)
                || (caps.trusted && row.config_policy == ConfigPolicy::Isolated))
        {
            handle.notice(Level::Warn, REVOKED_NOTICE);
            handle.stop(StopCause::User, StopTimings::NORMAL);
        }
        let (attempt_id, process_id) = (plan.ctx.attempt.id.clone(), plan.process.id.clone());
        let task = tokio::spawn(turn::run_turn(
            Arc::clone(self),
            plan,
            Arc::clone(&handle),
            cmd_rx,
        ));
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            // A panicking turn must not keep its slot and a `running` row forever.
            if let Err(e) = task.await
                && e.is_panic()
            {
                inner.turn_panicked(&attempt_id, &process_id, &handle).await;
            }
            let _ = done_tx.send(true);
        });
    }

    /// The unwinding already killed the agent's group (`turn::drive`); the rest of finalize.
    async fn turn_panicked(&self, attempt_id: &str, process_id: &str, handle: &Arc<TurnHandle>) {
        guard(&handle.pending).clear();
        self.cancel_open_tools(attempt_id, process_id);
        let fin = ProcessFinish {
            status: ProcessStatus::Failed,
            stop_reason: Some(StopReason::Crash),
            error: Some("errore interno dell'app durante il turno".into()),
            exit_code: None,
            result_subtype: None,
            is_error: None,
            cost_usd_estimate: None,
            num_turns: None,
            duration_ms: None,
            head_after: None,
            finished_at: now_ms(),
        };
        if let Err(e) = self.db.finish_process(process_id, &fin) {
            eprintln!("process {process_id}: {e}");
        }
        self.release(attempt_id, handle);
        self.emit_changed(Some(&handle.project_id), Some(&handle.task_id));
        self.emit_env().await;
    }

    /// Persists the entries, then broadcasts them (spec §6.5: the DB first); typing
    /// previews only go to the live channel.
    fn publish(&self, attempt_id: &str, ops: Vec<EntryOp>) {
        let mut entries = Vec::new();
        let mut typing = Vec::new();
        for op in ops {
            match op {
                EntryOp::Upsert(entry) => entries.push(entry),
                EntryOp::Typing(text) => typing.push(text),
            }
        }
        if !entries.is_empty()
            && let Err(e) = self.db.upsert_entries(attempt_id, &entries)
        {
            eprintln!("attempt {attempt_id}: entries not saved: {e}");
        }
        for entry in entries {
            self.live.send(attempt_id, LiveMsg::Upsert(Arc::new(entry)));
        }
        for text in typing {
            self.live.send(attempt_id, LiveMsg::Typing(text));
        }
    }

    /// Open tool calls of a turn that died without its finalize → `Cancelled`, broadcast.
    fn cancel_open_tools(&self, attempt_id: &str, process_id: &str) {
        match self.db.cancel_open_tools(attempt_id, process_id) {
            Ok(entries) => {
                for entry in entries {
                    self.live.send(attempt_id, LiveMsg::Upsert(Arc::new(entry)));
                }
            }
            Err(e) => eprintln!("process {process_id}: open tools not cancelled: {e}"),
        }
    }

    /// Auto-commit at the end of a turn (spec §8.5) and its Notice, if any.
    async fn autocommit(
        &self,
        git: &Git,
        ctx: &AttemptCtx,
        seq: u32,
        prompt: &str,
    ) -> Option<(Level, String)> {
        let worktree = Path::new(&ctx.attempt.worktree_path);
        let message = git::turn_commit_message(seq, prompt);
        match git.autocommit(worktree, &message).await {
            Ok(Some(commit)) => Some((
                Level::Info,
                format!(
                    "Commit automatico {} ({} file)",
                    commit.commit.get(..7).unwrap_or(&commit.commit),
                    commit.files
                ),
            )),
            Ok(None) => None,
            Err(e) => {
                let e = self.missing_on(e, ctx);
                Some((
                    Level::Error,
                    format!("Commit automatico non riuscito: {}", e.message),
                ))
            }
        }
    }

    /// Spec §7.9 recovery of the turns of previous app instances, in parallel: verified
    /// kill of the group, Notice, cancelled tools, auto-commit.
    pub(crate) async fn recover_orphans(self: &Arc<Self>) -> Result<(), AppError> {
        let orphans = self.db.mark_orphans(&self.app_instance_id, now_ms())?;
        let mut tasks = JoinSet::new();
        for process in orphans {
            let inner = Arc::clone(self);
            tasks.spawn(async move { inner.recover(process).await });
        }
        while tasks.join_next().await.is_some() {}
        Ok(())
    }

    async fn recover(&self, p: ProcessRow) {
        if let Some(pgid) = p.pid
            && is_our_orphan(&p, pgid).await
        {
            kill_orphan(pgid).await;
        }
        let ctx = match self.db.attempt_ctx(&p.attempt_id) {
            Ok(ctx) => ctx,
            Err(e) => return eprintln!("recovery of process {}: {e}", p.id),
        };
        let _attempt = self.lock(attempt_key(&ctx.attempt.id)).await;
        self.cancel_open_tools(&ctx.attempt.id, &p.id);
        let next_idx = match self.db.next_entry_idx(&ctx.attempt.id) {
            Ok(idx) => idx,
            Err(e) => return eprintln!("recovery of process {}: {e}", p.id),
        };
        let mut normalizer =
            Normalizer::new(p.id.clone(), next_idx, ctx.attempt.worktree_path.clone());
        let mut ops = normalizer.on_notice(Level::Warn, INTERRUPTED_NOTICE, None, now_ms());
        let mut head_after = None;
        if ctx.attempt.worktree_state == atm_types::WorktreeState::Present {
            let git = self.git().await;
            if let Some((level, text)) = self.autocommit(&git, &ctx, p.seq, &p.prompt).await {
                ops.extend(normalizer.on_notice(level, &text, None, now_ms()));
            }
            head_after = git.head(Path::new(&ctx.attempt.worktree_path)).await.ok();
        }
        self.publish(&ctx.attempt.id, ops);
        let fin = ProcessFinish {
            status: ProcessStatus::Failed,
            stop_reason: Some(StopReason::AppRestart),
            error: p.error,
            exit_code: None,
            result_subtype: p.result_subtype,
            is_error: p.is_error,
            cost_usd_estimate: p.cost_usd_estimate,
            num_turns: p.num_turns,
            duration_ms: p.duration_ms,
            head_after,
            finished_at: p.finished_at.unwrap_or_else(now_ms),
        };
        if let Err(e) = self.db.finish_process(&p.id, &fin) {
            eprintln!("recovery of process {}: {e}", p.id);
        }
    }
}

/// The group leader is alive and `ps` shows the claude path of the turn's argv and its
/// session id: a UUID, so a reused pid cannot match (spec §7.9).
async fn is_our_orphan(p: &ProcessRow, pgid: i32) -> bool {
    let claude = serde_json::from_str::<Vec<String>>(&p.argv_json)
        .ok()
        .and_then(|argv| argv.into_iter().next());
    let Some(claude) = claude else {
        return false;
    };
    if uuid::Uuid::parse_str(&p.session_id).is_err() {
        return false;
    }
    claude::pid_alive(pgid)
        && claude::process_command(pgid)
            .await
            .is_some_and(|command| command.contains(&claude) && command.contains(&p.session_id))
}

/// SIGTERM to the group, up to [`ORPHAN_TERM`] for it to go, then SIGKILL; the leader's
/// descendants (collected before the SIGTERM: the CLI's Bash commands run in groups of their
/// own, M5) get SIGKILL too if still alive.
async fn kill_orphan(pgid: i32) {
    let mut tree = claude::descendants(pgid);
    let _ = claude::killpg(pgid, libc::SIGTERM);
    let deadline = tokio::time::Instant::now() + ORPHAN_TERM;
    while claude::group_alive(pgid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(ORPHAN_POLL).await;
    }
    if claude::group_alive(pgid) {
        tree.extend(claude::descendants(pgid));
        let _ = claude::killpg(pgid, libc::SIGKILL);
    }
    let alive: Vec<i32> = tree.into_iter().filter(|&p| claude::pid_alive(p)).collect();
    claude::kill_all(&alive, libc::SIGKILL);
}
