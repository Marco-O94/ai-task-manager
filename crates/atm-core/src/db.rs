//! SQLite persistence (spec §5): one `Mutex<Connection>`, WAL, migrations by `user_version`.
//! Owner: M2-DB. Every query is a short synchronous call; multi-row mutations run in one
//! transaction. Constraint violations map to typed errors (`Conflict`, `Busy`), everything
//! else from rusqlite to `Db`. Timestamps are passed in (`now`) so tests are deterministic.
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code)]

use std::path::Path;
use std::sync::Mutex;

use atm_types::{
    AppError, AttemptState, AttemptView, ConfigPolicy, CreateTaskReq, Effort, Entry, EntryPage, Id,
    Millis, PermissionMode, ProcessInfo, ProcessStatus, Project, Settings, StopReason, Task,
    TaskCard, TaskStatus, UpdateProjectReq, UpdateTaskReq, WorktreeState,
};
use rusqlite::Connection;

/// Applied on every open (spec §5.1).
pub const PRAGMAS: &str = "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA synchronous=NORMAL;";

/// Migration `i` brings `PRAGMA user_version` from `i` to `i + 1`.
pub const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];

/// Gap between consecutive positions (appends use `max + GAP`, renumbering uses `k * GAP`).
pub const POSITION_GAP: f64 = 1024.0;
/// A column is renumbered in the same transaction when two neighbours get closer than this.
pub const RENUMBER_EPSILON: f64 = 1e-6;
/// Upper bound for `entries_tail` / `entries_before` pages.
pub const MAX_ENTRY_PAGE: u32 = 200;

/// Applies every migration with index ≥ `PRAGMA user_version`, each in its own transaction,
/// then sets `user_version`. Idempotent. Errors: `Db`.
pub fn migrate(conn: &mut Connection) -> Result<(), AppError> {
    Err(AppError::not_implemented("db::migrate"))
}

/// `projects` row (spec §5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    pub id: Id,
    pub name: String,
    pub repo_path: String,
    pub default_target_branch: String,
    pub default_permission_mode: PermissionMode,
    pub default_model: Option<String>,
    pub config_policy: ConfigPolicy,
    /// sha256 hex approved by the user (spec §8.9); `None` = never approved.
    pub trusted_fingerprint: Option<String>,
    pub allow_bypass: bool,
    pub created_at: Millis,
    pub updated_at: Millis,
}

impl ProjectRow {
    /// IPC view; `trusted` (policy Trusted and the approved fingerprint still matching) is
    /// computed by the caller.
    pub fn to_project(&self, trusted: bool) -> Project {
        Project {
            id: self.id.clone(),
            name: self.name.clone(),
            repo_path: self.repo_path.clone(),
            default_target_branch: self.default_target_branch.clone(),
            default_permission_mode: self.default_permission_mode,
            default_model: self.default_model.clone(),
            config_policy: self.config_policy,
            trusted,
            allow_bypass: self.allow_bypass,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// `attempts` row (spec §5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptRow {
    pub id: Id,
    pub task_id: Id,
    pub state: AttemptState,
    pub branch: String,
    pub target_branch: String,
    pub base_commit: String,
    pub worktree_path: String,
    pub worktree_state: WorktreeState,
    pub session_id: String,
    pub session_started: bool,
    pub permission_mode: PermissionMode,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// `Tool(ruleContent)` strings from "Consenti sempre" (spec §7.8); JSON array in the DB.
    pub allow_rules: Vec<String>,
    pub merge_commit: Option<String>,
    pub created_at: Millis,
    pub updated_at: Millis,
    pub closed_at: Option<Millis>,
}

impl AttemptRow {
    /// IPC view; `running` and `pending_approvals` come from the live registry.
    pub fn view(&self, running: bool, pending_approvals: u32) -> AttemptView {
        AttemptView {
            id: self.id.clone(),
            task_id: self.task_id.clone(),
            state: self.state,
            branch: self.branch.clone(),
            target_branch: self.target_branch.clone(),
            base_commit: self.base_commit.clone(),
            worktree_path: self.worktree_path.clone(),
            worktree_state: self.worktree_state,
            permission_mode: self.permission_mode,
            model: self.model.clone(),
            effort: self.effort,
            session_started: self.session_started,
            merge_commit: self.merge_commit.clone(),
            running,
            pending_approvals,
            created_at: self.created_at,
            closed_at: self.closed_at,
        }
    }
}

/// An attempt with its task and project, as the runner and the merge need them.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptCtx {
    pub attempt: AttemptRow,
    pub task: Task,
    pub project: ProjectRow,
}

/// `processes` row (spec §5.2): one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessRow {
    pub id: Id,
    pub attempt_id: Id,
    pub seq: u32,
    pub prompt: String,
    pub permission_mode: PermissionMode,
    pub session_id: String,
    pub resumed: bool,
    pub status: ProcessStatus,
    pub stop_reason: Option<StopReason>,
    pub error: Option<String>,
    pub cli_version: Option<String>,
    /// Full argv as a JSON array; never the environment.
    pub argv_json: String,
    /// = pgid (`process_group(0)`).
    pub pid: Option<i32>,
    pub app_instance_id: String,
    pub exit_code: Option<i32>,
    pub result_subtype: Option<String>,
    pub is_error: Option<bool>,
    pub cost_usd_estimate: Option<f64>,
    pub num_turns: Option<u32>,
    pub duration_ms: Option<u64>,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub started_at: Millis,
    pub finished_at: Option<Millis>,
}

impl ProcessRow {
    pub fn info(&self) -> ProcessInfo {
        ProcessInfo {
            id: self.id.clone(),
            seq: self.seq,
            prompt: self.prompt.clone(),
            status: self.status,
            stop_reason: self.stop_reason,
            result_subtype: self.result_subtype.clone(),
            is_error: self.is_error,
            cost_usd_estimate: self.cost_usd_estimate,
            duration_ms: self.duration_ms,
            num_turns: self.num_turns,
            head_after: self.head_after.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
        }
    }
}

/// Final columns of a process written by [`Db::finish_process`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessFinish {
    pub status: ProcessStatus,
    pub stop_reason: Option<StopReason>,
    pub error: Option<String>,
    pub exit_code: Option<i32>,
    pub result_subtype: Option<String>,
    pub is_error: Option<bool>,
    pub cost_usd_estimate: Option<f64>,
    pub num_turns: Option<u32>,
    pub duration_ms: Option<u64>,
    pub head_after: Option<String>,
    pub finished_at: Millis,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Opens or creates the DB file (created 0600; the caller creates the parent dir 0700),
    /// applies [`PRAGMAS`] and [`migrate`]. Errors: `Db`, `Io`.
    pub fn open(path: &Path) -> Result<Db, AppError> {
        Err(AppError::not_implemented("Db::open"))
    }

    /// Private in-memory DB with the same PRAGMAs and migrations (tests).
    pub fn open_in_memory() -> Result<Db, AppError> {
        Err(AppError::not_implemented("Db::open_in_memory"))
    }

    // ---- settings -------------------------------------------------------------------------

    /// Every key of [`Settings`]; missing keys take `Settings::default()` values.
    pub fn settings(&self) -> Result<Settings, AppError> {
        Err(AppError::not_implemented("Db::settings"))
    }

    /// Upserts every key as JSON. Validation (e.g. `max_running` 1..=6) is the caller's.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::save_settings"))
    }

    // ---- projects -------------------------------------------------------------------------

    /// Errors: `Conflict` if `repo_path` is already a project.
    pub fn insert_project(&self, project: &ProjectRow) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::insert_project"))
    }

    /// Errors: `NotFound`.
    pub fn project(&self, id: &str) -> Result<ProjectRow, AppError> {
        Err(AppError::not_implemented("Db::project"))
    }

    /// All projects ordered by name.
    pub fn projects(&self) -> Result<Vec<ProjectRow>, AppError> {
        Err(AppError::not_implemented("Db::projects"))
    }

    /// Updates the editable fields and `updated_at`. Errors: `NotFound`.
    pub fn update_project(
        &self,
        req: &UpdateProjectReq,
        now: Millis,
    ) -> Result<ProjectRow, AppError> {
        Err(AppError::not_implemented("Db::update_project"))
    }

    /// Sets policy, bypass opt-in and the approved fingerprint (`None` clears it).
    /// Errors: `NotFound`.
    pub fn set_project_security(
        &self,
        id: &str,
        config_policy: ConfigPolicy,
        allow_bypass: bool,
        trusted_fingerprint: Option<&str>,
        now: Millis,
    ) -> Result<ProjectRow, AppError> {
        Err(AppError::not_implemented("Db::set_project_security"))
    }

    /// Deletes the project; tasks, attempts, processes and entries cascade.
    pub fn delete_project(&self, id: &str) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::delete_project"))
    }

    // ---- tasks ----------------------------------------------------------------------------

    /// Inserts task `id` at the end of column `req.status` (default todo): `max + GAP`.
    /// Errors: `NotFound` (project), `Invalid` (CHECK: title 1..=200, description ≤ 100000).
    pub fn insert_task(
        &self,
        id: &str,
        req: &CreateTaskReq,
        now: Millis,
    ) -> Result<Task, AppError> {
        Err(AppError::not_implemented("Db::insert_task"))
    }

    /// Errors: `NotFound`.
    pub fn task(&self, id: &str) -> Result<Task, AppError> {
        Err(AppError::not_implemented("Db::task"))
    }

    /// Updates title and description. Errors: `NotFound`, `Invalid`.
    pub fn update_task(&self, req: &UpdateTaskReq, now: Millis) -> Result<Task, AppError> {
        Err(AppError::not_implemented("Db::update_task"))
    }

    /// Lifecycle transition (spec §5.4): when the status changes the task goes to the end of
    /// the new column; same status = no-op. Errors: `NotFound`.
    pub fn set_task_status(
        &self,
        id: &str,
        status: TaskStatus,
        now: Millis,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::set_task_status"))
    }

    /// Manual move (spec §5.2 positions): before `before_id` (midpoint with its predecessor)
    /// or at the end when `None`; renumbers the column by `GAP` in the same transaction when
    /// the gap falls under [`RENUMBER_EPSILON`]. The running-turn rule (`Busy`) is the
    /// caller's. Errors: `NotFound`, `Invalid` (`before_id` not in the destination column).
    pub fn move_task(
        &self,
        id: &str,
        status: TaskStatus,
        before_id: Option<&str>,
        now: Millis,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::move_task"))
    }

    /// Deletes the task and, by cascade, its attempts, processes and entries.
    pub fn delete_task(&self, id: &str) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::delete_task"))
    }

    /// Board of a project ordered by (column, position): each task joined with its active
    /// attempt, else its most recent one (`attempt_state` says which), and that attempt's
    /// highest-`seq` process.
    /// `running` = that attempt has a `running` process; `pending_approvals` is 0 (the
    /// caller merges the live registry).
    pub fn board(&self, project_id: &str) -> Result<Vec<TaskCard>, AppError> {
        Err(AppError::not_implemented("Db::board"))
    }

    /// One card, same rules as [`Db::board`]. Errors: `NotFound`.
    pub fn task_card(&self, task_id: &str) -> Result<TaskCard, AppError> {
        Err(AppError::not_implemented("Db::task_card"))
    }

    // ---- attempts -------------------------------------------------------------------------

    /// One transaction (spec §6.3 `start_attempt`): inserts the attempt and its first
    /// `running` process, task → inprogress. Errors: `Conflict` (another active attempt on
    /// the task, or duplicate worktree path / session id).
    pub fn begin_attempt(
        &self,
        attempt: &AttemptRow,
        process: &ProcessRow,
        now: Millis,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::begin_attempt"))
    }

    /// Errors: `NotFound`.
    pub fn attempt(&self, id: &str) -> Result<AttemptRow, AppError> {
        Err(AppError::not_implemented("Db::attempt"))
    }

    /// Attempt joined with its task and project. Errors: `NotFound`.
    pub fn attempt_ctx(&self, attempt_id: &str) -> Result<AttemptCtx, AppError> {
        Err(AppError::not_implemented("Db::attempt_ctx"))
    }

    pub fn active_attempt(&self, task_id: &str) -> Result<Option<AttemptRow>, AppError> {
        Err(AppError::not_implemented("Db::active_attempt"))
    }

    /// Every attempt of the task, oldest first.
    pub fn task_attempts(&self, task_id: &str) -> Result<Vec<AttemptRow>, AppError> {
        Err(AppError::not_implemented("Db::task_attempts"))
    }

    /// Attempts whose worktree is `present` or `missing` (reconciliation, project removal),
    /// optionally restricted to one project.
    pub fn attempts_with_worktree(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<AttemptRow>, AppError> {
        Err(AppError::not_implemented("Db::attempts_with_worktree"))
    }

    pub fn set_worktree_state(
        &self,
        attempt_id: &str,
        state: WorktreeState,
        now: Millis,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::set_worktree_state"))
    }

    /// Replaces the session: a new UUID after a turn died before `init` (`started=false`),
    /// `started=true` on `system/init`, or the id observed there (spec §7.9).
    pub fn set_session(
        &self,
        attempt_id: &str,
        session_id: &str,
        started: bool,
        now: Millis,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::set_session"))
    }

    /// Appends the rules not already present; returns the full list.
    pub fn add_allow_rules(
        &self,
        attempt_id: &str,
        rules: &[String],
        now: Millis,
    ) -> Result<Vec<String>, AppError> {
        Err(AppError::not_implemented("Db::add_allow_rules"))
    }

    /// One transaction (spec §8.7 step 7): attempt → merged with `merge_commit` and
    /// `closed_at`, task → done.
    pub fn finish_merge(
        &self,
        attempt_id: &str,
        merge_commit: &str,
        now: Millis,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::finish_merge"))
    }

    /// One transaction: attempt → discarded with `closed_at`; task inprogress|inreview → todo.
    pub fn finish_discard(&self, attempt_id: &str, now: Millis) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::finish_discard"))
    }

    // ---- processes ------------------------------------------------------------------------

    /// `max(seq) + 1` for the attempt (1 for the first turn).
    pub fn next_seq(&self, attempt_id: &str) -> Result<u32, AppError> {
        Err(AppError::not_implemented("Db::next_seq"))
    }

    /// One transaction (follow-up): inserts the `running` process, task → inprogress.
    /// Errors: `Busy` if the attempt already has a running process (partial unique index).
    pub fn begin_turn(&self, process: &ProcessRow, now: Millis) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::begin_turn"))
    }

    /// Records the spawned process group and the CLI version in use.
    pub fn set_process_pid(
        &self,
        process_id: &str,
        pid: i32,
        cli_version: Option<&str>,
    ) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::set_process_pid"))
    }

    /// One transaction: writes the final columns; if the attempt has no other running
    /// process, its task inprogress → inreview.
    pub fn finish_process(&self, process_id: &str, fin: &ProcessFinish) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::finish_process"))
    }

    /// Errors: `NotFound`.
    pub fn process(&self, id: &str) -> Result<ProcessRow, AppError> {
        Err(AppError::not_implemented("Db::process"))
    }

    /// Turns of the attempt by `seq`.
    pub fn attempt_processes(&self, attempt_id: &str) -> Result<Vec<ProcessRow>, AppError> {
        Err(AppError::not_implemented("Db::attempt_processes"))
    }

    /// Every `running` process (all attempts).
    pub fn running_processes(&self) -> Result<Vec<ProcessRow>, AppError> {
        Err(AppError::not_implemented("Db::running_processes"))
    }

    /// Startup recovery (spec §5.4, §7.9), one transaction: every `running` process with a
    /// different `app_instance_id` → failed / app_restart / `finished_at = now`; then every
    /// inprogress task without a running process → inreview. Returns the marked rows (with
    /// their `pid`) so the caller can kill verified groups, cancel tools and auto-commit.
    pub fn mark_orphans(
        &self,
        app_instance_id: &str,
        now: Millis,
    ) -> Result<Vec<ProcessRow>, AppError> {
        Err(AppError::not_implemented("Db::mark_orphans"))
    }

    // ---- entries --------------------------------------------------------------------------

    /// Idempotent upsert by `(attempt_id, idx)`: a row is written only if absent or if the
    /// stored `rev` is lower. `kind` = `EntryBody::kind()`, `payload` = the `Entry` JSON.
    pub fn upsert_entries(&self, attempt_id: &str, entries: &[Entry]) -> Result<(), AppError> {
        Err(AppError::not_implemented("Db::upsert_entries"))
    }

    /// The newest `limit` (≤ [`MAX_ENTRY_PAGE`]) entries in ascending `idx`; `has_more` =
    /// older ones exist.
    pub fn entries_tail(&self, attempt_id: &str, limit: u32) -> Result<EntryPage, AppError> {
        Err(AppError::not_implemented("Db::entries_tail"))
    }

    /// The newest `limit` entries with `idx < before_idx`, ascending; `has_more` as above.
    pub fn entries_before(
        &self,
        attempt_id: &str,
        before_idx: u32,
        limit: u32,
    ) -> Result<EntryPage, AppError> {
        Err(AppError::not_implemented("Db::entries_before"))
    }

    /// `max(idx) + 1`, or 0: the `next_idx` of a new turn's `Normalizer`.
    pub fn next_entry_idx(&self, attempt_id: &str) -> Result<u32, AppError> {
        Err(AppError::not_implemented("Db::next_entry_idx"))
    }

    /// Every `ToolCall` of `process_id` still `Running` or `AwaitingApproval` → `Cancelled`
    /// with `rev + 1` (recovery of a dead turn). Returns the updated entries.
    pub fn cancel_open_tools(
        &self,
        attempt_id: &str,
        process_id: &str,
    ) -> Result<Vec<Entry>, AppError> {
        Err(AppError::not_implemented("Db::cancel_open_tools"))
    }
}
