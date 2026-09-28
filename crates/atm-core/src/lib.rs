//! Application core (spec §3, §4): the services behind every IPC command, the turn runner,
//! the live transcript fan-out, git and SQLite. No Tauri types: the shell adapts [`Notify`]
//! to `app.emit` and [`TranscriptSink`] to a `Channel`.
//!
//! Module owners after M1 (spec §11.2): `db` M2-DB, `git` M2-GIT, `claude`/`wire`/`normalize`
//! M2-CLAUDE, `lib`/`runner`/`live` M3-CORE. Public signatures are frozen; owners only add.

pub mod claude;
pub mod db;
pub mod git;
pub mod live;
pub mod normalize;
pub mod runner;
pub mod wire;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atm_types::{
    AddProjectReq, AddProjectRes, AppError, AttemptIdReq, AttemptView, BranchList, BranchStatus,
    Changed, CreateTaskReq, DiffResult, EVENT_CHANGED, EVENT_ENV_CHANGED, EntryPage, EnvStatus,
    GetEntriesReq, GetEnvReq, Id, IdReq, MergeAttemptReq, MergeOutcome, Millis, MoveTaskReq,
    OpenAttemptReq, OpenLoginTerminalReq, OpenUrlReq, ProcessInfo, Project, ProjectIdReq,
    RespondApprovalReq, SendFollowUpReq, SetProjectSecurityReq, Settings, StartAttemptReq,
    TaskCard, TaskDetail, UnsubscribeTranscriptReq, UpdateProjectReq, UpdateTaskReq,
};

pub use live::TranscriptSink;

/// Global event for the UI (spec §6.4), emitted after the DB commit of every mutation.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // rare, emitted once per mutation
pub enum AppEvent {
    Changed(Changed),
    EnvChanged(EnvStatus),
}

impl AppEvent {
    /// Tauri event name: `changed` or `env_changed`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Changed(_) => EVENT_CHANGED,
            Self::EnvChanged(_) => EVENT_ENV_CHANGED,
        }
    }
}

/// Event callback given to [`Core::new`]; must not block.
pub type Notify = Arc<dyn Fn(AppEvent) + Send + Sync>;

#[derive(Debug, Clone, Default)]
pub struct CoreConfig {
    /// `app_data_dir()`: `atm.sqlite3` and `logs/` (the shell creates it 0700).
    pub data_dir: PathBuf,
    /// `app_cache_dir()`: `claude-login.command`.
    pub cache_dir: PathBuf,
    /// Claude CLI to use instead of discovery (tests: `CARGO_BIN_EXE_fake-claude`).
    pub claude_path: Option<PathBuf>,
    /// `PATH` to use instead of importing it from the login shell (tests).
    pub path_env: Option<OsString>,
    /// Added to the inherited environment before the scrub rules, for every child (claude
    /// through `ChildEnv`, git through `Git::with_env`). Tests: `FAKE_CLAUDE_*` per Core,
    /// without `std::env::set_var`.
    pub extra_env: Vec<(OsString, OsString)>,
}

/// Unix time in milliseconds.
pub fn now_ms() -> Millis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Millis)
}

/// New lowercase UUID v4.
pub fn new_id() -> Id {
    uuid::Uuid::new_v4().to_string()
}

/// The application service. One method per IPC command (spec §6.3), all returning typed
/// [`AppError`]s; `pick_repo_folder` and the native confirmations live in the shell.
pub struct Core {
    #[allow(dead_code)] // M3-CORE
    config: CoreConfig,
    #[allow(dead_code)] // M3-CORE
    notify: Notify,
    #[allow(dead_code)] // M3-CORE
    app_instance_id: Id,
}

// M1 contract stubs: remove this allow when implementing.
#[allow(unused_variables)]
impl Core {
    /// Builds the core with a fresh `app_instance_id`. M3 opens the DB here. Runs inside a
    /// tokio runtime context (the shell's `block_on`, `#[tokio::test]`), so it may spawn.
    /// Errors: `Db`, `Io`, with a message naming the path (the shell shows it and exits).
    pub fn new(config: CoreConfig, notify: Notify) -> Result<Core, AppError> {
        Ok(Core {
            config,
            notify,
            app_instance_id: new_id(),
        })
    }

    /// Runs before the UI loads data (spec §7.9): orphan recovery (`Db::mark_orphans`,
    /// verified kill, cancelled tools, auto-commit), worktree reconciliation, first env check.
    pub async fn startup(&self) -> Result<(), AppError> {
        Err(AppError::not_implemented("Core::startup"))
    }

    /// Stops every running turn in parallel with [`runner::StopTimings::SHUTDOWN`] and
    /// finalizes them (auto-commit included) within `deadline`. Best effort, never fails.
    pub async fn shutdown(&self, deadline: Duration) {
        // M3-CORE. A no-op until then: the shell calls it on every exit.
    }

    /// Stops every transcript forwarder (page reload, spec §6.5).
    pub fn drop_subscriptions(&self) {
        // M3-CORE. A no-op until then: the shell calls it on every page load.
    }

    /// Live transcript forwarders (`Live::forwarder_count`): 0 after a reload (selftest, M4).
    pub fn forwarder_count(&self) -> usize {
        0 // M3-CORE: delegate to `Live`. No subscription can exist until then.
    }

    /// One project, `trusted` computed from this project's fingerprint only (the shell's
    /// confirmation check). Errors: `NotFound`.
    pub async fn project(&self, id: &str) -> Result<Project, AppError> {
        Err(AppError::not_implemented("Core::project"))
    }

    /// Env status. Discovery, versions and `auth status` are cached 60 s (`force` re-reads
    /// PATH, `claude --version`, `auth status`, git); `paused`, `running`, `max_running` and
    /// `checked_at` are filled at every call. Emits `env_changed` when the pause, the running
    /// count or `max_running` change.
    pub async fn get_env(&self, req: GetEnvReq) -> Result<EnvStatus, AppError> {
        Err(AppError::not_implemented("get_env"))
    }

    /// Writes the login script and opens Terminal (spec §7.10). Errors: `ClaudeNotFound`.
    pub async fn open_login_terminal(&self, req: OpenLoginTerminalReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("open_login_terminal"))
    }

    /// Clears the usage-limit pause; emits `env_changed`.
    pub async fn resume_agents(&self) -> Result<EnvStatus, AppError> {
        Err(AppError::not_implemented("resume_agents"))
    }

    pub async fn get_settings(&self) -> Result<Settings, AppError> {
        Err(AppError::not_implemented("get_settings"))
    }

    /// Validates (`max_running` 1..=6, …) and saves. The shell has already obtained the
    /// native confirmation when `allow_env_api_key` is being enabled. Errors: `Invalid`.
    pub async fn update_settings(&self, req: Settings) -> Result<Settings, AppError> {
        Err(AppError::not_implemented("update_settings"))
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, AppError> {
        Err(AppError::not_implemented("list_projects"))
    }

    /// Validates the repo (spec §8.3) and inserts it with its current branch as default
    /// target. Errors: `Invalid`, `Conflict` (already added).
    pub async fn add_project(&self, req: AddProjectReq) -> Result<AddProjectRes, AppError> {
        Err(AppError::not_implemented("add_project"))
    }

    /// Errors: `NotFound`, `Invalid` (target branch not in `refs/heads`).
    pub async fn update_project(&self, req: UpdateProjectReq) -> Result<Project, AppError> {
        Err(AppError::not_implemented("update_project"))
    }

    /// M6: Trusted stores the fingerprint of the main checkout. The shell has already
    /// obtained the native confirmation when raising the effective level (Trusted while not
    /// `Project::trusted`, or enabling bypass).
    pub async fn set_project_security(
        &self,
        req: SetProjectSecurityReq,
    ) -> Result<Project, AppError> {
        Err(AppError::not_implemented("set_project_security"))
    }

    /// Errors: `Busy` with running turns. Snapshots and removes the worktrees, keeps branches.
    pub async fn remove_project(&self, req: IdReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("remove_project"))
    }

    pub async fn list_branches(&self, req: ProjectIdReq) -> Result<BranchList, AppError> {
        Err(AppError::not_implemented("list_branches"))
    }

    /// DB board merged with the live registry (`running`, `pending_approvals`).
    pub async fn get_board(&self, req: ProjectIdReq) -> Result<Vec<TaskCard>, AppError> {
        Err(AppError::not_implemented("get_board"))
    }

    pub async fn create_task(&self, req: CreateTaskReq) -> Result<TaskCard, AppError> {
        Err(AppError::not_implemented("create_task"))
    }

    pub async fn update_task(&self, req: UpdateTaskReq) -> Result<TaskCard, AppError> {
        Err(AppError::not_implemented("update_task"))
    }

    /// Errors: `Busy` if running and the destination is done or cancelled.
    pub async fn move_task(&self, req: MoveTaskReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("move_task"))
    }

    /// Errors: `Busy` if running. Discards the active attempt first.
    pub async fn delete_task(&self, req: IdReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("delete_task"))
    }

    pub async fn get_task_detail(&self, req: IdReq) -> Result<TaskDetail, AppError> {
        Err(AppError::not_implemented("get_task_detail"))
    }

    /// Preflight (spec §7.7 step 2), worktree under the repo lock, rows, then spawns the
    /// first turn in the background. Errors: `ClaudeNotFound`, `NotLoggedIn`, `UsageLimited`,
    /// `ConcurrencyLimit`, `Conflict` (active attempt exists), `Invalid` (bypass without the
    /// project's `allow_bypass`), `Git`.
    pub async fn start_attempt(&self, req: StartAttemptReq) -> Result<AttemptView, AppError> {
        Err(AppError::not_implemented("start_attempt"))
    }

    /// `permission_mode` applies to this turn only (`processes.permission_mode`); the
    /// attempt's mode is unchanged. Errors: `Busy` (turn running), `WorktreeMissing`,
    /// `BranchMismatch`, plus the preflight errors of `start_attempt`.
    pub async fn send_follow_up(&self, req: SendFollowUpReq) -> Result<ProcessInfo, AppError> {
        Err(AppError::not_implemented("send_follow_up"))
    }

    /// Returns at once; the stop sequence runs in the background.
    pub async fn stop_attempt(&self, req: AttemptIdReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("stop_attempt"))
    }

    /// Errors: `NotFound` (approval no longer pending).
    pub async fn respond_approval(&self, req: RespondApprovalReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("respond_approval"))
    }

    /// Registers a forwarder into `sink` and returns its id at once (spec §6.5). An unknown
    /// attempt gets an empty `Snapshot` (`has_more: false`), not an error.
    pub async fn subscribe_transcript(
        &self,
        req: AttemptIdReq,
        sink: TranscriptSink,
    ) -> Result<Id, AppError> {
        Err(AppError::not_implemented("subscribe_transcript"))
    }

    pub async fn unsubscribe_transcript(
        &self,
        req: UnsubscribeTranscriptReq,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("unsubscribe_transcript"))
    }

    /// Errors: `Invalid` if `limit` > 200.
    pub async fn get_entries(&self, req: GetEntriesReq) -> Result<EntryPage, AppError> {
        Err(AppError::not_implemented("get_entries"))
    }

    pub async fn get_diff(&self, req: AttemptIdReq) -> Result<DiffResult, AppError> {
        Err(AppError::not_implemented("get_diff"))
    }

    pub async fn get_branch_status(&self, req: AttemptIdReq) -> Result<BranchStatus, AppError> {
        Err(AppError::not_implemented("get_branch_status"))
    }

    /// Spec §8.7. Errors: `Busy`, `WorktreeMissing`, `BranchMismatch`,
    /// `TargetCheckoutDirty`, `GitIdentityMissing`, `Git`.
    pub async fn merge_attempt(&self, req: MergeAttemptReq) -> Result<MergeOutcome, AppError> {
        Err(AppError::not_implemented("merge_attempt"))
    }

    /// Stops the turn, snapshot commit, removes the worktree (spec §5.4).
    pub async fn discard_attempt(&self, req: AttemptIdReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("discard_attempt"))
    }

    /// Errors: `Invalid` unless the attempt is merged and the branch is `atm/…`.
    pub async fn delete_branch(&self, req: AttemptIdReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("delete_branch"))
    }

    /// `open <wt>`, `open -a Terminal <wt>` or `open -a <editor_app> <wt>`; path from the DB.
    pub async fn open_attempt(&self, req: OpenAttemptReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("open_attempt"))
    }

    /// `open <url>`. Errors: `Invalid` unless `http(s)`.
    pub async fn open_url(&self, req: OpenUrlReq) -> Result<(), AppError> {
        Err(AppError::not_implemented("open_url"))
    }
}
