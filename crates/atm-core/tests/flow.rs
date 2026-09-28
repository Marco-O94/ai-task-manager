//! End-to-end flows of `Core` (spec §11.2 M3-CORE): fake-claude as the CLI (configured per
//! Core through `CoreConfig::extra_env`), temporary repos, and sinks that record every app
//! event and transcript message. Waits are event-driven with generous bounds; only process
//! deaths and reaping, which no event announces, are polled.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atm_core::claude;
use atm_core::db::{AttemptRow, Db, ProcessRow, ProjectRow};
use atm_core::live::{BROADCAST_CAPACITY, Live, LiveMsg, SNAPSHOT_TAIL};
use atm_core::normalize::TurnResult;
use atm_core::runner::{self, StopCause, TurnOutcome};
use atm_core::{AppEvent, Core, CoreConfig, Notify, TranscriptSink};
use atm_types::*;
use serde_json::Value;
use tokio::sync::watch;
use tokio::time::Instant;

/// Bound of every wait for a turn that needs no stop escalation.
const TURN: Duration = Duration::from_secs(30);

// ---- harness ------------------------------------------------------------------------------

/// App events and transcript messages as they arrive; each one bumps `tick`.
#[derive(Clone)]
struct Seen {
    events: Arc<Mutex<Vec<AppEvent>>>,
    msgs: Arc<Mutex<Vec<TranscriptMsg>>>,
    tick: Arc<watch::Sender<u64>>,
}

impl Seen {
    fn new() -> Seen {
        Seen {
            events: Arc::default(),
            msgs: Arc::default(),
            tick: Arc::new(watch::channel(0).0),
        }
    }

    fn notify(&self) -> Notify {
        let seen = self.clone();
        Arc::new(move |event| {
            seen.events.lock().unwrap().push(event);
            seen.tick.send_modify(|n| *n += 1);
        })
    }

    fn sink(&self) -> TranscriptSink {
        let seen = self.clone();
        Box::new(move |msg| {
            seen.msgs.lock().unwrap().push(msg);
            seen.tick.send_modify(|n| *n += 1);
            true
        })
    }

    fn events(&self) -> Vec<AppEvent> {
        self.events.lock().unwrap().clone()
    }

    fn msgs(&self) -> Vec<TranscriptMsg> {
        self.msgs.lock().unwrap().clone()
    }

    /// The UI store (spec §6.5): `Snapshot` replaces it, `Upsert` applies by `idx` if `rev`
    /// is greater. Also returns the lowest `idx` of the last snapshot.
    fn view(&self) -> (BTreeMap<u32, Entry>, u32) {
        let mut store = BTreeMap::new();
        let mut floor = 0;
        for msg in self.msgs() {
            match msg {
                TranscriptMsg::Snapshot { entries, .. } => {
                    floor = entries.first().map_or(0, |e| e.idx);
                    store = entries.into_iter().map(|e| (e.idx, e)).collect();
                }
                TranscriptMsg::Upsert { entries } => {
                    for e in entries {
                        if store.get(&e.idx).is_none_or(|old: &Entry| e.rev > old.rev) {
                            store.insert(e.idx, e);
                        }
                    }
                }
                TranscriptMsg::Typing { .. } => {}
            }
        }
        (store, floor)
    }

    fn store(&self) -> BTreeMap<u32, Entry> {
        self.view().0
    }

    /// Re-checks `cond` after every event until it holds; panics after `limit`.
    async fn until(&self, what: &str, limit: Duration, mut cond: impl AsyncFnMut() -> bool) {
        let mut tick = self.tick.subscribe();
        let deadline = Instant::now() + limit;
        loop {
            tick.borrow_and_update();
            if cond().await {
                return;
            }
            if tokio::time::timeout_at(deadline, tick.changed())
                .await
                .is_err()
            {
                panic!("timed out after {limit:?} waiting for {what}");
            }
        }
    }
}

/// A temporary repo, its project and a Core on fake-claude.
struct Flow {
    dir: tempfile::TempDir,
    repo: PathBuf,
    config: CoreConfig,
    core: Core,
    seen: Seen,
    project: Project,
}

impl Flow {
    /// Hermetic git configuration plus `extra`.
    async fn new(extra: &[(&str, &str)]) -> Flow {
        Flow::setup(common::tempdir(), hermetic(extra), common::fake_claude()).await
    }

    async fn setup(
        dir: tempfile::TempDir,
        mut env: Vec<(OsString, OsString)>,
        claude: PathBuf,
    ) -> Flow {
        let repo = common::init_repo(&dir.path().join("repo"));
        env.push((
            "FAKE_CLAUDE_RECORD".into(),
            dir.path().join("record.jsonl").into(),
        ));
        let config = CoreConfig {
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
            claude_path: Some(claude),
            path_env: Some(std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())),
            extra_env: env,
            open_log: None,
        };
        let seen = Seen::new();
        let core = Core::new(config.clone(), seen.notify()).unwrap();
        core.startup().await.unwrap();
        let settings = Settings {
            worktree_root: dir.path().join("wt").to_string_lossy().into_owned(),
            ..core.get_settings().await.unwrap()
        };
        core.update_settings(settings).await.unwrap();
        let project = core
            .add_project(AddProjectReq {
                path: repo.to_string_lossy().into_owned(),
            })
            .await
            .unwrap()
            .project;
        Flow {
            dir,
            repo,
            config,
            core,
            seen,
            project,
        }
    }

    /// A second connection to the app's DB, for what the IPC types do not show.
    fn db(&self) -> Db {
        Db::open(&self.config.data_dir.join("atm.sqlite3")).unwrap()
    }

    fn worktrees(&self) -> usize {
        std::fs::read_dir(self.dir.path().join("wt")).map_or(0, |d| d.count())
    }

    /// Lines of `$FAKE_CLAUDE_RECORD`.
    fn record(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("record.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// argv of every `-p` call, oldest first.
    fn calls(&self) -> Vec<Vec<String>> {
        self.record()
            .into_iter()
            .filter(|r| r["kind"] == "call")
            .map(|r| serde_json::from_value(r["argv"].clone()).unwrap())
            .collect()
    }

    async fn task(&self, title: &str, description: &str) -> Task {
        let req = CreateTaskReq {
            project_id: self.project.id.clone(),
            title: title.into(),
            description: description.into(),
            status: None,
        };
        self.core.create_task(req).await.unwrap().task
    }

    async fn try_start(&self, task: &Task) -> Result<AttemptView, AppError> {
        self.try_start_as(task, PermissionMode::AcceptEdits).await
    }

    async fn try_start_as(
        &self,
        task: &Task,
        permission_mode: PermissionMode,
    ) -> Result<AttemptView, AppError> {
        let req = StartAttemptReq {
            task_id: task.id.clone(),
            target_branch: "main".into(),
            permission_mode,
            model: None,
            effort: None,
        };
        self.core.start_attempt(req).await
    }

    async fn start(&self, task: &Task) -> AttemptView {
        self.try_start(task).await.unwrap()
    }

    async fn follow_up(&self, attempt_id: &str, prompt: &str, fresh: bool) -> ProcessInfo {
        let req = SendFollowUpReq {
            attempt_id: attempt_id.into(),
            prompt: prompt.into(),
            permission_mode: None,
            fresh_session: fresh,
        };
        self.core.send_follow_up(req).await.unwrap()
    }

    async fn subscribe(&self, attempt_id: &str) {
        let req = AttemptIdReq {
            attempt_id: attempt_id.into(),
        };
        self.core
            .subscribe_transcript(req, self.seen.sink())
            .await
            .unwrap();
    }

    async fn detail(&self, task_id: &str) -> TaskDetail {
        let req = IdReq { id: task_id.into() };
        self.core.get_task_detail(req).await.unwrap()
    }

    async fn card(&self, task_id: &str) -> TaskCard {
        let req = ProjectIdReq {
            project_id: self.project.id.clone(),
        };
        let board = self.core.get_board(req).await.unwrap();
        board.into_iter().find(|c| c.task.id == task_id).unwrap()
    }

    /// Waits until turn `seq` of the task's attempt is finalized and out of the registry.
    async fn turn_end(&self, task_id: &str, seq: usize, limit: Duration) -> TaskDetail {
        let what = format!("the end of turn {seq}");
        self.seen
            .until(&what, limit, async || {
                let d = self.detail(task_id).await;
                let running = d.attempt.as_ref().is_some_and(|a| a.running);
                !running
                    && d.processes
                        .get(seq - 1)
                        .is_some_and(|p| p.status != ProcessStatus::Running)
            })
            .await;
        self.detail(task_id).await
    }

    /// Waits for an entry of the subscribed transcript.
    async fn entry(&self, what: &str, pred: impl Fn(&Entry) -> bool) -> Entry {
        let found = || self.seen.store().into_values().find(|e| pred(e));
        self.seen
            .until(what, TURN, async || found().is_some())
            .await;
        found().unwrap()
    }

    /// Persisted entries of the attempt (at most the last 200).
    async fn entries(&self, attempt_id: &str) -> Vec<Entry> {
        let req = GetEntriesReq {
            attempt_id: attempt_id.into(),
            before_idx: u32::MAX,
            limit: 200,
        };
        self.core.get_entries(req).await.unwrap().entries
    }

    async fn stop(&self, attempt_id: &str) {
        let req = AttemptIdReq {
            attempt_id: attempt_id.into(),
        };
        self.core.stop_attempt(req).await.unwrap();
    }
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs.iter().map(|(k, v)| (k.into(), v.into())).collect()
}

/// No user or system gitconfig, plus `extra`.
fn hermetic(extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    let mut env = vars(&[
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ]);
    env.extend(vars(extra));
    env
}

/// Value of `--flag=value` in an argv.
fn flag<'a>(argv: &'a [String], name: &str) -> Option<&'a str> {
    argv.iter().find_map(|a| a.strip_prefix(name))
}

fn state(p: &ProcessInfo) -> (ProcessStatus, Option<StopReason>) {
    (p.status, p.stop_reason)
}

fn is_session_init(e: &Entry) -> bool {
    matches!(e.body, EntryBody::SessionInit { .. })
}

fn notices(entries: &[Entry]) -> Vec<(Level, String, Option<NoticeAction>)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice {
                level,
                text,
                action,
            } => Some((*level, text.clone(), *action)),
            _ => None,
        })
        .collect()
}

/// One line per entry of a turn, without ids, hashes or timings.
fn outline(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| match &e.body {
            EntryBody::ToolCall { name, status, .. } => format!("ToolCall {name} {status:?}"),
            EntryBody::TurnEnd {
                subtype, is_error, ..
            } => format!("TurnEnd {subtype} is_error={is_error}"),
            EntryBody::Notice { level, text, .. } => {
                let text: String = text
                    .split(' ')
                    .map(|w| {
                        let hex = w.len() == 7 && w.chars().all(|c| c.is_ascii_hexdigit());
                        if hex { "<sha>" } else { w }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("Notice {level:?} {text}")
            }
            body => body.kind().to_owned(),
        })
        .collect()
}

/// Polls a condition that no event announces (reaping by launchd).
async fn eventually(what: &str, limit: Duration, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Blocking [`eventually`], for code outside any runtime.
fn eventually_blocking(what: &str, limit: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + limit;
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Reaps our child `pid` that a dropped or leaked runtime left behind: its pid, or -1 if
/// tokio's orphan reaper got it first. Panics if it is still alive after 10 s.
fn reap(pid: i32) -> i32 {
    let mut reaped = 0;
    eventually_blocking("the agent to be killed", Duration::from_secs(10), || {
        let mut status = 0;
        // SAFETY: a non-blocking wait for our own child; `status` outlives the call.
        reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        reaped != 0
    });
    reaped
}

// ---- flows --------------------------------------------------------------------------------

/// start → worktree → Snapshot → entries → pending approval → Allow{remember} → allow_rules
/// → result → auto-commit → inreview → diff → follow-up with `--resume` and the rules in
/// `--settings` → merge → done, worktree removed. Under a hostile global gitconfig.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordered_flow_from_start_to_merge() {
    let dir = common::tempdir();
    let hostile = common::Hostile::new(&dir.path().join("hostile"));
    let f = Flow::setup(dir, hostile.env.clone(), common::fake_claude()).await;
    let task = f
        .task("Crea hello", "Scrivi hello.txt [fake:approval]")
        .await;

    let attempt = f.start(&task).await;
    assert!(attempt.running);
    assert_eq!(attempt.worktree_state, WorktreeState::Present);
    assert!(Path::new(&attempt.worktree_path).join(".git").exists());
    assert!(attempt.branch.starts_with("atm/"), "{}", attempt.branch);
    assert_eq!(f.detail(&task.id).await.task.status, TaskStatus::InProgress);

    f.subscribe(&attempt.id).await;
    f.seen
        .until("the snapshot", TURN, async || !f.seen.msgs().is_empty())
        .await;
    assert!(matches!(f.seen.msgs()[0], TranscriptMsg::Snapshot { .. }));
    let user = f
        .entry("the user message", |e| {
            matches!(e.body, EntryBody::UserMessage { .. })
        })
        .await;
    assert_eq!(
        user.body,
        EntryBody::UserMessage {
            text: "# Crea hello\n\nScrivi hello.txt [fake:approval]".into()
        }
    );

    let asking = f
        .entry("the approval request", |e| {
            matches!(
                &e.body,
                EntryBody::ToolCall {
                    status: ToolStatus::AwaitingApproval { .. },
                    ..
                }
            )
        })
        .await;
    let EntryBody::ToolCall {
        name,
        status:
            ToolStatus::AwaitingApproval {
                approval_id,
                can_remember,
                ..
            },
        ..
    } = asking.body
    else {
        unreachable!()
    };
    assert_eq!(name, "Bash");
    assert!(can_remember);
    let card = f.card(&task.id).await;
    assert!(card.running);
    assert_eq!(card.pending_approvals, 1);
    assert_eq!(
        f.detail(&task.id).await.attempt.unwrap().pending_approvals,
        1
    );

    let respond = RespondApprovalReq {
        attempt_id: attempt.id.clone(),
        approval_id: approval_id.clone(),
        decision: ApprovalDecision::Allow { remember: true },
    };
    f.core.respond_approval(respond.clone()).await.unwrap();
    assert_eq!(
        f.db().attempt(&attempt.id).unwrap().allow_rules,
        ["Bash(echo hello)"]
    );
    let again = f.core.respond_approval(respond).await.unwrap_err();
    assert_eq!(again.code, ErrorCode::NotFound);

    let d = f.turn_end(&task.id, 1, TURN).await;
    let turn = &d.processes[0];
    assert_eq!(state(turn), (ProcessStatus::Completed, None));
    assert_eq!(turn.result_subtype.as_deref(), Some("success"));
    let head = turn.head_after.clone().unwrap();
    assert_ne!(head, attempt.base_commit);
    assert_eq!(common::git(&f.repo, &["rev-parse", &attempt.branch]), head);
    assert_eq!(d.task.status, TaskStatus::InReview);
    let a = d.attempt.unwrap();
    assert!(a.session_started && !a.running && a.pending_approvals == 0);
    assert_eq!(f.card(&task.id).await.pending_approvals, 0);
    let entries = f.entries(&attempt.id).await;
    insta::assert_debug_snapshot!("approval_turn_transcript", outline(&entries));

    let diff = f
        .core
        .get_diff(AttemptIdReq {
            attempt_id: attempt.id.clone(),
        })
        .await
        .unwrap();
    assert!(
        diff.files
            .iter()
            .any(|file| file.path == "hello.txt" && file.status == FileStatus::Added),
        "{diff:?}"
    );

    let second = f
        .follow_up(&attempt.id, "Ricontrolla [fake:simple]", false)
        .await;
    assert_eq!(second.seq, 2);
    let d = f.turn_end(&task.id, 2, TURN).await;
    assert_eq!(state(&d.processes[1]), (ProcessStatus::Completed, None));
    let calls = f.calls();
    assert_eq!(calls.len(), 2);
    let session = flag(&calls[0], "--session-id=").unwrap();
    assert_eq!(session, f.db().attempt(&attempt.id).unwrap().session_id);
    assert_eq!(flag(&calls[0], "--resume="), None);
    assert_eq!(flag(&calls[1], "--resume="), Some(session));
    assert_eq!(flag(&calls[1], "--session-id="), None);
    let allow = |argv: &[String]| {
        let settings: Value = serde_json::from_str(flag(argv, "--settings=").unwrap()).unwrap();
        settings["permissions"]["allow"].clone()
    };
    assert_eq!(allow(&calls[0]), serde_json::json!([]));
    assert_eq!(allow(&calls[1]), serde_json::json!(["Bash(echo hello)"]));
    for argv in &calls {
        assert!(argv.contains(&"--permission-mode=acceptEdits".to_owned()));
        assert!(argv.contains(&"--strict-mcp-config".to_owned()));
    }

    let message = merge_message(&task.title, &task.description, &attempt.id);
    let outcome = f
        .core
        .merge_attempt(MergeAttemptReq {
            attempt_id: attempt.id.clone(),
            message,
        })
        .await
        .unwrap();
    let MergeOutcome::Merged {
        commit,
        strategy,
        cleanup_warning,
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(strategy, MergeStrategy::FfCheckedOut);
    assert_eq!(cleanup_warning, None);
    assert_eq!(common::git(&f.repo, &["rev-parse", "main"]), commit);
    assert_eq!(
        std::fs::read_to_string(f.repo.join("hello.txt")).unwrap(),
        "hello\n"
    );
    let d = f.detail(&task.id).await;
    assert_eq!(d.task.status, TaskStatus::Done);
    assert_eq!(d.attempt, None);
    assert_eq!(d.closed_attempts[0].state, AttemptState::Merged);
    assert_eq!(d.closed_attempts[0].worktree_state, WorktreeState::Removed);
    assert!(!Path::new(&attempt.worktree_path).exists());
    assert_eq!(common::git(&f.repo, &["rev-parse", &attempt.branch]), head);

    let running = || -> Vec<u32> {
        let events = f.seen.events();
        events
            .iter()
            .filter_map(|e| match e {
                AppEvent::EnvChanged(env) => Some(env.running),
                AppEvent::Changed(_) => None,
            })
            .collect()
    };
    f.seen
        .until("four env_changed", TURN, async || running().len() >= 4)
        .await;
    assert_eq!(running(), [1, 0, 1, 0]);
    hostile.assert_untouched();
}

/// A turn that dies before `system/init` never started its session: the next turn gets a
/// new `--session-id`. A CLI that exits before answering `initialize` fails at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noinit_gets_a_new_session_id_on_the_next_turn() {
    let f = Flow::new(&[]).await;
    let task = f.task("Senza init", "[fake:noinit]").await;
    let attempt = f.start(&task).await;
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(
        state(&d.processes[0]),
        (ProcessStatus::Failed, Some(StopReason::Crash))
    );
    assert!(!d.attempt.unwrap().session_started);

    f.follow_up(&attempt.id, "Riprova [fake:simple]", false)
        .await;
    let d = f.turn_end(&task.id, 2, TURN).await;
    assert_eq!(state(&d.processes[1]), (ProcessStatus::Completed, None));
    assert!(d.attempt.unwrap().session_started);
    let calls = f.calls();
    let first = flag(&calls[0], "--session-id=").unwrap();
    let second = flag(&calls[1], "--session-id=").unwrap();
    assert_ne!(first, second);
    assert_eq!(flag(&calls[1], "--resume="), None);
    assert_eq!(second, f.db().attempt(&attempt.id).unwrap().session_id);

    let g = Flow::new(&[("FAKE_CLAUDE_SCENARIO", "noinit")]).await;
    let task = g.task("Muore subito", "").await;
    let started = Instant::now();
    g.start(&task).await;
    let d = g.turn_end(&task.id, 1, TURN).await;
    assert_eq!(
        state(&d.processes[0]),
        (ProcessStatus::Failed, Some(StopReason::Crash))
    );
    assert!(started.elapsed() < runner::INIT_TIMEOUT / 2);
}

/// Stop during `hang`: the interrupt is honoured, the turn is killed in ≤ 5 s. While it runs
/// the task cannot be closed or deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hang_stops_on_interrupt_within_5_s() {
    let f = Flow::new(&[]).await;
    let task = f.task("Appeso", "[fake:hang]").await;
    let attempt = f.start(&task).await;
    f.subscribe(&attempt.id).await;
    f.entry("system/init", is_session_init).await;

    let to_done = MoveTaskReq {
        id: task.id.clone(),
        status: TaskStatus::Done,
        before_id: None,
    };
    assert_eq!(
        f.core.move_task(to_done).await.unwrap_err().code,
        ErrorCode::Busy
    );
    let delete = IdReq {
        id: task.id.clone(),
    };
    assert_eq!(
        f.core.delete_task(delete).await.unwrap_err().code,
        ErrorCode::Busy
    );
    let follow = SendFollowUpReq {
        attempt_id: attempt.id.clone(),
        prompt: "altro".into(),
        permission_mode: None,
        fresh_session: false,
    };
    assert_eq!(
        f.core.send_follow_up(follow).await.unwrap_err().code,
        ErrorCode::Busy
    );

    let stopped = Instant::now();
    f.stop(&attempt.id).await;
    assert!(
        stopped.elapsed() < Duration::from_secs(1),
        "stop_attempt must return at once"
    );
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert!(
        stopped.elapsed() <= Duration::from_secs(5),
        "{:?}",
        stopped.elapsed()
    );
    let turn = &d.processes[0];
    assert_eq!(
        state(turn),
        (ProcessStatus::Killed, Some(StopReason::UserStop))
    );
    assert_eq!(
        turn.result_subtype.as_deref(),
        Some("error_during_execution")
    );
    assert_eq!(d.task.status, TaskStatus::InReview);
}

/// `hang_ignore` ignores interrupt, EOF and SIGTERM: SIGKILL of the group within 13 s, the
/// group and the grandchild `sleep` are gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hang_ignore_is_killed_with_its_group_within_13_s() {
    let f = Flow::new(&[]).await;
    let task = f.task("Sordo", "[fake:hang_ignore]").await;
    let attempt = f.start(&task).await;
    f.subscribe(&attempt.id).await;
    f.entry("system/init", is_session_init).await;
    let pgid = f.db().attempt_processes(&attempt.id).unwrap()[0]
        .pid
        .unwrap();
    let grandchild = f
        .record()
        .iter()
        .find_map(|r| (r["kind"] == "grandchild").then(|| r["pid"].as_i64()))
        .flatten()
        .unwrap() as i32;
    assert!(claude::group_alive(pgid) && claude::pid_alive(grandchild));

    let stopped = Instant::now();
    f.stop(&attempt.id).await;
    let d = f.turn_end(&task.id, 1, Duration::from_secs(20)).await;
    let elapsed = stopped.elapsed();
    assert!(elapsed <= Duration::from_secs(13), "{elapsed:?}");
    assert!(
        elapsed >= Duration::from_secs(10),
        "the ladder was skipped: {elapsed:?}"
    );
    assert_eq!(
        state(&d.processes[0]),
        (ProcessStatus::Killed, Some(StopReason::UserStop))
    );
    eventually("the group to vanish", Duration::from_secs(5), || {
        !claude::group_alive(pgid) && !claude::pid_alive(grandchild)
    })
    .await;
}

/// A job the agent leaves running in a process group of its own (a `run_in_background` command
/// of the real Bash tool, M5) ends with the turn, although the leader exits by itself at EOF and
/// no signal ever reaches it through the agent's group (spec §7.4 step 4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_job_in_its_own_group_ends_with_the_turn() {
    let f = Flow::new(&[]).await;
    let task = f.task("Sfondo", "[fake:background]").await;
    let attempt = f.start(&task).await;
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(state(&d.processes[0]), (ProcessStatus::Completed, None));
    let row = &f.db().attempt_processes(&attempt.id).unwrap()[0];
    assert_eq!(row.exit_code, Some(0));
    let job = f
        .record()
        .iter()
        .find_map(|r| (r["kind"] == "background").then(|| r["pid"].as_i64()))
        .flatten()
        .unwrap() as i32;
    let deadline = Instant::now() + Duration::from_secs(5);
    while claude::pid_alive(job) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if claude::pid_alive(job) {
        claude::kill_all(&[job], libc::SIGKILL);
        panic!("the background job {job} outlived the turn");
    }
}

/// The process-table helpers behind it: the tree of a leader, and which of its recorded
/// descendants are still the same processes (reparented to launchd, same group).
#[test]
fn survivors_of_a_recorded_tree() {
    let table = claude::parse_process_table(
        "  1     0     1\n 100     1   100\n 101   100   100\n 102   101   102\n 103   102   102\n\
         200     1   200\n bad row\n",
    );
    let tree = claude::tree_of(&table, 100);
    let pids: Vec<i32> = tree.iter().map(|p| p.pid).collect();
    assert_eq!(pids, [101, 102, 103]);
    assert!(claude::tree_of(&table, 1).is_empty());
    // The leader and 101 exited: 102 moved to launchd, 103 is still under 102, and 101's pid
    // now names a stranger in another group.
    let now = claude::parse_process_table(
        "  1     0     1\n 101     1   300\n 102     1   102\n 103   102   102\n 200     1   200\n",
    );
    assert_eq!(claude::survivors(100, &tree, &now), [102, 103]);
    // While the leader lives, its direct children count too.
    assert_eq!(claude::survivors(100, &tree, &table), [101, 102, 103]);
}

/// Exit 1 without `result`: failed / crash, stderr in the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_is_failed_with_its_stderr() {
    let f = Flow::new(&[]).await;
    let task = f.task("Crash", "[fake:crash]").await;
    let attempt = f.start(&task).await;
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(
        state(&d.processes[0]),
        (ProcessStatus::Failed, Some(StopReason::Crash))
    );
    let row = &f.db().attempt_processes(&attempt.id).unwrap()[0];
    assert_eq!(row.exit_code, Some(1));
    let entries = f.entries(&attempt.id).await;
    assert!(entries.iter().any(|e| matches!(
        &e.body,
        EntryBody::Stderr { text } if text.contains("crash simulato")
    )));
    assert!(
        notices(&entries)
            .iter()
            .any(|(level, text, _)| *level == Level::Error && text.contains("senza risultato"))
    );
    assert_eq!(d.task.status, TaskStatus::InReview);
}

/// An unsupported control request is answered with an error (recorded by the fake) plus a
/// warning Notice; the turn goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_control_request_gets_an_error_reply() {
    let f = Flow::new(&[]).await;
    let task = f.task("Hook", "[fake:control]").await;
    let attempt = f.start(&task).await;
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(state(&d.processes[0]), (ProcessStatus::Completed, None));
    let reply = f
        .record()
        .into_iter()
        .find(|r| r["kind"] == "control_response")
        .unwrap();
    assert_eq!(reply["response"]["subtype"], "error");
    assert_eq!(
        reply["response"]["error"],
        "Unsupported control request subtype: hook_callback"
    );
    let entries = f.entries(&attempt.id).await;
    assert!(
        notices(&entries)
            .iter()
            .any(|(level, text, _)| *level == Level::Warn && text.contains("hook_callback"))
    );
}

/// A usage-limit `result` pauses new turns (`UsageLimited`) until `resume_agents`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_pauses_new_turns_until_resumed() {
    let f = Flow::new(&[]).await;
    let task = f.task("Limite", "[fake:usage_limit]").await;
    let attempt = f.start(&task).await;
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(
        state(&d.processes[0]),
        (ProcessStatus::Failed, Some(StopReason::UsageLimit))
    );
    let env = f.core.get_env(GetEnvReq { force: false }).await.unwrap();
    assert!(
        env.paused.as_deref().unwrap().contains("usage limit"),
        "{env:?}"
    );
    f.seen
        .until("env_changed with the pause", TURN, async || {
            f.seen
                .events()
                .iter()
                .any(|e| matches!(e, AppEvent::EnvChanged(env) if env.paused.is_some()))
        })
        .await;

    let other = f.task("Altro", "[fake:simple]").await;
    let err = f.try_start(&other).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::UsageLimited);
    assert_eq!(f.worktrees(), 1);
    let follow = SendFollowUpReq {
        attempt_id: attempt.id.clone(),
        prompt: "ancora".into(),
        permission_mode: None,
        fresh_session: false,
    };
    let err = f.core.send_follow_up(follow).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::UsageLimited);
    assert_eq!(f.calls().len(), 1);

    let env = f.core.resume_agents().await.unwrap();
    assert_eq!(env.paused, None);
    f.start(&other).await;
    let d = f.turn_end(&other.id, 1, TURN).await;
    assert_eq!(state(&d.processes[0]), (ProcessStatus::Completed, None));
}

/// A login failure in `result`: failed / auth_failure, and `auth status` runs again for the
/// `env_changed` after the turn, while a normal turn reuses the cached status (60 s). The CLI
/// is a wrapper around fake-claude that counts the `auth status` calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_failure_invalidates_the_env() {
    let dir = common::tempdir();
    let calls = dir.path().join("auth-status.calls");
    let claude = dir.path().join("bin/claude");
    common::script(
        &claude,
        &format!(
            "[ \"$1 $2\" = 'auth status' ] && echo >> '{}'\nexec '{}' \"$@\"",
            calls.display(),
            common::fake_claude().display()
        ),
    );
    let f = Flow::setup(dir, hermetic(&[]), claude).await;
    let auth_checks = || std::fs::read_to_string(&calls).map_or(0, |s| s.lines().count());
    let checked_at_startup = auth_checks();
    assert!(checked_at_startup >= 1);
    let envs = || -> Vec<EnvStatus> {
        let events = f.seen.events();
        events
            .into_iter()
            .filter_map(|e| match e {
                AppEvent::EnvChanged(env) => Some(env),
                AppEvent::Changed(_) => None,
            })
            .collect()
    };

    let task = f.task("Login", "[fake:auth_fail]").await;
    let attempt = f.start(&task).await;
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(
        state(&d.processes[0]),
        (ProcessStatus::Failed, Some(StopReason::AuthFailure))
    );
    let entries = f.entries(&attempt.id).await;
    assert!(entries.iter().any(|e| matches!(
        e.body,
        EntryBody::TurnEnd {
            limit: Some(LimitKind::AuthFailure),
            ..
        }
    )));
    f.seen
        .until("env_changed after the turn", TURN, async || {
            envs().iter().map(|env| env.running).eq([1, 0])
        })
        .await;
    assert_eq!(auth_checks(), checked_at_startup + 1);
    let env = envs().pop().unwrap();
    assert!(matches!(env.auth, AuthState::LoggedIn { .. }));
    assert_eq!(env.paused, None);

    f.follow_up(&attempt.id, "Riprova [fake:simple]", false)
        .await;
    f.turn_end(&task.id, 2, TURN).await;
    f.seen
        .until("env_changed after the second turn", TURN, async || {
            envs().iter().map(|env| env.running).eq([1, 0, 1, 0])
        })
        .await;
    assert_eq!(auth_checks(), checked_at_startup + 1);
}

/// `max_running = 2`: the third start is refused before any worktree; shutdown stops both
/// turns within its deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn third_start_hits_the_concurrency_cap() {
    let f = Flow::new(&[]).await;
    let mut tasks = Vec::new();
    for n in 1..=3 {
        tasks.push(f.task(&format!("Appeso {n}"), "[fake:hang]").await);
    }
    f.start(&tasks[0]).await;
    f.start(&tasks[1]).await;
    let err = f.try_start(&tasks[2]).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ConcurrencyLimit);
    assert_eq!(f.worktrees(), 2);
    assert_eq!(f.detail(&tasks[2].id).await.attempt, None);
    let env = f.core.get_env(GetEnvReq { force: false }).await.unwrap();
    assert_eq!((env.running, env.max_running), (2, 2));

    let started = Instant::now();
    f.core.shutdown(runner::SHUTDOWN_DEADLINE).await;
    assert!(started.elapsed() <= runner::SHUTDOWN_DEADLINE);
    for task in &tasks[..2] {
        let d = f.detail(&task.id).await;
        assert_eq!(
            state(&d.processes[0]),
            (ProcessStatus::Killed, Some(StopReason::AppShutdown))
        );
        assert!(!d.attempt.unwrap().running);
    }
    let env = f.core.get_env(GetEnvReq { force: false }).await.unwrap();
    assert_eq!(env.running, 0);
}

/// `auth status` exit 1: `NotLoggedIn` before any worktree or spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logged_out_refuses_to_start_without_spawning() {
    let f = Flow::new(&[("FAKE_CLAUDE_AUTH", "out")]).await;
    let env = f.core.get_env(GetEnvReq { force: true }).await.unwrap();
    assert_eq!(env.auth, AuthState::LoggedOut);
    assert!(env.claude.path.is_some());
    let task = f.task("Mai partito", "[fake:simple]").await;
    let err = f.try_start(&task).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotLoggedIn);
    assert!(f.calls().is_empty());
    assert_eq!(f.worktrees(), 0);
    let d = f.detail(&task.id).await;
    assert_eq!((d.task.status, d.attempt), (TaskStatus::Todo, None));
}

/// Bypass needs the project's `allow_bypass` on every path, and a revoked one also stops the
/// attempt's next turns. The agent's environment lacks the credentials, nesting and git
/// variables of the app's own, and its `PWD` is the worktree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bypass_needs_the_project_setting_and_the_env_is_scrubbed() {
    let f = Flow::new(&[
        ("ANTHROPIC_API_KEY", "sk-ant-test"),
        ("CLAUDECODE", "1"),
        ("GIT_DIR", "/nonexistent"),
    ])
    .await;
    let bypass = PermissionMode::BypassPermissions;
    let task = f.task("Autonomo", "[fake:simple]").await;
    let err = f.try_start_as(&task, bypass).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert_eq!(f.worktrees(), 0);
    let update = UpdateProjectReq {
        id: f.project.id.clone(),
        name: f.project.name.clone(),
        default_target_branch: f.project.default_target_branch.clone(),
        default_permission_mode: bypass,
        default_model: None,
    };
    let err = f.core.update_project(update).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);

    let security = |allow_bypass| SetProjectSecurityReq {
        id: f.project.id.clone(),
        config_policy: ConfigPolicy::Isolated,
        allow_bypass,
    };
    f.core.set_project_security(security(true)).await.unwrap();
    let attempt = f.try_start_as(&task, bypass).await.unwrap();
    let d = f.turn_end(&task.id, 1, TURN).await;
    assert_eq!(state(&d.processes[0]), (ProcessStatus::Completed, None));
    let call = f
        .record()
        .into_iter()
        .find(|r| r["kind"] == "call")
        .unwrap();
    let argv: Vec<String> = serde_json::from_value(call["argv"].clone()).unwrap();
    assert!(
        argv.contains(&"--permission-mode=bypassPermissions".to_owned()),
        "{argv:?}"
    );
    for var in ["ANTHROPIC_API_KEY", "CLAUDECODE", "GIT_DIR"] {
        assert_eq!(call["env"][var], false, "{var} reached the agent");
    }
    assert_eq!(call["pwd"], attempt.worktree_path.as_str());
    let env = f.core.get_env(GetEnvReq { force: false }).await.unwrap();
    assert!(env.api_key_in_env);

    f.core.set_project_security(security(false)).await.unwrap();
    for permission_mode in [None, Some(bypass)] {
        let req = SendFollowUpReq {
            attempt_id: attempt.id.clone(),
            prompt: "Ancora [fake:simple]".into(),
            permission_mode,
            fresh_session: false,
        };
        let err = f.core.send_follow_up(req).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid, "{permission_mode:?}");
    }
    assert_eq!(f.calls().len(), 1);
}

/// A failed `--resume` offers "Nuova sessione" once; `fresh_session` starts a new session
/// with the task and the attempt's commits in the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_resume_offers_a_fresh_session() {
    let f = Flow::new(&[]).await;
    let task = f.task("Riprendi", "[fake:simple]").await;
    let attempt = f.start(&task).await;
    f.turn_end(&task.id, 1, TURN).await;
    f.follow_up(&attempt.id, "[fake:resume_fail]", false).await;
    let d = f.turn_end(&task.id, 2, TURN).await;
    assert_eq!(
        state(&d.processes[1]),
        (ProcessStatus::Failed, Some(StopReason::Crash))
    );
    let offers = notices(&f.entries(&attempt.id).await)
        .into_iter()
        .filter(|(_, _, action)| *action == Some(NoticeAction::NewSession))
        .count();
    assert_eq!(offers, 1);

    f.follow_up(&attempt.id, "Da capo [fake:simple]", true)
        .await;
    let d = f.turn_end(&task.id, 3, TURN).await;
    assert_eq!(state(&d.processes[2]), (ProcessStatus::Completed, None));
    let calls = f.calls();
    let old = flag(&calls[0], "--session-id=").unwrap();
    assert_eq!(flag(&calls[1], "--resume="), Some(old));
    let fresh = flag(&calls[2], "--session-id=").unwrap();
    assert_ne!(fresh, old);
    let prompt = f
        .entries(&attempt.id)
        .await
        .into_iter()
        .rev()
        .find_map(|e| match e.body {
            EntryBody::UserMessage { text } => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(
        prompt.starts_with("# Riprendi\n\n[fake:simple]"),
        "{prompt}"
    );
    assert!(prompt.contains("atm: turn 1"), "{prompt}");
    assert!(prompt.ends_with("Da capo [fake:simple]"), "{prompt}");
}

/// The runtime is dropped mid-turn: the turn's future kills the agent's whole group, the
/// grandchild `sleep` included (the leader alone would get `kill_on_drop`). The next Core
/// marks the turn failed/app_restart and moves the task to inreview.
#[test]
fn runtime_dropped_mid_turn_kills_the_group() {
    let first = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (f, task, pgid, grandchild) = first.block_on(async {
        let f = Flow::new(&[]).await;
        let task = f.task("Interrotto", "[fake:hang_ignore]").await;
        let attempt = f.start(&task).await;
        f.subscribe(&attempt.id).await;
        f.entry("system/init", is_session_init).await;
        let pgid = f.db().attempt_processes(&attempt.id).unwrap()[0]
            .pid
            .unwrap();
        let grandchild = f
            .record()
            .iter()
            .find_map(|r| (r["kind"] == "grandchild").then(|| r["pid"].as_i64()))
            .flatten()
            .unwrap() as i32;
        (f, task, pgid, grandchild)
    });
    assert!(claude::group_alive(pgid) && claude::pid_alive(grandchild));
    drop(first);
    reap(pgid);
    eventually_blocking("the group to vanish", Duration::from_secs(5), || {
        !claude::group_alive(pgid) && !claude::pid_alive(grandchild)
    });

    let second = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    second.block_on(async {
        let core = Core::new(f.config.clone(), Seen::new().notify()).unwrap();
        core.startup().await.unwrap();
        let d = core
            .get_task_detail(IdReq {
                id: task.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            state(&d.processes[0]),
            (ProcessStatus::Failed, Some(StopReason::AppRestart))
        );
        assert_eq!(d.task.status, TaskStatus::InReview);
    });
}

/// The app vanishes mid-turn without running any destructor (its runtime is leaked, never
/// polled again: its agent keeps running). The next Core marks the turn failed/app_restart,
/// kills the group after verifying it with `ps`, moves the task to inreview, and "Continua"
/// resumes the session.
#[test]
fn app_vanished_mid_turn_is_recovered_by_the_next_core() {
    let first = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (f, task, attempt, grandchild) = first.block_on(async {
        let f = Flow::new(&[]).await;
        let task = f.task("Interrotto", "[fake:hang_ignore]").await;
        let attempt = f.start(&task).await;
        f.subscribe(&attempt.id).await;
        f.entry("system/init", is_session_init).await;
        let grandchild = f
            .record()
            .iter()
            .find_map(|r| (r["kind"] == "grandchild").then(|| r["pid"].as_i64()))
            .flatten()
            .unwrap() as i32;
        (f, task, attempt, grandchild)
    });
    let Flow {
        dir, config, core, ..
    } = f;
    drop(core);
    std::mem::forget(first);

    let second = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    second.block_on(async {
        let db = Db::open(&config.data_dir.join("atm.sqlite3")).unwrap();
        let pgid = db.attempt_processes(&attempt.id).unwrap()[0].pid.unwrap();
        assert!(claude::pid_alive(pgid), "the orphan must still run");

        let seen = Seen::new();
        let core = Core::new(config.clone(), seen.notify()).unwrap();
        core.startup().await.unwrap();

        let turn = &db.attempt_processes(&attempt.id).unwrap()[0];
        assert_eq!(
            (turn.status, turn.stop_reason),
            (ProcessStatus::Failed, Some(StopReason::AppRestart))
        );
        // The leaked runtime never reaps its child: do it here, then the whole group is gone.
        let reaped = tokio::task::spawn_blocking(move || reap(pgid))
            .await
            .unwrap();
        assert_eq!(reaped, pgid);
        // The grandchild leads its own group, like the real Bash tool's commands (M5): the
        // recovery kills the orphan's descendants too.
        eventually("the orphan group to vanish", Duration::from_secs(5), || {
            !claude::group_alive(pgid) && !claude::pid_alive(grandchild)
        })
        .await;

        let detail = core
            .get_task_detail(IdReq {
                id: task.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(detail.task.status, TaskStatus::InReview);
        let a = detail.attempt.unwrap();
        assert!(!a.running && a.session_started);
        let board = core
            .get_board(ProjectIdReq {
                project_id: task.project_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(board[0].last_stop_reason, Some(StopReason::AppRestart));
        let entries = db.entries_tail(&attempt.id, 200).unwrap().entries;
        assert!(
            notices(&entries)
                .iter()
                .any(|(_, text, _)| text.contains("riavvio dell'app"))
        );

        let req = SendFollowUpReq {
            attempt_id: attempt.id.clone(),
            prompt: CONTINUE_PROMPT.into(),
            permission_mode: None,
            fresh_session: false,
        };
        core.send_follow_up(req).await.unwrap();
        seen.until("the continued turn", TURN, async || {
            let d = core
                .get_task_detail(IdReq {
                    id: task.id.clone(),
                })
                .await
                .unwrap();
            d.processes
                .get(1)
                .is_some_and(|p| p.status != ProcessStatus::Running)
                && !d.attempt.is_some_and(|a| a.running)
        })
        .await;
        let record = std::fs::read_to_string(dir.path().join("record.jsonl")).unwrap();
        let calls: Vec<Vec<String>> = record
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|r| r["kind"] == "call")
            .map(|r| serde_json::from_value(r["argv"].clone()).unwrap())
            .collect();
        let session = flag(&calls[0], "--session-id=").unwrap();
        assert_eq!(flag(&calls[1], "--resume="), Some(session));
    });
}

/// Subscriptions taken while entries are written and updated concurrently: each view,
/// rebuilt from its Snapshot and Upserts, ends equal to the DB from its snapshot on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_then_upserts_never_lose_a_concurrent_write() {
    const N: u32 = 3000;
    const VIEWS: u32 = 6;
    let (db, attempt) = seeded_db();
    let live = Arc::new(Live::new());
    let (progress, written) = watch::channel(0u32);
    let producer = tokio::spawn({
        let (db, live, attempt) = (Arc::clone(&db), Arc::clone(&live), attempt.clone());
        async move {
            for i in 1..=N {
                let mut batch = vec![entry(i, 0)];
                if i % 3 == 0 {
                    batch.push(entry(i / 2, i));
                }
                db.upsert_entries(&attempt, &batch).unwrap();
                for e in batch {
                    live.send(&attempt, LiveMsg::Upsert(Arc::new(e)));
                }
                progress.send_replace(i);
                if i % 8 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }
    });
    let mut views = Vec::new();
    for k in 0..VIEWS {
        written
            .clone()
            .wait_for(|&i| i >= k * N / VIEWS)
            .await
            .unwrap();
        let seen = Seen::new();
        live.subscribe(Arc::clone(&db), &attempt, seen.sink())
            .unwrap();
        views.push(seen);
    }
    producer.await.unwrap();
    let last = entry(N + 1, 0);
    db.upsert_entries(&attempt, std::slice::from_ref(&last))
        .unwrap();
    live.send(&attempt, LiveMsg::Upsert(Arc::new(last)));

    let mut truth = BTreeMap::new();
    let mut before = u32::MAX;
    loop {
        let page = db.entries_before(&attempt, before, 200).unwrap();
        before = page.entries.first().map_or(0, |e| e.idx);
        truth.extend(page.entries.into_iter().map(|e| (e.idx, e.rev)));
        if !page.has_more {
            break;
        }
    }
    for seen in &views {
        seen.until("the last entry", TURN, async || {
            seen.store().contains_key(&(N + 1))
        })
        .await;
        let (store, floor) = seen.view();
        for (idx, rev) in truth.range(floor..) {
            assert_eq!(
                store.get(idx).map(|e| e.rev),
                Some(*rev),
                "idx {idx} from {floor}"
            );
        }
    }
    assert_eq!(live.forwarder_count(), VIEWS as usize);
    live.drop_all();
    assert_eq!(live.forwarder_count(), 0);
}

/// Over [`BROADCAST_CAPACITY`] messages before the forwarder runs: `Lagged` → a fresh
/// Snapshot of the DB tail, then the retained upserts.
#[tokio::test]
async fn a_lagging_view_gets_a_fresh_snapshot() {
    let (db, attempt) = seeded_db();
    let live = Arc::new(Live::new());
    let seen = Seen::new();
    // Current-thread runtime: the forwarder cannot run before this test awaits.
    live.subscribe(Arc::clone(&db), &attempt, seen.sink())
        .unwrap();
    let n = BROADCAST_CAPACITY as u32 + 500;
    for i in 0..n {
        db.upsert_entries(&attempt, &[entry(i, 0)]).unwrap();
        live.send(&attempt, LiveMsg::Upsert(Arc::new(entry(i, 0))));
    }
    seen.until("the last entry", TURN, async || {
        seen.store().contains_key(&(n - 1))
    })
    .await;
    let msgs = seen.msgs();
    let snapshots: Vec<(&Vec<Entry>, bool)> = msgs
        .iter()
        .filter_map(|m| match m {
            TranscriptMsg::Snapshot {
                entries, has_more, ..
            } => Some((entries, *has_more)),
            _ => None,
        })
        .collect();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots[0].0.is_empty() && !snapshots[0].1);
    assert!(matches!(msgs[1], TranscriptMsg::Snapshot { .. }));
    let fresh: Vec<u32> = snapshots[1].0.iter().map(|e| e.idx).collect();
    assert_eq!(fresh, (n - SNAPSHOT_TAIL..n).collect::<Vec<_>>());
    assert!(snapshots[1].1);
    assert!(
        msgs[2..]
            .iter()
            .all(|m| matches!(m, TranscriptMsg::Upsert { entries } if entries.len() <= 200))
    );
}

/// An unknown attempt gets an empty Snapshot; a page reload drops every forwarder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_attempt_snapshot_and_reload() {
    let f = Flow::new(&[]).await;
    f.subscribe("no-such-attempt").await;
    f.subscribe("no-such-attempt").await;
    f.seen
        .until("two snapshots", TURN, async || f.seen.msgs().len() == 2)
        .await;
    for msg in f.seen.msgs() {
        assert_eq!(
            msg,
            TranscriptMsg::Snapshot {
                entries: Vec::new(),
                has_more: false,
                typing: None
            }
        );
    }
    assert_eq!(f.core.forwarder_count(), 2);
    f.core.drop_subscriptions();
    assert_eq!(f.core.forwarder_count(), 0);
    let too_many = GetEntriesReq {
        attempt_id: "x".into(),
        before_idx: 0,
        limit: 201,
    };
    assert_eq!(
        f.core.get_entries(too_many).await.unwrap_err().code,
        ErrorCode::Invalid
    );
    let url = OpenUrlReq {
        url: "file:///etc/passwd".into(),
    };
    assert_eq!(
        f.core.open_url(url).await.unwrap_err().code,
        ErrorCode::Invalid
    );
}

/// With `open_log` (debug builds, the M4 E2E) the login script is still written but `open` is
/// only recorded: no Terminal, Finder or browser is launched. Release builds ignore `open_log`,
/// so there the test would really open Terminal and a browser: it only runs in debug.
#[tokio::test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "open_log is honoured only in debug builds"
)]
async fn open_calls_are_recorded_with_open_log() {
    let dir = common::tempdir();
    let log = dir.path().join("open.jsonl");
    let config = CoreConfig {
        data_dir: dir.path().join("data"),
        cache_dir: dir.path().join("cache"),
        claude_path: Some(common::fake_claude()),
        path_env: Some(std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())),
        extra_env: hermetic(&[]),
        open_log: Some(log.clone()),
    };
    let core = Core::new(config, Seen::new().notify()).unwrap();
    let req = OpenLoginTerminalReq {
        method: LoginMethod::Sso,
    };
    core.open_login_terminal(req).await.unwrap();
    let script = dir.path().join("cache").join(claude::LOGIN_SCRIPT_NAME);
    assert!(
        std::fs::read_to_string(&script)
            .unwrap()
            .contains(" auth login --sso\n")
    );
    let url = OpenUrlReq {
        url: "https://example.com/docs".into(),
    };
    core.open_url(url).await.unwrap();
    let calls: Vec<Vec<String>> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        calls,
        [
            vec![
                "-a".to_owned(),
                "Terminal".into(),
                script.display().to_string()
            ],
            vec!["https://example.com/docs".into()],
        ]
    );
}

/// The classification table of spec §7.7.
#[test]
fn classification_table() {
    let result = |is_error, limit| TurnResult {
        subtype: "success".into(),
        is_error,
        duration_ms: None,
        num_turns: None,
        cost_usd_estimate: None,
        permission_denials: 0,
        text: None,
        limit,
    };
    let outcome = |result, exit_code| TurnOutcome {
        result,
        exit_code,
        ..TurnOutcome::default()
    };
    use ProcessStatus::*;
    let cases = [
        (
            outcome(Some(result(false, None)), Some(0)),
            (Completed, None),
        ),
        (
            outcome(Some(result(true, Some(LimitKind::UsageLimit))), Some(0)),
            (Failed, Some(StopReason::UsageLimit)),
        ),
        (
            outcome(Some(result(true, Some(LimitKind::AuthFailure))), Some(0)),
            (Failed, Some(StopReason::AuthFailure)),
        ),
        (outcome(Some(result(true, None)), Some(0)), (Failed, None)),
        (outcome(None, Some(1)), (Failed, Some(StopReason::Crash))),
        (outcome(None, None), (Failed, Some(StopReason::Crash))),
        (
            TurnOutcome {
                stop: Some(StopCause::User),
                ..outcome(None, None)
            },
            (Killed, Some(StopReason::UserStop)),
        ),
        (
            TurnOutcome {
                stop: Some(StopCause::Shutdown),
                ..outcome(Some(result(false, None)), Some(0))
            },
            (Killed, Some(StopReason::AppShutdown)),
        ),
        (
            TurnOutcome {
                init_timeout: true,
                ..outcome(None, None)
            },
            (Failed, Some(StopReason::InitTimeout)),
        ),
        (
            TurnOutcome {
                exit_timeout: true,
                ..outcome(Some(result(false, None)), None)
            },
            (Completed, Some(StopReason::ExitTimeout)),
        ),
        (
            TurnOutcome {
                spawn_error: true,
                ..TurnOutcome::default()
            },
            (Failed, Some(StopReason::SpawnError)),
        ),
    ];
    for (outcome, expected) in cases {
        assert_eq!(runner::classify(&outcome), expected, "{outcome:?}");
    }
}

// ---- live fixtures ------------------------------------------------------------------------

/// An in-memory DB with one attempt (the entries' foreign key).
fn seeded_db() -> (Arc<Db>, String) {
    let db = Db::open_in_memory().unwrap();
    db.insert_project(&ProjectRow {
        id: "p".into(),
        name: "p".into(),
        repo_path: "/nonexistent/p".into(),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: None,
        config_policy: ConfigPolicy::Isolated,
        trusted_fingerprint: None,
        allow_bypass: false,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    let task = CreateTaskReq {
        project_id: "p".into(),
        title: "t".into(),
        description: String::new(),
        status: None,
    };
    db.insert_task("t", &task, 0).unwrap();
    let attempt = AttemptRow {
        id: "a".into(),
        task_id: "t".into(),
        state: AttemptState::Active,
        branch: "atm/a".into(),
        target_branch: "main".into(),
        base_commit: "0".repeat(40),
        worktree_path: "/nonexistent/wt".into(),
        worktree_state: WorktreeState::Present,
        session_id: "s".into(),
        session_started: false,
        permission_mode: PermissionMode::AcceptEdits,
        model: None,
        effort: None,
        allow_rules: Vec::new(),
        merge_commit: None,
        created_at: 0,
        updated_at: 0,
        closed_at: None,
    };
    let process = ProcessRow {
        id: "pr".into(),
        attempt_id: "a".into(),
        seq: 1,
        prompt: "p".into(),
        permission_mode: PermissionMode::AcceptEdits,
        session_id: "s".into(),
        resumed: false,
        status: ProcessStatus::Running,
        stop_reason: None,
        error: None,
        cli_version: None,
        argv_json: "[]".into(),
        pid: None,
        app_instance_id: "i".into(),
        exit_code: None,
        result_subtype: None,
        is_error: None,
        cost_usd_estimate: None,
        num_turns: None,
        duration_ms: None,
        head_before: None,
        head_after: None,
        started_at: 0,
        finished_at: None,
    };
    db.begin_attempt(&attempt, &process, 0).unwrap();
    (Arc::new(db), attempt.id)
}

fn entry(idx: u32, rev: u32) -> Entry {
    Entry {
        idx,
        rev,
        process_id: "pr".into(),
        ts: 0,
        parent_tool_use_id: None,
        body: EntryBody::AssistantText {
            text: format!("{idx}.{rev}"),
        },
    }
}

/// The CLI's answer to `initialize` names the account (M5 capture): the raw log keeps its
/// shape, never the email or the organization (spec §7.10).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_log_never_holds_the_account_email() {
    let f = Flow::new(&[]).await;
    let task = f.task("Log", "[fake:simple]").await;
    let attempt = f.start(&task).await;
    f.turn_end(&task.id, 1, TURN).await;
    let p = &f.db().attempt_processes(&attempt.id).unwrap()[0];
    let dir = runner::log_dir(&f.config.data_dir, &attempt.id, &p.id);
    let log = std::fs::read_to_string(dir.join("stdout.jsonl")).unwrap();
    let answer = log
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["type"] == "control_response")
        .unwrap();
    let redacted = atm_core::wire::REDACTED;
    assert_eq!(
        answer["response"]["response"]["account"],
        serde_json::json!({"email": redacted, "organization": redacted,
                           "subscriptionType": "Claude Max", "apiProvider": "firstParty"})
    );
    assert!(!log.contains("fake@example.com") && !log.contains("Fake Org"));
}
