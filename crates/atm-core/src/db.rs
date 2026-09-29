//! SQLite persistence (spec §5): one `Mutex<Connection>`, WAL, migrations by `user_version`.
//! Owner: M2-DB. Every query is a short synchronous call; multi-row mutations run in one
//! transaction. Constraint violations map to typed errors (`Conflict`, `Busy`), everything
//! else from rusqlite to `Db`. Timestamps are passed in (`now`) so tests are deterministic.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use atm_types::{
    AppError, AttemptState, AttemptView, ConfigPolicy, CreateTaskReq, Effort, Entry, EntryBody,
    EntryPage, Id, Millis, PermissionMode, ProcessInfo, ProcessStatus, Project, Settings,
    StopReason, Task, TaskCard, TaskStatus, ToolStatus, UpdateProjectReq, UpdateTaskReq,
    WorktreeState,
};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Params, Row, TransactionBehavior, ffi, params,
};
use serde::de::DeserializeOwned;

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
    Ok(apply_migrations(conn)?)
}

fn apply_migrations(conn: &mut Connection) -> Res<()> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version as usize > MIGRATIONS.len() {
        return Err(AppError::db(format!(
            "Database creato da una versione più recente dell'app (schema {version})"
        ))
        .into());
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
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
            trust_error: None,
        }
    }

    /// The security fields, as a compare-and-set expects them ([`Db::set_project_security`]).
    pub fn security(&self) -> SecurityState {
        SecurityState {
            config_policy: self.config_policy,
            allow_bypass: self.allow_bypass,
            trusted_fingerprint: self.trusted_fingerprint.clone(),
        }
    }
}

/// The stored security state of a project that a change applies to (M6): a change whose
/// expected state is no longer the stored one is refused (`Conflict`), so what the user
/// confirmed is what gets stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityState {
    pub config_policy: ConfigPolicy,
    pub allow_bypass: bool,
    pub trusted_fingerprint: Option<String>,
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
        use std::os::unix::fs::OpenOptionsExt;

        let named =
            |e: AppError| AppError::new(e.code, format!("{}: {}", path.display(), e.message));
        // SQLite would create the file with the umask's mode; its -wal and -shm copy this one.
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| named(e.into()))?;
        Self::init(Connection::open(path)).map_err(|e| named(e.into()))
    }

    /// Private in-memory DB with the same PRAGMAs and migrations (tests).
    pub fn open_in_memory() -> Result<Db, AppError> {
        Ok(Self::init(Connection::open_in_memory())?)
    }

    fn init(conn: rusqlite::Result<Connection>) -> Res<Db> {
        let mut conn = conn?;
        conn.execute_batch(PRAGMAS)?;
        apply_migrations(&mut conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic inside a transaction rolls it back while unwinding: the connection is sound.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Res<T>) -> Result<T, AppError> {
        Ok(f(&self.lock())?)
    }

    /// Runs `f` in an IMMEDIATE transaction, committed only if `f` succeeds.
    fn write<T>(&self, f: impl FnOnce(&Connection) -> Res<T>) -> Result<T, AppError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(Failure::from)?;
        let out = f(&tx)?;
        tx.commit().map_err(Failure::from)?;
        Ok(out)
    }

    // ---- settings -------------------------------------------------------------------------

    /// Every key of [`Settings`]; missing keys take `Settings::default()` values.
    pub fn settings(&self) -> Result<Settings, AppError> {
        self.read(|c| {
            let mut settings = serde_json::to_value(Settings::default())?;
            let rows: Vec<(String, String)> = all(c, "SELECT key, value FROM settings", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            for (key, value) in rows {
                if let Some(slot) = settings.get_mut(&key) {
                    *slot = serde_json::from_str(&value)?;
                }
            }
            Ok(serde_json::from_value(settings)?)
        })
    }

    /// Upserts every key as JSON. Validation (e.g. `max_running` 1..=6) is the caller's.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), AppError> {
        self.write(|c| {
            let serde_json::Value::Object(map) = serde_json::to_value(settings)? else {
                unreachable!("Settings serializes as an object")
            };
            let mut stmt = c.prepare_cached(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )?;
            for (key, value) in map {
                stmt.execute(params![key, value.to_string()])?;
            }
            Ok(())
        })
    }

    // ---- projects -------------------------------------------------------------------------

    /// Errors: `Conflict` if `repo_path` is already a project.
    pub fn insert_project(&self, project: &ProjectRow) -> Result<(), AppError> {
        let p = project;
        self.write(|c| {
            c.execute(
                "INSERT INTO projects (id, name, repo_path, default_target_branch,
                    default_permission_mode, default_model, config_policy, trusted_fingerprint,
                    allow_bypass, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    p.id,
                    p.name,
                    p.repo_path,
                    p.default_target_branch,
                    p.default_permission_mode.as_str(),
                    p.default_model,
                    p.config_policy.as_str(),
                    p.trusted_fingerprint,
                    p.allow_bypass,
                    p.created_at,
                    p.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    /// Errors: `NotFound`.
    pub fn project(&self, id: &str) -> Result<ProjectRow, AppError> {
        self.read(|c| get_project(c, id))
    }

    /// All projects ordered by name.
    pub fn projects(&self) -> Result<Vec<ProjectRow>, AppError> {
        self.read(|c| {
            all(
                c,
                "SELECT * FROM projects ORDER BY name COLLATE NOCASE, id",
                [],
                project_row,
            )
        })
    }

    /// Updates the editable fields and `updated_at`. Errors: `NotFound`.
    pub fn update_project(
        &self,
        req: &UpdateProjectReq,
        now: Millis,
    ) -> Result<ProjectRow, AppError> {
        self.write(|c| {
            // Autonomo only while the project allows it, checked with the write (a revocation
            // may have landed since the caller read the row).
            let row = c
                .query_row(
                    "UPDATE projects SET name = ?2, default_target_branch = ?3,
                        default_permission_mode = ?4, default_model = ?5, updated_at = ?6
                     WHERE id = ?1 AND (?4 <> 'bypassPermissions' OR allow_bypass = 1)
                     RETURNING *",
                    params![
                        req.id,
                        req.name,
                        req.default_target_branch,
                        req.default_permission_mode.as_str(),
                        req.default_model,
                        now,
                    ],
                    project_row,
                )
                .optional()?;
            match row {
                Some(row) => Ok(row),
                None => {
                    get_project(c, &req.id)?;
                    Err(AppError::invalid(
                        "La modalità Autonoma richiede di abilitarla nella sicurezza del progetto",
                    )
                    .into())
                }
            }
        })
    }

    /// Sets policy, bypass opt-in and the approved fingerprint (`None` clears it), and resets
    /// a default mode of Autonomo to Auto-edit when the bypass is off, in one statement, only
    /// if the project still has `expected` (compare-and-set). Errors: `NotFound`, `Conflict`
    /// (the stored state is no longer `expected`).
    pub fn set_project_security(
        &self,
        id: &str,
        expected: &SecurityState,
        config_policy: ConfigPolicy,
        allow_bypass: bool,
        trusted_fingerprint: Option<&str>,
        now: Millis,
    ) -> Result<ProjectRow, AppError> {
        self.write(|c| {
            let row = c
                .query_row(
                    "UPDATE projects SET config_policy = ?2, allow_bypass = ?3,
                        trusted_fingerprint = ?4,
                        default_permission_mode = CASE
                            WHEN ?3 = 0 AND default_permission_mode = 'bypassPermissions'
                            THEN 'acceptEdits' ELSE default_permission_mode END,
                        updated_at = ?5
                     WHERE id = ?1 AND config_policy = ?6 AND allow_bypass = ?7
                        AND trusted_fingerprint IS ?8
                     RETURNING *",
                    params![
                        id,
                        config_policy.as_str(),
                        allow_bypass,
                        trusted_fingerprint,
                        now,
                        expected.config_policy.as_str(),
                        expected.allow_bypass,
                        expected.trusted_fingerprint,
                    ],
                    project_row,
                )
                .optional()?;
            match row {
                Some(row) => Ok(row),
                None => {
                    get_project(c, id)?;
                    Err(AppError::conflict(
                        "La sicurezza del progetto è cambiata nel frattempo: riapri le \
                         impostazioni e riprova",
                    )
                    .into())
                }
            }
        })
    }

    /// Deletes the project; tasks, attempts, processes and entries cascade.
    pub fn delete_project(&self, id: &str) -> Result<(), AppError> {
        self.write(|c| {
            c.execute("DELETE FROM projects WHERE id = ?1", [id])?;
            Ok(())
        })
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
        self.write(|c| {
            let status = req.status.unwrap_or(TaskStatus::Todo);
            let position = end_of_column(c, &req.project_id, status, id)?;
            Ok(c.query_row(
                "INSERT INTO tasks (id, project_id, title, description, status, position,
                    created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) RETURNING *",
                params![
                    id,
                    req.project_id,
                    req.title,
                    req.description,
                    status.as_str(),
                    position,
                    now
                ],
                task_row,
            )?)
        })
    }

    /// Errors: `NotFound`.
    pub fn task(&self, id: &str) -> Result<Task, AppError> {
        self.read(|c| get_task(c, id))
    }

    /// Updates title and description. Errors: `NotFound`, `Invalid`.
    pub fn update_task(&self, req: &UpdateTaskReq, now: Millis) -> Result<Task, AppError> {
        self.write(|c| {
            c.query_row(
                "UPDATE tasks SET title = ?2, description = ?3, updated_at = ?4
                 WHERE id = ?1 RETURNING *",
                params![req.id, req.title, req.description, now],
                task_row,
            )
            .or_missing("Task", &req.id)
        })
    }

    /// Lifecycle transition (spec §5.4): when the status changes the task goes to the end of
    /// the new column; same status = no-op. Errors: `NotFound`.
    pub fn set_task_status(
        &self,
        id: &str,
        status: TaskStatus,
        now: Millis,
    ) -> Result<(), AppError> {
        self.write(|c| transition(c, id, TaskStatus::ALL, status, now))
    }

    /// Manual move (spec §5.2 positions): before `before_id` (midpoint with its predecessor)
    /// or at the end when `None`; renumbers the column by `GAP` in the same transaction when
    /// the gap falls under [`RENUMBER_EPSILON`]. The running-turn rule (`Busy`) is the
    /// caller's. Errors: `NotFound`, `Invalid` (`before_id` not in the destination column).
    ///
    /// `before_id == Some(id)` inside the task's own column leaves it where it is.
    pub fn move_task(
        &self,
        id: &str,
        status: TaskStatus,
        before_id: Option<&str>,
        now: Millis,
    ) -> Result<(), AppError> {
        self.write(|c| {
            let task = get_task(c, id)?;
            if before_id == Some(id) && task.status == status {
                return Ok(());
            }
            let position = match before_id {
                None => end_of_column(c, &task.project_id, status, id)?,
                Some(before) => {
                    let b: f64 = c
                        .query_row(
                            "SELECT position FROM tasks
                             WHERE id = ?1 AND project_id = ?2 AND status = ?3",
                            params![before, task.project_id, status.as_str()],
                            |r| r.get(0),
                        )
                        .optional()?
                        .ok_or_else(|| {
                            AppError::invalid(format!(
                                "Il task {before} non è nella colonna di destinazione"
                            ))
                        })?;
                    let a: Option<f64> = c.query_row(
                        "SELECT MAX(position) FROM tasks
                         WHERE project_id = ?1 AND status = ?2 AND position < ?3 AND id <> ?4",
                        params![task.project_id, status.as_str(), b, id],
                        |r| r.get(0),
                    )?;
                    let a = a.unwrap_or(b - POSITION_GAP);
                    if b - a < RENUMBER_EPSILON {
                        renumber(c, &task.project_id, status, id, before)?
                    } else {
                        (a + b) / 2.0
                    }
                }
            };
            place(c, id, status, position, now)
        })
    }

    /// Deletes the task and, by cascade, its attempts, processes and entries.
    pub fn delete_task(&self, id: &str) -> Result<(), AppError> {
        self.write(|c| {
            c.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    /// Board of a project ordered by (column, position): each task joined with its active
    /// attempt, else its most recent one (`attempt_state` says which), and that attempt's
    /// highest-`seq` process.
    /// `running` = that attempt has a `running` process; `pending_approvals` is 0 (the
    /// caller merges the live registry).
    pub fn board(&self, project_id: &str) -> Result<Vec<TaskCard>, AppError> {
        let mut cards = self.read(|c| {
            all(
                c,
                &format!("{CARD_SELECT} WHERE t.project_id = ?1 ORDER BY t.position"),
                [project_id],
                card_row,
            )
        })?;
        // Stable: positions stay ordered inside each column; declaration order = board order.
        cards.sort_by_key(|card| card.task.status as u8);
        Ok(cards)
    }

    /// One card, same rules as [`Db::board`]. Errors: `NotFound`.
    pub fn task_card(&self, task_id: &str) -> Result<TaskCard, AppError> {
        self.read(|c| {
            c.query_row(
                &format!("{CARD_SELECT} WHERE t.id = ?1"),
                [task_id],
                card_row,
            )
            .or_missing("Task", task_id)
        })
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
        let a = attempt;
        self.write(|c| {
            c.execute(
                "INSERT INTO attempts (id, task_id, state, branch, target_branch, base_commit,
                    worktree_path, worktree_state, session_id, session_started, permission_mode,
                    model, effort, allow_rules, merge_commit, created_at, updated_at, closed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                    ?17, ?18)",
                params![
                    a.id,
                    a.task_id,
                    a.state.as_str(),
                    a.branch,
                    a.target_branch,
                    a.base_commit,
                    a.worktree_path,
                    a.worktree_state.as_str(),
                    a.session_id,
                    a.session_started,
                    a.permission_mode.as_str(),
                    a.model,
                    a.effort.map(Effort::as_str),
                    serde_json::to_string(&a.allow_rules)?,
                    a.merge_commit,
                    a.created_at,
                    a.updated_at,
                    a.closed_at,
                ],
            )?;
            insert_process(c, process)?;
            transition(c, &a.task_id, TaskStatus::ALL, TaskStatus::InProgress, now)
        })
    }

    /// Errors: `NotFound`.
    pub fn attempt(&self, id: &str) -> Result<AttemptRow, AppError> {
        self.read(|c| get_attempt(c, id))
    }

    /// Attempt joined with its task and project. Errors: `NotFound`.
    pub fn attempt_ctx(&self, attempt_id: &str) -> Result<AttemptCtx, AppError> {
        self.read(|c| {
            let attempt = get_attempt(c, attempt_id)?;
            let task = get_task(c, &attempt.task_id)?;
            let project = get_project(c, &task.project_id)?;
            Ok(AttemptCtx {
                attempt,
                task,
                project,
            })
        })
    }

    pub fn active_attempt(&self, task_id: &str) -> Result<Option<AttemptRow>, AppError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT * FROM attempts WHERE task_id = ?1 AND state = 'active'",
                [task_id],
                attempt_row,
            )
            .optional()?)
        })
    }

    /// Every attempt of the task, oldest first.
    pub fn task_attempts(&self, task_id: &str) -> Result<Vec<AttemptRow>, AppError> {
        self.read(|c| {
            all(
                c,
                "SELECT * FROM attempts WHERE task_id = ?1 ORDER BY created_at, rowid",
                [task_id],
                attempt_row,
            )
        })
    }

    /// Attempts whose worktree is `present` or `missing` (reconciliation, project removal),
    /// optionally restricted to one project.
    pub fn attempts_with_worktree(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<AttemptRow>, AppError> {
        self.read(|c| {
            all(
                c,
                "SELECT a.* FROM attempts a JOIN tasks t ON t.id = a.task_id
                 WHERE a.worktree_state IN ('present', 'missing')
                    AND (?1 IS NULL OR t.project_id = ?1)
                 ORDER BY a.created_at, a.rowid",
                [project_id],
                attempt_row,
            )
        })
    }

    /// Errors: `NotFound`.
    pub fn set_worktree_state(
        &self,
        attempt_id: &str,
        state: WorktreeState,
        now: Millis,
    ) -> Result<(), AppError> {
        self.write(|c| {
            let n = c.execute(
                "UPDATE attempts SET worktree_state = ?2, updated_at = ?3 WHERE id = ?1",
                params![attempt_id, state.as_str(), now],
            )?;
            updated(n, "Attempt", attempt_id)
        })
    }

    /// Replaces the session: a new UUID after a turn died before `init` (`started=false`),
    /// `started=true` on `system/init`, or the id observed there (spec §7.9).
    /// Errors: `NotFound`, `Conflict` (session id of another attempt).
    pub fn set_session(
        &self,
        attempt_id: &str,
        session_id: &str,
        started: bool,
        now: Millis,
    ) -> Result<(), AppError> {
        self.write(|c| {
            let n = c.execute(
                "UPDATE attempts SET session_id = ?2, session_started = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![attempt_id, session_id, started, now],
            )?;
            updated(n, "Attempt", attempt_id)
        })
    }

    /// Appends the rules not already present; returns the full list. Errors: `NotFound`.
    pub fn add_allow_rules(
        &self,
        attempt_id: &str,
        rules: &[String],
        now: Millis,
    ) -> Result<Vec<String>, AppError> {
        self.write(|c| {
            let Json(mut list) = c
                .query_row(
                    "SELECT allow_rules FROM attempts WHERE id = ?1",
                    [attempt_id],
                    |r| r.get::<_, Json<Vec<String>>>(0),
                )
                .or_missing("Attempt", attempt_id)?;
            let known = list.len();
            for rule in rules {
                if !list.contains(rule) {
                    list.push(rule.clone());
                }
            }
            if list.len() > known {
                c.execute(
                    "UPDATE attempts SET allow_rules = ?2, updated_at = ?3 WHERE id = ?1",
                    params![attempt_id, serde_json::to_string(&list)?, now],
                )?;
            }
            Ok(list)
        })
    }

    /// One transaction (spec §8.7 step 7): attempt → merged with `merge_commit` and
    /// `closed_at`, task → done. Errors: `NotFound`.
    pub fn finish_merge(
        &self,
        attempt_id: &str,
        merge_commit: &str,
        now: Millis,
    ) -> Result<(), AppError> {
        self.write(|c| {
            let task_id: String = c
                .query_row(
                    "UPDATE attempts SET state = 'merged', merge_commit = ?2, closed_at = ?3,
                        updated_at = ?3
                     WHERE id = ?1 RETURNING task_id",
                    params![attempt_id, merge_commit, now],
                    |r| r.get(0),
                )
                .or_missing("Attempt", attempt_id)?;
            transition(c, &task_id, TaskStatus::ALL, TaskStatus::Done, now)
        })
    }

    /// One transaction: attempt → discarded with `closed_at`; task inprogress|inreview → todo.
    /// Errors: `NotFound`.
    pub fn finish_discard(&self, attempt_id: &str, now: Millis) -> Result<(), AppError> {
        self.write(|c| {
            let task_id: String = c
                .query_row(
                    "UPDATE attempts SET state = 'discarded', closed_at = ?2, updated_at = ?2
                     WHERE id = ?1 RETURNING task_id",
                    params![attempt_id, now],
                    |r| r.get(0),
                )
                .or_missing("Attempt", attempt_id)?;
            let from = [TaskStatus::InProgress, TaskStatus::InReview];
            transition(c, &task_id, &from, TaskStatus::Todo, now)
        })
    }

    // ---- processes ------------------------------------------------------------------------

    /// `max(seq) + 1` for the attempt (1 for the first turn).
    pub fn next_seq(&self, attempt_id: &str) -> Result<u32, AppError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM processes WHERE attempt_id = ?1",
                [attempt_id],
                |r| r.get(0),
            )?)
        })
    }

    /// One transaction (follow-up): inserts the `running` process, task → inprogress.
    /// Errors: `Busy` if the attempt already has a running process (partial unique index),
    /// `NotFound` (attempt).
    pub fn begin_turn(&self, process: &ProcessRow, now: Millis) -> Result<(), AppError> {
        self.write(|c| {
            let (task_id, running): (String, bool) = c
                .query_row(
                    "SELECT task_id, EXISTS (SELECT 1 FROM processes p
                        WHERE p.attempt_id = a.id AND p.status = 'running')
                     FROM attempts a WHERE a.id = ?1",
                    [&process.attempt_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .or_missing("Attempt", &process.attempt_id)?;
            if running {
                return Err(AppError::busy("Un turno dell'agente è già in esecuzione").into());
            }
            insert_process(c, process)?;
            transition(c, &task_id, TaskStatus::ALL, TaskStatus::InProgress, now)
        })
    }

    /// Records the spawned process group and the CLI version in use. Errors: `NotFound`.
    pub fn set_process_pid(
        &self,
        process_id: &str,
        pid: i32,
        cli_version: Option<&str>,
    ) -> Result<(), AppError> {
        self.write(|c| {
            let n = c.execute(
                "UPDATE processes SET pid = ?2, cli_version = ?3 WHERE id = ?1",
                params![process_id, pid, cli_version],
            )?;
            updated(n, "Processo", process_id)
        })
    }

    /// One transaction: writes the final columns; if the attempt has no other running
    /// process, its task inprogress → inreview. Errors: `NotFound`.
    pub fn finish_process(&self, process_id: &str, fin: &ProcessFinish) -> Result<(), AppError> {
        self.write(|c| {
            let attempt_id: String = c
                .query_row(
                    "UPDATE processes SET status = ?2, stop_reason = ?3, error = ?4,
                        exit_code = ?5, result_subtype = ?6, is_error = ?7,
                        cost_usd_estimate = ?8, num_turns = ?9, duration_ms = ?10,
                        head_after = ?11, finished_at = ?12
                     WHERE id = ?1 RETURNING attempt_id",
                    params![
                        process_id,
                        fin.status.as_str(),
                        fin.stop_reason.map(StopReason::as_str),
                        fin.error,
                        fin.exit_code,
                        fin.result_subtype,
                        fin.is_error,
                        fin.cost_usd_estimate,
                        fin.num_turns,
                        fin.duration_ms.map(|d| d as i64),
                        fin.head_after,
                        fin.finished_at,
                    ],
                    |r| r.get(0),
                )
                .or_missing("Processo", process_id)?;
            let idle_task: Option<String> = c
                .query_row(
                    "SELECT task_id FROM attempts a WHERE a.id = ?1 AND NOT EXISTS (
                        SELECT 1 FROM processes p
                        WHERE p.attempt_id = a.id AND p.status = 'running')",
                    [&attempt_id],
                    |r| r.get(0),
                )
                .optional()?;
            match idle_task {
                Some(task_id) => transition(
                    c,
                    &task_id,
                    &[TaskStatus::InProgress],
                    TaskStatus::InReview,
                    fin.finished_at,
                ),
                None => Ok(()),
            }
        })
    }

    /// Errors: `NotFound`.
    pub fn process(&self, id: &str) -> Result<ProcessRow, AppError> {
        self.read(|c| {
            c.query_row("SELECT * FROM processes WHERE id = ?1", [id], process_row)
                .or_missing("Processo", id)
        })
    }

    /// Turns of the attempt by `seq`.
    pub fn attempt_processes(&self, attempt_id: &str) -> Result<Vec<ProcessRow>, AppError> {
        self.read(|c| {
            all(
                c,
                "SELECT * FROM processes WHERE attempt_id = ?1 ORDER BY seq",
                [attempt_id],
                process_row,
            )
        })
    }

    /// Every `running` process (all attempts).
    pub fn running_processes(&self) -> Result<Vec<ProcessRow>, AppError> {
        self.read(|c| {
            all(
                c,
                "SELECT * FROM processes WHERE status = 'running' ORDER BY started_at, id",
                [],
                process_row,
            )
        })
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
        self.write(|c| {
            let mut marked = all(
                c,
                "UPDATE processes SET status = 'failed', stop_reason = 'app_restart',
                    finished_at = ?2
                 WHERE status = 'running' AND app_instance_id <> ?1 RETURNING *",
                params![app_instance_id, now],
                process_row,
            )?;
            marked.sort_by_key(|p| p.started_at);
            let idle: Vec<String> = all(
                c,
                "SELECT id FROM tasks t WHERE status = 'inprogress' AND NOT EXISTS (
                    SELECT 1 FROM attempts a JOIN processes p ON p.attempt_id = a.id
                    WHERE a.task_id = t.id AND p.status = 'running')
                 ORDER BY project_id, position",
                [],
                |r| r.get(0),
            )?;
            for task_id in idle {
                transition(
                    c,
                    &task_id,
                    &[TaskStatus::InProgress],
                    TaskStatus::InReview,
                    now,
                )?;
            }
            Ok(marked)
        })
    }

    // ---- entries --------------------------------------------------------------------------

    /// Idempotent upsert by `(attempt_id, idx)`: a row is written only if absent or if the
    /// stored `rev` is lower. `kind` = `EntryBody::kind()`, `payload` = the `Entry` JSON.
    pub fn upsert_entries(&self, attempt_id: &str, entries: &[Entry]) -> Result<(), AppError> {
        self.write(|c| upsert(c, attempt_id, entries))
    }

    /// The newest `limit` (≤ [`MAX_ENTRY_PAGE`]) entries in ascending `idx`; `has_more` =
    /// older ones exist.
    pub fn entries_tail(&self, attempt_id: &str, limit: u32) -> Result<EntryPage, AppError> {
        self.read(|c| page(c, attempt_id, i64::MAX, limit))
    }

    /// The newest `limit` entries with `idx < before_idx`, ascending; `has_more` as above.
    pub fn entries_before(
        &self,
        attempt_id: &str,
        before_idx: u32,
        limit: u32,
    ) -> Result<EntryPage, AppError> {
        self.read(|c| page(c, attempt_id, before_idx.into(), limit))
    }

    /// `max(idx) + 1`, or 0: the `next_idx` of a new turn's `Normalizer`.
    pub fn next_entry_idx(&self, attempt_id: &str) -> Result<u32, AppError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(idx) + 1, 0) FROM entries WHERE attempt_id = ?1",
                [attempt_id],
                |r| r.get(0),
            )?)
        })
    }

    /// Every `ToolCall` of `process_id` still `Running` or `AwaitingApproval` → `Cancelled`
    /// with `rev + 1` (recovery of a dead turn). Returns the updated entries.
    pub fn cancel_open_tools(
        &self,
        attempt_id: &str,
        process_id: &str,
    ) -> Result<Vec<Entry>, AppError> {
        self.write(|c| {
            let calls = all(
                c,
                "SELECT payload FROM entries
                 WHERE attempt_id = ?1 AND process_id = ?2 AND kind = 'ToolCall' ORDER BY idx",
                [attempt_id, process_id],
                entry_row,
            )?;
            let cancelled: Vec<Entry> = calls
                .into_iter()
                .filter_map(|mut e| {
                    let EntryBody::ToolCall { status, .. } = &mut e.body else {
                        return None;
                    };
                    if !matches!(
                        status,
                        ToolStatus::Running | ToolStatus::AwaitingApproval { .. }
                    ) {
                        return None;
                    }
                    *status = ToolStatus::Cancelled;
                    e.rev += 1;
                    Some(e)
                })
                .collect();
            upsert(c, attempt_id, &cancelled)?;
            Ok(cancelled)
        })
    }
}

// ---- errors -------------------------------------------------------------------------------

/// Internal error, so that `?` works on rusqlite, serde_json and typed errors alike.
enum Failure {
    Sql(rusqlite::Error),
    App(AppError),
}

type Res<T> = Result<T, Failure>;

impl From<rusqlite::Error> for Failure {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

impl From<AppError> for Failure {
    fn from(e: AppError) -> Self {
        Self::App(e)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(e: serde_json::Error) -> Self {
        Self::App(e.into())
    }
}

impl From<Failure> for AppError {
    fn from(f: Failure) -> Self {
        let e = match f {
            Failure::App(e) => return e,
            Failure::Sql(e) => e,
        };
        let Some(sqlite) = e.sqlite_error() else {
            return AppError::db(format!("Errore del database: {e}"));
        };
        match sqlite.extended_code {
            ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
                AppError::conflict(format!("Esiste già ({e})"))
            }
            ffi::SQLITE_CONSTRAINT_CHECK
            | ffi::SQLITE_CONSTRAINT_NOTNULL
            | ffi::SQLITE_CONSTRAINT_DATATYPE => {
                AppError::invalid(format!("Valore non valido ({e})"))
            }
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY => {
                AppError::not_found(format!("Riferimento inesistente ({e})"))
            }
            _ if matches!(
                sqlite.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
            {
                AppError::busy(format!("Database occupato ({e})"))
            }
            _ => AppError::db(format!("Errore del database: {e}")),
        }
    }
}

fn missing(what: &str, id: &str) -> Failure {
    AppError::not_found(format!("{what} non trovato: {id}")).into()
}

/// `NotFound` when a single-row statement matched nothing.
trait OrMissing<T> {
    fn or_missing(self, what: &str, id: &str) -> Res<T>;
}

impl<T> OrMissing<T> for rusqlite::Result<T> {
    fn or_missing(self, what: &str, id: &str) -> Res<T> {
        self.optional()?.ok_or_else(|| missing(what, id))
    }
}

fn updated(changed: usize, what: &str, id: &str) -> Res<()> {
    match changed {
        0 => Err(missing(what, id)),
        _ => Ok(()),
    }
}

// ---- rows ---------------------------------------------------------------------------------

/// A §5.3 enum column, stored as its `as_str()`.
struct Text<T>(T);

impl<T: FromStr<Err = AppError>> FromSql for Text<T> {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        v.as_str()?.parse().map(Text).map_err(FromSqlError::other)
    }
}

/// A JSON column.
struct Json<T>(T);

impl<T: DeserializeOwned> FromSql for Json<T> {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        serde_json::from_str(v.as_str()?)
            .map(Json)
            .map_err(FromSqlError::other)
    }
}

fn text<T: FromStr<Err = AppError>>(r: &Row<'_>, col: &str) -> rusqlite::Result<T> {
    Ok(r.get::<_, Text<T>>(col)?.0)
}

fn opt_text<T: FromStr<Err = AppError>>(r: &Row<'_>, col: &str) -> rusqlite::Result<Option<T>> {
    Ok(r.get::<_, Option<Text<T>>>(col)?.map(|t| t.0))
}

fn all<T>(
    c: &Connection,
    sql: &str,
    params: impl Params,
    f: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Res<Vec<T>> {
    Ok(c.prepare_cached(sql)?
        .query_map(params, f)?
        .collect::<rusqlite::Result<_>>()?)
}

fn project_row(r: &Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: r.get("id")?,
        name: r.get("name")?,
        repo_path: r.get("repo_path")?,
        default_target_branch: r.get("default_target_branch")?,
        default_permission_mode: text(r, "default_permission_mode")?,
        default_model: r.get("default_model")?,
        config_policy: text(r, "config_policy")?,
        trusted_fingerprint: r.get("trusted_fingerprint")?,
        allow_bypass: r.get("allow_bypass")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn task_row(r: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        title: r.get("title")?,
        description: r.get("description")?,
        status: text(r, "status")?,
        position: r.get("position")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn attempt_row(r: &Row<'_>) -> rusqlite::Result<AttemptRow> {
    Ok(AttemptRow {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        state: text(r, "state")?,
        branch: r.get("branch")?,
        target_branch: r.get("target_branch")?,
        base_commit: r.get("base_commit")?,
        worktree_path: r.get("worktree_path")?,
        worktree_state: text(r, "worktree_state")?,
        session_id: r.get("session_id")?,
        session_started: r.get("session_started")?,
        permission_mode: text(r, "permission_mode")?,
        model: r.get("model")?,
        effort: opt_text(r, "effort")?,
        allow_rules: r.get::<_, Json<_>>("allow_rules")?.0,
        merge_commit: r.get("merge_commit")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        closed_at: r.get("closed_at")?,
    })
}

fn process_row(r: &Row<'_>) -> rusqlite::Result<ProcessRow> {
    Ok(ProcessRow {
        id: r.get("id")?,
        attempt_id: r.get("attempt_id")?,
        seq: r.get("seq")?,
        prompt: r.get("prompt")?,
        permission_mode: text(r, "permission_mode")?,
        session_id: r.get("session_id")?,
        resumed: r.get("resumed")?,
        status: text(r, "status")?,
        stop_reason: opt_text(r, "stop_reason")?,
        error: r.get("error")?,
        cli_version: r.get("cli_version")?,
        argv_json: r.get("argv_json")?,
        pid: r.get("pid")?,
        app_instance_id: r.get("app_instance_id")?,
        exit_code: r.get("exit_code")?,
        result_subtype: r.get("result_subtype")?,
        is_error: r.get("is_error")?,
        cost_usd_estimate: r.get("cost_usd_estimate")?,
        num_turns: r.get("num_turns")?,
        duration_ms: r.get::<_, Option<i64>>("duration_ms")?.map(|d| d as u64),
        head_before: r.get("head_before")?,
        head_after: r.get("head_after")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
    })
}

fn entry_row(r: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(r.get::<_, Json<Entry>>("payload")?.0)
}

/// A task with its active (else most recent) attempt and that attempt's latest process.
const CARD_SELECT: &str = "SELECT t.*,
        a.id AS attempt_id, a.state AS attempt_state, a.branch, a.worktree_state,
        p.status AS last_status, p.stop_reason AS last_stop_reason,
        EXISTS (SELECT 1 FROM processes r WHERE r.attempt_id = a.id AND r.status = 'running')
            AS running
    FROM tasks t
    LEFT JOIN attempts a ON a.id = (SELECT x.id FROM attempts x WHERE x.task_id = t.id
        ORDER BY x.state = 'active' DESC, x.created_at DESC, x.rowid DESC LIMIT 1)
    LEFT JOIN processes p ON p.id = (SELECT y.id FROM processes y WHERE y.attempt_id = a.id
        ORDER BY y.seq DESC LIMIT 1)";

fn card_row(r: &Row<'_>) -> rusqlite::Result<TaskCard> {
    Ok(TaskCard {
        task: task_row(r)?,
        attempt_id: r.get("attempt_id")?,
        attempt_state: opt_text(r, "attempt_state")?,
        branch: r.get("branch")?,
        running: r.get("running")?,
        pending_approvals: 0,
        last_status: opt_text(r, "last_status")?,
        last_stop_reason: opt_text(r, "last_stop_reason")?,
        worktree_state: opt_text(r, "worktree_state")?,
    })
}

// ---- queries shared by several methods ------------------------------------------------------

fn get_project(c: &Connection, id: &str) -> Res<ProjectRow> {
    c.query_row("SELECT * FROM projects WHERE id = ?1", [id], project_row)
        .or_missing("Progetto", id)
}

fn get_task(c: &Connection, id: &str) -> Res<Task> {
    c.query_row("SELECT * FROM tasks WHERE id = ?1", [id], task_row)
        .or_missing("Task", id)
}

fn get_attempt(c: &Connection, id: &str) -> Res<AttemptRow> {
    c.query_row("SELECT * FROM attempts WHERE id = ?1", [id], attempt_row)
        .or_missing("Attempt", id)
}

fn insert_process(c: &Connection, p: &ProcessRow) -> Res<()> {
    c.execute(
        "INSERT INTO processes (id, attempt_id, seq, prompt, permission_mode, session_id, resumed,
            status, stop_reason, error, cli_version, argv_json, pid, app_instance_id, exit_code,
            result_subtype, is_error, cost_usd_estimate, num_turns, duration_ms, head_before,
            head_after, started_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22, ?23, ?24)",
        params![
            p.id,
            p.attempt_id,
            p.seq,
            p.prompt,
            p.permission_mode.as_str(),
            p.session_id,
            p.resumed,
            p.status.as_str(),
            p.stop_reason.map(StopReason::as_str),
            p.error,
            p.cli_version,
            p.argv_json,
            p.pid,
            p.app_instance_id,
            p.exit_code,
            p.result_subtype,
            p.is_error,
            p.cost_usd_estimate,
            p.num_turns,
            p.duration_ms.map(|d| d as i64),
            p.head_before,
            p.head_after,
            p.started_at,
            p.finished_at,
        ],
    )?;
    Ok(())
}

/// `max + GAP` of the column, ignoring task `id` itself; `GAP` for an empty column.
fn end_of_column(c: &Connection, project_id: &str, status: TaskStatus, id: &str) -> Res<f64> {
    let max: Option<f64> = c.query_row(
        "SELECT MAX(position) FROM tasks WHERE project_id = ?1 AND status = ?2 AND id <> ?3",
        params![project_id, status.as_str(), id],
        |r| r.get(0),
    )?;
    Ok(max.unwrap_or(0.0) + POSITION_GAP)
}

fn place(c: &Connection, id: &str, status: TaskStatus, position: f64, now: Millis) -> Res<()> {
    c.execute(
        "UPDATE tasks SET status = ?2, position = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, status.as_str(), position, now],
    )?;
    Ok(())
}

/// Lifecycle move (spec §5.4): the task goes to the end of column `to` if its status is one
/// of `from` and differs from `to`.
fn transition(
    c: &Connection,
    task_id: &str,
    from: &[TaskStatus],
    to: TaskStatus,
    now: Millis,
) -> Res<()> {
    let task = get_task(c, task_id)?;
    if task.status == to || !from.contains(&task.status) {
        return Ok(());
    }
    let position = end_of_column(c, &task.project_id, to, task_id)?;
    place(c, task_id, to, position, now)
}

/// Rewrites the column's positions as `k * GAP` with `id` right before `before`; returns the
/// position of `id` (the caller writes it together with the new status).
fn renumber(
    c: &Connection,
    project_id: &str,
    status: TaskStatus,
    id: &str,
    before: &str,
) -> Res<f64> {
    let mut ids: Vec<String> = all(
        c,
        "SELECT id FROM tasks WHERE project_id = ?1 AND status = ?2 AND id <> ?3
         ORDER BY position",
        params![project_id, status.as_str(), id],
        |r| r.get(0),
    )?;
    let at = ids.iter().position(|t| t == before).unwrap_or(ids.len());
    ids.insert(at, id.to_owned());
    let mut stmt = c.prepare_cached("UPDATE tasks SET position = ?2 WHERE id = ?1")?;
    for (k, task_id) in ids.iter().enumerate() {
        stmt.execute(params![task_id, (k + 1) as f64 * POSITION_GAP])?;
    }
    Ok((at + 1) as f64 * POSITION_GAP)
}

fn upsert(c: &Connection, attempt_id: &str, entries: &[Entry]) -> Res<()> {
    let mut stmt = c.prepare_cached(
        "INSERT INTO entries (attempt_id, idx, rev, process_id, kind, payload, ts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (attempt_id, idx) DO UPDATE SET rev = excluded.rev,
            process_id = excluded.process_id, kind = excluded.kind, payload = excluded.payload,
            ts = excluded.ts
         WHERE excluded.rev > entries.rev",
    )?;
    for e in entries {
        stmt.execute(params![
            attempt_id,
            e.idx,
            e.rev,
            e.process_id,
            e.body.kind(),
            serde_json::to_string(e)?,
            e.ts
        ])?;
    }
    Ok(())
}

/// The newest `limit` entries with `idx < before`, ascending; one extra row tells `has_more`.
fn page(c: &Connection, attempt_id: &str, before: i64, limit: u32) -> Res<EntryPage> {
    let limit = limit.min(MAX_ENTRY_PAGE);
    let mut entries = all(
        c,
        "SELECT payload FROM entries WHERE attempt_id = ?1 AND idx < ?2
         ORDER BY idx DESC LIMIT ?3",
        params![attempt_id, before, limit + 1],
        entry_row,
    )?;
    let has_more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    entries.reverse();
    Ok(EntryPage { entries, has_more })
}
