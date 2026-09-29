//! One turn (spec §7.4, §7.7–§7.9): spawn, stdin writer and stdout/stderr reader tasks,
//! `initialize` then the user message, stdout routing, approvals, the stop sequence and
//! finalize. The loop below owns the normalizer; everything reaches it as a message.

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use atm_types::{
    AppError, ApprovalDecision, Level, LimitKind, NoticeAction, ProcessStatus, StopReason,
};
use serde_json::Value;
use tokio::io::{AsyncRead, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use super::{
    CappedLog, Cmd, EXIT_AFTER_RESULT, INIT_TIMEOUT, MAX_STDERR_LOG, MAX_STDOUT_LOG, StopCause,
    StopTimings, TurnHandle, TurnOutcome, TurnPlan, classify, log_dir, not_pending,
};
use crate::claude;
use crate::db::ProcessFinish;
use crate::normalize::{self, EntryOp, Normalizer, TurnResult};
use crate::wire::{self, Inbound, Line, Pending};
use crate::{Inner, attempt_key, guard, new_id, now_ms};

/// Once the leader has exited, the residual group got SIGTERM: stdout/stderr must reach EOF
/// within this, else the group gets SIGKILL and at most [`DRAIN_AFTER_KILL`] more. The
/// survivors of the leader's recorded tree get SIGKILL after it too.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(3);
const DRAIN_AFTER_KILL: Duration = Duration::from_secs(1);
/// Lines from the reader tasks waiting for the turn loop.
const IO_CHANNEL: usize = 64;
/// Disallowed in v1 but may still ask (spec §7.8): answered at once, never shown as pending.
const ASK_USER_QUESTION: &str = "AskUserQuestion";
const RESUME_FAILED_NOTICE: &str =
    "La sessione di Claude Code non si può riprendere: avvia una nuova sessione";
/// A Trusted turn whose configuration changed while the CLI started (checked again at
/// `system/init`), followed by the reason: that turn is stopped, `failed` (M6, spec §8.9).
pub const CHANGED_AT_START_NOTICE: &str = "Turno fermato: la configurazione Claude del \
    worktree è cambiata mentre Claude Code si avviava.";
/// A turn whose `system/init` reports an API key while the passthrough is off (spec §7.6):
/// stopped, `failed`. Followed by ` (apiKeySource: <value>)`.
pub const API_KEY_STOP_NOTICE: &str = "Turno fermato: Claude Code userebbe una chiave API \
    invece dell'abbonamento Claude, quindi l'uso verrebbe fatturato via API. Togli la chiave \
    (variabile d'ambiente, apiKeyHelper o env nelle impostazioni di Claude Code) oppure, se \
    vuoi davvero pagare via API, attiva «Passa agli agenti la chiave API dell'ambiente» nelle \
    Impostazioni.";

/// Output of the reader tasks.
enum Io {
    Stdout(Vec<u8>),
    StdoutTooLong(usize),
    Stderr(Vec<u8>),
    StderrTooLong(usize),
}

/// SIGKILL to the turn's group and to the leader's descendants if `drive` never completes: its
/// future dropped (runtime shutdown) or unwinding. `kill_on_drop` reaches the leader only,
/// startup recovery verifies the leader, which is dead by then, and the CLI's Bash commands
/// run in groups of their own (M5).
struct KillGroupOnDrop(Option<i32>);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        if let Some(pgid) = self.0 {
            claude::kill_tree(pgid);
        }
    }
}

/// Stop sequence phases (spec §7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Interrupt,
    Eof,
    Term,
    Kill,
}

/// Body of the task spawned by `Inner::launch`: runs the turn, finalizes it, leaves the
/// registry.
pub(super) async fn run_turn(
    inner: Arc<Inner>,
    plan: TurnPlan,
    handle: Arc<TurnHandle>,
    cmd_rx: mpsc::UnboundedReceiver<Cmd>,
) {
    let normalizer = Normalizer::new(
        plan.process.id.clone(),
        plan.next_idx,
        plan.ctx.attempt.worktree_path.clone(),
    );
    let mut turn = Turn {
        inner,
        plan,
        normalizer,
        handle,
        cmd_rx,
        stdout_log: None,
        stderr_log: None,
        requests: 0,
        resume_noticed: false,
        error: None,
    };
    let ops = turn
        .normalizer
        .on_user_message(&turn.plan.stdin_prompt, now_ms());
    turn.publish(ops);
    for text in std::mem::take(&mut turn.plan.notices) {
        turn.notice(Level::Warn, &text);
    }
    let plan = &turn.plan;
    let spawned = claude::spawn(
        &plan.argv,
        Path::new(&plan.ctx.attempt.worktree_path),
        &plan.env,
    );
    let outcome = match spawned {
        Ok(spawned) => turn.drive(spawned).await,
        Err(e) => {
            turn.notice(
                Level::Error,
                &format!("Avvio di Claude Code non riuscito: {}", e.message),
            );
            turn.error = Some(e.message);
            TurnOutcome {
                spawn_error: true,
                ..TurnOutcome::default()
            }
        }
    };
    turn.finalize(outcome).await;
}

struct Turn {
    inner: Arc<Inner>,
    plan: TurnPlan,
    normalizer: Normalizer,
    handle: Arc<TurnHandle>,
    cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    stdout_log: Option<CappedLog>,
    stderr_log: Option<CappedLog>,
    /// Host requests sent so far (`atm_<n>_…` ids).
    requests: u64,
    /// The resume-failure Notice goes once per turn.
    resume_noticed: bool,
    /// `processes.error`.
    error: Option<String>,
}

/// State of the process while the turn loop runs.
struct Driver {
    pgid: i32,
    /// Frames for the writer task; `None` = stdin closed.
    stdin: Option<mpsc::Sender<Value>>,
    started: Instant,
    /// Id of the `initialize` request until its response arrives.
    init_req: Option<String>,
    /// `system/init` arrived: the session exists.
    init_seen: bool,
    result: Option<TurnResult>,
    result_at: Option<Instant>,
    stop: Option<StopCause>,
    timings: StopTimings,
    ladder: Option<(Step, Instant)>,
    /// The leader's descendants, recorded while it lived (on `result` and at the stop steps
    /// that may end it): the CLI's Bash commands lead groups of their own (M5), and once the
    /// leader is reaped they are children of launchd that nothing else links to the turn.
    tree: Vec<claude::Proc>,
    /// `Some(exit code)` once the leader is reaped (`Some(None)`: killed by a signal).
    exit: Option<Option<i32>>,
    exit_at: Instant,
    drain_killed: bool,
    io_done: bool,
    io_abandoned: bool,
    init_timeout: bool,
    exit_timeout: bool,
    /// Stopped because `system/init` reported an API key ([`Turn::stop_for_api_key`]).
    api_key_stop: bool,
    /// Stopped because the configuration changed while the CLI started
    /// ([`Turn::recheck_trust`]).
    config_stop: bool,
}

impl Driver {
    fn new(pgid: i32, stdin: mpsc::Sender<Value>, init_req: String) -> Driver {
        let now = Instant::now();
        Driver {
            pgid,
            stdin: Some(stdin),
            started: now,
            init_req: Some(init_req),
            init_seen: false,
            result: None,
            result_at: None,
            stop: None,
            timings: StopTimings::NORMAL,
            ladder: None,
            tree: Vec::new(),
            exit: None,
            exit_at: now,
            drain_killed: false,
            io_done: false,
            io_abandoned: false,
            init_timeout: false,
            exit_timeout: false,
            api_key_stop: false,
            config_stop: false,
        }
    }

    fn done(&self) -> bool {
        self.exit.is_some() && (self.io_done || self.io_abandoned)
    }

    /// Queues a frame for stdin without ever blocking the loop; `false` once stdin is closed.
    fn send(&self, frame: Value) -> bool {
        self.stdin
            .as_ref()
            .is_some_and(|tx| tx.try_send(frame).is_ok())
    }

    fn deadline(&self) -> Option<Instant> {
        if self.exit.is_some() {
            let grace = if self.drain_killed {
                DRAIN_AFTER_KILL
            } else {
                DRAIN_AFTER_EXIT
            };
            return Some(self.exit_at + grace);
        }
        if let Some((step, at)) = self.ladder {
            return match step {
                Step::Interrupt => Some(at + self.timings.interrupt),
                Step::Eof => Some(at + self.timings.eof),
                Step::Term => Some(at + self.timings.term),
                Step::Kill => None,
            };
        }
        if self.init_req.is_some() {
            return Some(self.started + INIT_TIMEOUT);
        }
        self.result_at.map(|at| at + EXIT_AFTER_RESULT)
    }

    async fn enter(&mut self, step: Step) {
        match step {
            Step::Interrupt => {}
            Step::Eof => {
                self.snapshot_tree().await;
                self.stdin = None;
            }
            Step::Term => {
                self.snapshot_tree().await;
                let _ = claude::killpg(self.pgid, libc::SIGTERM);
            }
            // The leader still runs here: its descendants in other groups (the CLI's Bash
            // commands, M5) are found and killed with it.
            Step::Kill => {
                let table = claude::process_table_async().await;
                self.record_tree(&table);
                let _ = claude::killpg(self.pgid, libc::SIGKILL);
                claude::kill_all(
                    &claude::survivors(self.pgid, &self.tree, &table),
                    libc::SIGKILL,
                );
            }
        }
        self.ladder = Some((step, Instant::now()));
    }

    /// Adds the leader's current descendants to [`Self::tree`], while it lives.
    async fn snapshot_tree(&mut self) {
        if self.exit.is_none() {
            self.record_tree(&claude::process_table_async().await);
        }
    }

    /// Adds the leader's descendants in `table` to [`Self::tree`] (a later row of a pid wins).
    fn record_tree(&mut self, table: &[claude::Proc]) {
        for p in claude::tree_of(table, self.pgid) {
            match self.tree.iter_mut().find(|t| t.pid == p.pid) {
                Some(t) => *t = p,
                None => self.tree.push(p),
            }
        }
    }

    /// Closes stdin, which ends the leader once it has answered: its tree is recorded first.
    async fn close_stdin(&mut self) {
        if self.stdin.is_some() {
            self.snapshot_tree().await;
            self.stdin = None;
        }
    }

    /// After the leader's exit (spec §7.4 step 4): SIGTERM to the residual group and to what
    /// survives of the recorded tree, SIGKILL to the latter [`DRAIN_AFTER_EXIT`] later. The
    /// agent's background jobs (a `run_in_background` dev server included) end with the turn.
    async fn end_background(&mut self) {
        let _ = claude::killpg(self.pgid, libc::SIGTERM);
        let tree = std::mem::take(&mut self.tree);
        if tree.is_empty() {
            return;
        }
        let pgid = self.pgid;
        let alive = claude::survivors(pgid, &tree, &claude::process_table_async().await);
        if alive.is_empty() {
            return;
        }
        claude::kill_all(&alive, libc::SIGTERM);
        tokio::spawn(async move {
            tokio::time::sleep(DRAIN_AFTER_EXIT).await;
            let left = claude::survivors(pgid, &tree, &claude::process_table_async().await);
            claude::kill_all(&left, libc::SIGKILL);
        });
    }
}

impl Turn {
    fn attempt_id(&self) -> &str {
        &self.plan.ctx.attempt.id
    }

    fn publish(&self, ops: Vec<EntryOp>) {
        self.inner.publish(self.attempt_id(), ops);
    }

    fn notice(&mut self, level: Level, text: &str) {
        let ops = self.normalizer.on_notice(level, text, None, now_ms());
        self.publish(ops);
    }

    fn changed(&self) {
        let ctx = &self.plan.ctx;
        self.inner
            .emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
    }

    fn next_request_id(&mut self) -> String {
        self.requests += 1;
        wire::request_id(self.requests)
    }

    /// Creates the log dir (0700) and the stdout/stderr logs; returns `stdin.jsonl`. A log
    /// that cannot be created is skipped: diagnostics never stop a turn.
    async fn open_logs(&mut self) -> Option<tokio::fs::File> {
        let dir = log_dir(
            &self.inner.config.data_dir,
            self.attempt_id(),
            &self.plan.process.id,
        );
        if let Err(e) = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
        {
            eprintln!("{}: {e}", dir.display());
            return None;
        }
        self.stdout_log = CappedLog::create(&dir.join("stdout.jsonl"), MAX_STDOUT_LOG)
            .await
            .ok();
        self.stderr_log = CappedLog::create(&dir.join("stderr.log"), MAX_STDERR_LOG)
            .await
            .ok();
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(dir.join("stdin.jsonl"))
            .await
            .ok()
    }

    async fn drive(&mut self, spawned: claude::Spawned) -> TurnOutcome {
        let claude::Spawned {
            mut child,
            pgid,
            stdin,
            stdout,
            stderr,
        } = spawned;
        // Declared after `child`, so dropped before it: the pgid is still held by the unreaped
        // leader, or by the residual group while it has members.
        let mut group = KillGroupOnDrop(Some(pgid));
        let process = &self.plan.process;
        if let Err(e) =
            self.inner
                .db
                .set_process_pid(&process.id, pgid, process.cli_version.as_deref())
        {
            eprintln!("process {}: pid not recorded: {e}", process.id);
        }
        let stdin_log = self.open_logs().await;
        let (frames, frames_rx) = mpsc::channel(wire::STDIN_CHANNEL);
        tokio::spawn(wire::write_frames(stdin, frames_rx, stdin_log));
        let (io_tx, mut io_rx) = mpsc::channel(IO_CHANNEL);
        let readers = [
            spawn_reader(
                stdout,
                wire::MAX_STDOUT_LINE,
                io_tx.clone(),
                Io::Stdout,
                Io::StdoutTooLong,
            ),
            spawn_reader(
                stderr,
                wire::MAX_STDERR_LINE,
                io_tx,
                Io::Stderr,
                Io::StderrTooLong,
            ),
        ];
        let init_req = self.next_request_id();
        let mut d = Driver::new(pgid, frames, init_req.clone());
        d.send(wire::initialize_request(&init_req));
        while !d.done() {
            let deadline = d.deadline();
            tokio::select! {
                io = io_rx.recv(), if !d.io_done => match io {
                    Some(io) => self.on_io(io, &mut d).await,
                    None => d.io_done = true,
                },
                status = child.wait(), if d.exit.is_none() => {
                    d.exit = Some(status.ok().and_then(|s| s.code()));
                    d.exit_at = Instant::now();
                    d.end_background().await;
                }
                Some(cmd) = self.cmd_rx.recv() => self.on_cmd(cmd, &mut d).await,
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.on_deadline(&mut d).await;
                }
            }
        }
        group.0 = None;
        for reader in readers {
            reader.abort();
        }
        TurnOutcome {
            result: d.result,
            exit_code: d.exit.flatten(),
            stop: d.stop,
            spawn_error: false,
            init_timeout: d.init_timeout,
            exit_timeout: d.exit_timeout,
            api_key_stop: d.api_key_stop,
            config_stop: d.config_stop,
        }
    }

    async fn on_io(&mut self, io: Io, d: &mut Driver) {
        match io {
            Io::Stdout(line) => {
                let inbound = wire::parse(&line);
                // The answer to `initialize` names the account (email, org): redacted in any
                // line that carries one, whatever its classification.
                if !matches!(inbound, Inbound::StreamEvent(_)) {
                    self.log_stdout(&wire::redact_for_log(&line)).await;
                }
                self.on_inbound(inbound, d).await;
            }
            Io::StdoutTooLong(len) => self.notice(
                Level::Warn,
                &format!("Riga di output di {len} byte (oltre 16 MiB) ignorata"),
            ),
            Io::Stderr(line) => {
                if let Some(log) = &mut self.stderr_log
                    && log.write_line(&line).await.unwrap_or(false)
                {
                    self.notice(
                        Level::Warn,
                        "Log di stderr oltre 8 MiB: il resto non è salvato",
                    );
                }
                let text = String::from_utf8_lossy(&line).into_owned();
                let ops = self.normalizer.on_stderr(&text, now_ms());
                self.publish(ops);
                if normalize::is_resume_failure(&text) {
                    self.resume_failed();
                }
            }
            Io::StderrTooLong(len) => self.notice(
                Level::Warn,
                &format!("Riga di stderr di {len} byte (oltre 64 KiB) ignorata"),
            ),
        }
    }

    async fn log_stdout(&mut self, line: &[u8]) {
        if let Some(log) = &mut self.stdout_log
            && log.write_line(line).await.unwrap_or(false)
        {
            self.notice(
                Level::Warn,
                "Log di stdout oltre 64 MiB: il resto non è salvato",
            );
        }
    }

    /// The routing table of spec §7.4.
    async fn on_inbound(&mut self, inbound: Inbound, d: &mut Driver) {
        match inbound {
            Inbound::ControlResponse { request_id, result } => {
                if d.init_req.as_deref() != Some(request_id.as_str()) {
                    return;
                }
                d.init_req = None;
                if let Err(e) = result {
                    self.notice(
                        Level::Warn,
                        &format!("Inizializzazione di Claude Code con errore: {e}"),
                    );
                }
                d.send(wire::user_message(&self.plan.stdin_prompt));
            }
            Inbound::CanUseTool(req) => self.on_can_use_tool(req, d),
            Inbound::ControlRequest {
                request_id,
                subtype,
            } => {
                let error = format!("Unsupported control request subtype: {subtype}");
                d.send(wire::control_error(&request_id, &error));
                self.notice(
                    Level::Warn,
                    &format!("Richiesta di controllo non supportata: {subtype}"),
                );
            }
            Inbound::ControlCancel { request_id } => {
                let cancelled = {
                    let mut pending = guard(&self.handle.pending);
                    let id = pending
                        .values()
                        .find(|p| p.request_id == request_id)
                        .map(|p| p.approval_id.clone());
                    id.inspect(|id| {
                        pending.remove(id);
                    })
                };
                if let Some(approval_id) = cancelled {
                    let ops = self.normalizer.on_approval_cancelled(&approval_id);
                    self.publish(ops);
                    self.changed();
                }
            }
            Inbound::KeepAlive | Inbound::NotJson => {}
            Inbound::StreamEvent(line) => {
                let ops = self.normalizer.on_line(&line, now_ms());
                self.publish(ops);
            }
            Inbound::Message(line) => {
                if let Some(session) = normalize::init_session_id(&line) {
                    let first = !d.init_seen;
                    self.on_session(session, d);
                    if first && !self.stop_for_api_key(&line, d).await {
                        self.recheck_trust(d).await;
                    }
                }
                let result = normalize::parse_result(&line);
                let ops = self.normalizer.on_line(&line, now_ms());
                self.publish(ops);
                if let Some(result) = result {
                    self.on_result(result, d).await;
                }
            }
        }
    }

    /// `system/init`: `session_started = 1`, with the observed id if it differs. Only a UUID
    /// is taken: it becomes the next `--resume=` and the proof of an orphan (spec §7.9).
    fn on_session(&mut self, observed: &str, d: &mut Driver) {
        if std::mem::replace(&mut d.init_seen, true) {
            return;
        }
        let expected = self.plan.process.session_id.clone();
        let session = if observed == expected {
            expected
        } else if uuid::Uuid::parse_str(observed).is_ok() {
            let text = format!("Claude Code ha aperto la sessione {observed} invece di {expected}");
            self.notice(Level::Warn, &text);
            observed.to_owned()
        } else {
            let text = format!("Claude Code ha indicato una sessione non valida: resta {expected}");
            self.notice(Level::Warn, &text);
            expected
        };
        if let Err(e) = self
            .inner
            .db
            .set_session(self.attempt_id(), &session, true, now_ms())
        {
            eprintln!("attempt {}: session not recorded: {e}", self.attempt_id());
        }
    }

    /// M6 (spec §8.9): a turn that loads the repository's configuration checks it again once
    /// the CLI has loaded it (`system/init`, the first one). The check before the spawn cannot
    /// see a change that lands between it and the CLI's start (a process an earlier turn left
    /// running, a hook of the configuration itself). The CLI has loaded that configuration by
    /// now (a base URL or a provider its settings just gained included) and is about to send
    /// its first request: its group is frozen (`SIGSTOP`) while the check runs, then resumed
    /// (`SIGCONT`) if nothing changed, else killed at once like an API-key stop (tree recorded,
    /// `SIGKILL`): `failed`, with [`CHANGED_AT_START_NOTICE`] and the reason as Notice and
    /// error; the next turn runs Isolated.
    async fn recheck_trust(&mut self, d: &mut Driver) {
        let trusted = !self.plan.argv.iter().any(|a| a == "--strict-mcp-config");
        if !trusted {
            return;
        }
        let frozen = d.exit.is_none() && claude::killpg(d.pgid, libc::SIGSTOP).is_ok();
        let why = self
            .inner
            .untrusted_reason(&self.plan.git, &self.plan.ctx)
            .await;
        let Some(why) = why else {
            if frozen {
                let _ = claude::killpg(d.pgid, libc::SIGCONT);
            }
            return;
        };
        if d.exit.is_none() {
            d.stdin = None;
            d.enter(Step::Kill).await;
        }
        d.config_stop = true;
        let text = format!("{CHANGED_AT_START_NOTICE} {why}");
        eprintln!("attempt {}: {text}", self.attempt_id());
        self.notice(Level::Error, &text);
        self.error = Some(text);
    }

    /// The user's requirement (2026-09-29, spec §7.6, §10.2): agents run only on the Claude
    /// subscription. A `system/init` whose `apiKeySource` names an API key
    /// ([`normalize::api_key_billing`]) while the passthrough of the environment's key is off
    /// stops the turn at once, before its first request if the kill wins the race with it
    /// (the CLI emits `system/init` just before `system/status` `requesting`): the group is
    /// frozen (`SIGSTOP`), its tree recorded, then killed (`SIGKILL`). The turn is `failed`
    /// with [`API_KEY_STOP_NOTICE`]. Returns whether it stopped.
    async fn stop_for_api_key(&mut self, line: &Value, d: &mut Driver) -> bool {
        if self.plan.allow_api_key {
            return false;
        }
        let Some(source) = normalize::api_key_billing(line) else {
            return false;
        };
        if d.exit.is_none() {
            let _ = claude::killpg(d.pgid, libc::SIGSTOP);
            d.stdin = None;
            d.enter(Step::Kill).await;
        }
        d.api_key_stop = true;
        let text = format!("{API_KEY_STOP_NOTICE} (apiKeySource: {source})");
        eprintln!("attempt {}: {text}", self.attempt_id());
        self.notice(Level::Error, &text);
        self.error = Some(text);
        true
    }

    /// `result` → close stdin; the process must then exit within [`EXIT_AFTER_RESULT`].
    async fn on_result(&mut self, result: TurnResult, d: &mut Driver) {
        if result.is_error
            && result
                .text
                .as_deref()
                .is_some_and(normalize::is_resume_failure)
        {
            self.resume_failed();
        }
        d.result = Some(result);
        d.result_at = Some(Instant::now());
        if matches!(d.ladder, Some((Step::Interrupt, _))) {
            d.enter(Step::Eof).await;
        } else {
            d.close_stdin().await;
        }
    }

    fn resume_failed(&mut self) {
        if std::mem::replace(&mut self.resume_noticed, true) {
            return;
        }
        let ops = self.normalizer.on_notice(
            Level::Error,
            RESUME_FAILED_NOTICE,
            Some(NoticeAction::NewSession),
            now_ms(),
        );
        self.publish(ops);
    }

    fn on_can_use_tool(&mut self, req: wire::CanUseTool, d: &mut Driver) {
        let approval_id = new_id();
        let pending = Pending::new(approval_id.clone(), &req);
        let ask = req.tool_name == ASK_USER_QUESTION;
        if !ask {
            // Registered before the entry is visible, so the card counts it at once.
            guard(&self.handle.pending).insert(approval_id.clone(), pending.clone());
        }
        let can_remember = wire::can_remember(&req.permission_suggestions);
        let ops = self.normalizer.on_approval_requested(
            &approval_id,
            &req.request,
            can_remember,
            now_ms(),
        );
        self.publish(ops);
        if ask {
            let decision = ApprovalDecision::Deny {
                message: wire::ASK_USER_QUESTION_DENY.into(),
                interrupt: false,
            };
            d.send(wire::approval_response(&pending, &decision));
            let ops = self
                .normalizer
                .on_approval_resolved(&approval_id, &decision);
            return self.publish(ops);
        }
        self.changed();
    }

    async fn on_cmd(&mut self, cmd: Cmd, d: &mut Driver) {
        let (approval_id, decision, reply) = match cmd {
            Cmd::Stop(cause, timings) => return self.request_stop(d, cause, timings).await,
            Cmd::Notice(level, text) => return self.notice(level, &text),
            Cmd::Respond {
                approval_id,
                decision,
                reply,
            } => (approval_id, decision, reply),
        };
        let pending = guard(&self.handle.pending).remove(&approval_id);
        let Some(pending) = pending.filter(|_| d.stdin.is_some()) else {
            let _ = reply.send(Err(not_pending()));
            return;
        };
        if !d.send(wire::approval_response(&pending, &decision)) {
            // The CLI is not reading stdin: the approval stays pending for another try.
            guard(&self.handle.pending).insert(approval_id, pending);
            let _ = reply.send(Err(AppError::busy(
                "Claude Code non sta ricevendo risposte: riprova tra poco",
            )));
            return;
        }
        let ops = self
            .normalizer
            .on_approval_resolved(&approval_id, &decision);
        self.publish(ops);
        if decision == (ApprovalDecision::Allow { remember: true }) {
            let rules = wire::remembered_rules(&pending);
            if !rules.is_empty()
                && let Err(e) = self
                    .inner
                    .db
                    .add_allow_rules(self.attempt_id(), &rules, now_ms())
            {
                eprintln!("attempt {}: allow rules not saved: {e}", self.attempt_id());
            }
        }
        self.changed();
        let _ = reply.send(Ok(()));
        // "Nega e ferma": the CLI interrupts itself; the stop sequence makes sure of it.
        if let ApprovalDecision::Deny {
            interrupt: true, ..
        } = decision
        {
            self.request_stop(d, StopCause::User, StopTimings::NORMAL)
                .await;
        }
    }

    async fn request_stop(&mut self, d: &mut Driver, cause: StopCause, timings: StopTimings) {
        if d.exit.is_some() {
            return;
        }
        let cause = *d.stop.get_or_insert(cause);
        // The `result` that answers the interrupt is an error only because of this stop.
        self.normalizer.on_stop_requested(cause.stop_reason());
        d.timings = d.timings.min(timings);
        if d.ladder.is_none() {
            self.start_ladder(d).await;
        }
    }

    /// Step 1 (interrupt) only once the session exists and stdin is open without a result.
    async fn start_ladder(&mut self, d: &mut Driver) {
        if d.init_seen && d.stdin.is_some() && d.result.is_none() {
            let id = self.next_request_id();
            d.send(wire::interrupt_request(&id));
            d.enter(Step::Interrupt).await;
        } else {
            d.enter(Step::Eof).await;
        }
    }

    async fn on_deadline(&mut self, d: &mut Driver) {
        if d.exit.is_some() {
            if d.drain_killed {
                d.io_abandoned = true;
            } else {
                let _ = claude::killpg(d.pgid, libc::SIGKILL);
                d.drain_killed = true;
                d.exit_at = Instant::now();
            }
            return;
        }
        match d.ladder.map(|(step, _)| step) {
            Some(Step::Interrupt) => d.enter(Step::Eof).await,
            Some(Step::Eof) => d.enter(Step::Term).await,
            Some(Step::Term) => d.enter(Step::Kill).await,
            Some(Step::Kill) => {}
            None if d.init_req.is_some() => {
                d.init_timeout = true;
                self.start_ladder(d).await;
            }
            None => {
                d.exit_timeout = true;
                self.start_ladder(d).await;
            }
        }
    }

    /// Spec §7.7 step 8: open tools cancelled, auto-commit, HEAD check, process row, task,
    /// pause / auth, registry, events.
    async fn finalize(mut self, outcome: TurnOutcome) {
        guard(&self.handle.pending).clear();
        let ops = self.normalizer.finish(now_ms());
        self.publish(ops);
        let (status, stop_reason) = classify(&outcome);
        if let Some((level, text)) = end_notice(&outcome, status, stop_reason) {
            self.notice(level, &text);
            if status == ProcessStatus::Failed && outcome.result.is_none() {
                self.error.get_or_insert(text);
            }
        }
        let inner = Arc::clone(&self.inner);
        let ctx = self.plan.ctx.clone();
        let worktree = Path::new(&ctx.attempt.worktree_path);
        let git = self.plan.git.clone();
        let head_after = {
            let _attempt = inner.lock(attempt_key(&ctx.attempt.id)).await;
            let (seq, prompt) = (self.plan.process.seq, &self.plan.process.prompt);
            if let Some((level, text)) = inner.autocommit(&git, &ctx, seq, prompt).await {
                self.notice(level, &text);
            }
            let head_after = git.head(worktree).await.ok();
            let expected = format!("refs/heads/{}", ctx.attempt.branch);
            if head_after.is_some() && git.head_ref(worktree).await.ok().flatten() != Some(expected)
            {
                let text = format!(
                    "HEAD del worktree non è più sul branch {}: riportalo prima del merge",
                    ctx.attempt.branch
                );
                self.notice(Level::Warn, &text);
            }
            head_after
        };
        let result = outcome.result.as_ref();
        let fin = ProcessFinish {
            status,
            stop_reason,
            error: self
                .error
                .take()
                .or_else(|| result.filter(|r| r.is_error).and_then(|r| r.text.clone())),
            exit_code: outcome.exit_code,
            result_subtype: result.map(|r| r.subtype.clone()),
            is_error: result.map(|r| r.is_error),
            cost_usd_estimate: result.and_then(|r| r.cost_usd_estimate),
            num_turns: result.and_then(|r| r.num_turns),
            duration_ms: result.and_then(|r| r.duration_ms),
            head_after,
            finished_at: now_ms(),
        };
        if let Err(e) = inner.db.finish_process(&self.plan.process.id, &fin) {
            eprintln!(
                "process {}: final state not saved: {e}",
                self.plan.process.id
            );
        }
        match result.filter(|r| r.is_error).and_then(|r| r.limit) {
            Some(LimitKind::UsageLimit | LimitKind::Billing) => {
                let text = result.and_then(|r| r.text.clone());
                *guard(&inner.paused) =
                    Some(text.unwrap_or_else(|| "limite d'uso raggiunto".into()));
            }
            // The next status check re-reads `auth status` (spec §7.10).
            Some(LimitKind::AuthFailure) => *inner.probe.lock().await = None,
            _ => {}
        }
        inner.release(&ctx.attempt.id, &self.handle);
        inner.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        inner.emit_env().await;
    }
}

/// The Notice closing a turn that did not end with its own `result` (an API-key or
/// configuration stop has said why already).
fn end_notice(
    outcome: &TurnOutcome,
    status: ProcessStatus,
    reason: Option<StopReason>,
) -> Option<(Level, String)> {
    if outcome.api_key_stop || outcome.config_stop {
        return None;
    }
    let exit = match outcome.exit_code {
        Some(code) => format!("codice {code}"),
        None => "terminato da un segnale".into(),
    };
    Some(match (status, reason) {
        (_, Some(StopReason::UserStop)) => (Level::Info, "Esecuzione fermata dall'utente".into()),
        (_, Some(StopReason::AppShutdown)) => (
            Level::Info,
            "Esecuzione fermata alla chiusura dell'app".into(),
        ),
        (_, Some(StopReason::InitTimeout)) => (
            Level::Error,
            "Claude Code non ha risposto all'inizializzazione entro 60 s".into(),
        ),
        (_, Some(StopReason::ExitTimeout)) => (
            Level::Warn,
            "Claude Code non è terminato entro 30 s dal risultato ed è stato fermato".into(),
        ),
        (_, Some(StopReason::Crash)) => (
            Level::Error,
            format!("Claude Code è terminato senza risultato ({exit})"),
        ),
        (ProcessStatus::Failed, None) if outcome.result.is_none() => (
            Level::Error,
            "Claude Code è terminato senza risultato".into(),
        ),
        _ => return None,
    })
}

/// Reads capped lines into the turn loop until EOF (`read_line_capped` is not cancel-safe,
/// so it runs in its own task).
fn spawn_reader<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    max: usize,
    tx: mpsc::Sender<Io>,
    line: fn(Vec<u8>) -> Io,
    too_long: fn(usize) -> Io,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            let io = match wire::read_line_capped(&mut reader, &mut buf, max).await {
                Ok(Line::Complete) => line(std::mem::take(&mut buf)),
                Ok(Line::TooLong(len)) => too_long(len),
                Ok(Line::Eof) | Err(_) => break,
            };
            if tx.send(io).await.is_err() {
                break;
            }
        }
    })
}
