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

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atm_types::{
    AddProjectReq, AddProjectRes, AppError, AttemptIdReq, AttemptState, AttemptView, AuthState,
    BranchList, BranchStatus, Changed, ConfigPolicy, CreateTaskReq, DiffResult, EVENT_CHANGED,
    EVENT_ENV_CHANGED, EntryPage, EnvStatus, ErrorCode, GetEntriesReq, GetEnvReq, Id, IdReq,
    MergeAttemptReq, MergeOutcome, Millis, MoveTaskReq, OpenAttemptReq, OpenLoginTerminalReq,
    OpenTarget, OpenUrlReq, PermissionMode, ProcessInfo, Project, ProjectIdReq, RespondApprovalReq,
    SendFollowUpReq, SetProjectSecurityReq, Settings, StartAttemptReq, TaskCard, TaskDetail,
    TaskStatus, UnsubscribeTranscriptReq, UpdateProjectReq, UpdateTaskReq, WorktreeState,
};
use tokio::sync::OwnedMutexGuard;

use crate::claude::{ChildEnv, Discovered};
use crate::db::{AttemptCtx, AttemptRow, Db, ProjectRow};
use crate::git::Git;
use crate::live::Live;
use crate::runner::{StopCause, StopTimings, TurnHandle};

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
    /// Debug builds only (the E2E of M4): every `/usr/bin/open` the core would run (Terminal for
    /// the login, Finder, editor, URLs) is appended to this file as a JSON array of its
    /// arguments instead. Ignored in release builds.
    pub open_log: Option<PathBuf>,
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

/// Discovery, versions and `auth status` are reused for this long (spec §6.3 `get_env`).
const ENV_TTL: Duration = Duration::from_secs(60);
/// The login script outlives the UI's polling (at most 10 minutes, spec §7.10).
const LOGIN_SCRIPT_TTL: Duration = Duration::from_secs(600);

/// The application service. One method per IPC command (spec §6.3), all returning typed
/// [`AppError`]s; `pick_repo_folder` and the native confirmations live in the shell.
pub struct Core {
    inner: Arc<Inner>,
}

/// State shared by the services and the turn tasks (`runner`).
struct Inner {
    config: CoreConfig,
    notify: Notify,
    app_instance_id: Id,
    db: Arc<Db>,
    live: Arc<Live>,
    /// Running turns by attempt (slots are reserved before the spawn): the live registry
    /// merged into cards and views, and the concurrency count.
    turns: Mutex<HashMap<Id, Arc<TurnHandle>>>,
    /// Usage-limit text while new turns are paused (spec §7.9).
    paused: Mutex<Option<String>>,
    /// Set by `shutdown`: no new turn starts.
    closing: AtomicBool,
    tools: tokio::sync::Mutex<Option<Arc<Tools>>>,
    probe: tokio::sync::Mutex<Option<Arc<Probe>>>,
    /// Per-attempt, per-task and per-repo mutexes (spec §8.1: always attempt → repo).
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Login-shell `PATH` and the git found on it: imported once, re-read by `get_env{force}`.
struct Tools {
    path: OsString,
    git: Git,
}

/// The cached part of [`EnvStatus`].
struct Probe {
    at: tokio::time::Instant,
    claude: Option<Discovered>,
    auth: AuthState,
    git_version: Result<String, String>,
}

impl Core {
    /// Builds the core with a fresh `app_instance_id`. M3 opens the DB here. Runs inside a
    /// tokio runtime context (the shell's `block_on`, `#[tokio::test]`), so it may spawn.
    /// Errors: `Db`, `Io`, with a message naming the path (the shell shows it and exits).
    pub fn new(config: CoreConfig, notify: Notify) -> Result<Core, AppError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.data_dir)
            .map_err(|e| AppError::io(format!("{}: {e}", config.data_dir.display())))?;
        let db = Db::open(&config.data_dir.join("atm.sqlite3"))?;
        Ok(Core {
            inner: Arc::new(Inner {
                config,
                notify,
                app_instance_id: new_id(),
                db: Arc::new(db),
                live: Arc::new(Live::new()),
                turns: Mutex::default(),
                paused: Mutex::default(),
                closing: AtomicBool::new(false),
                tools: tokio::sync::Mutex::default(),
                probe: tokio::sync::Mutex::default(),
                locks: Mutex::default(),
            }),
        })
    }

    /// Runs before the UI loads data (spec §7.9): orphan recovery (`Db::mark_orphans`,
    /// verified kill, cancelled tools, auto-commit), worktree reconciliation, first env check.
    pub async fn startup(&self) -> Result<(), AppError> {
        self.inner.recover_orphans().await?;
        self.inner.reconcile_worktrees().await;
        self.inner.probe(false).await;
        self.inner.emit_changed(None, None);
        Ok(())
    }

    /// Stops every running turn in parallel with [`runner::StopTimings::SHUTDOWN`] and
    /// finalizes them (auto-commit included) within `deadline`. Best effort, never fails.
    pub async fn shutdown(&self, deadline: Duration) {
        let s = &self.inner;
        // Set under the registry lock that `reserve` checks it under: every slot is either in
        // this snapshot or refused.
        let turns: Vec<Arc<TurnHandle>> = {
            let turns = guard(&s.turns);
            s.closing.store(true, Ordering::SeqCst);
            turns.values().cloned().collect()
        };
        for turn in &turns {
            turn.stop(StopCause::Shutdown, StopTimings::SHUTDOWN);
        }
        let all = async {
            for turn in &turns {
                turn.finished().await;
            }
        };
        let _ = tokio::time::timeout(deadline, all).await;
    }

    /// Stops every transcript forwarder (page reload, spec §6.5).
    pub fn drop_subscriptions(&self) {
        self.inner.live.drop_all();
    }

    /// Live transcript forwarders (`Live::forwarder_count`): 0 after a reload (selftest, M4).
    pub fn forwarder_count(&self) -> usize {
        self.inner.live.forwarder_count()
    }

    /// One project, `trusted` computed from this project's fingerprint only (the shell's
    /// confirmation check). Errors: `NotFound`.
    pub async fn project(&self, id: &str) -> Result<Project, AppError> {
        let row = self.inner.db.project(id)?;
        Ok(self.inner.project_view(&row).await)
    }

    /// Env status. Discovery, versions and `auth status` are cached 60 s (`force` re-reads
    /// PATH, `claude --version`, `auth status`, git); `paused`, `running`, `max_running` and
    /// `checked_at` are filled at every call. Emits `env_changed` when the pause, the running
    /// count or `max_running` change.
    pub async fn get_env(&self, req: GetEnvReq) -> Result<EnvStatus, AppError> {
        Ok(self.inner.env_status(req.force).await)
    }

    /// Writes the login script and opens Terminal (spec §7.10). Errors: `ClaudeNotFound`.
    pub async fn open_login_terminal(&self, req: OpenLoginTerminalReq) -> Result<(), AppError> {
        let s = &self.inner;
        let probe = s.probe(false).await;
        let claude = probe.claude.as_ref().ok_or_else(claude_not_found)?;
        let script = claude::write_login_script(&s.config.cache_dir, &claude.path, req.method)?;
        let args = [
            "-a".to_owned(),
            "Terminal".into(),
            script.display().to_string(),
        ];
        if !s.record_open(&args).await? {
            claude::open_login_terminal(&script).await?;
        }
        tokio::spawn(async move {
            tokio::time::sleep(LOGIN_SCRIPT_TTL).await;
            let _ = tokio::fs::remove_file(script).await;
        });
        Ok(())
    }

    /// Clears the usage-limit pause; emits `env_changed`.
    pub async fn resume_agents(&self) -> Result<EnvStatus, AppError> {
        *guard(&self.inner.paused) = None;
        Ok(self.inner.emit_env().await)
    }

    pub async fn get_settings(&self) -> Result<Settings, AppError> {
        self.inner.db.settings()
    }

    /// Validates (`max_running` 1..=6, …) and saves. The shell has already obtained the
    /// native confirmation when `allow_env_api_key` is being enabled. Errors: `Invalid`.
    pub async fn update_settings(&self, req: Settings) -> Result<Settings, AppError> {
        let s = &self.inner;
        if !(1..=6).contains(&req.max_running) {
            return Err(AppError::invalid("Gli agenti in parallelo vanno da 1 a 6"));
        }
        let req = Settings {
            claude_path_override: non_empty(req.claude_path_override),
            default_model: non_empty(req.default_model),
            worktree_root: req.worktree_root.trim().to_owned(),
            editor_app: req.editor_app.trim().to_owned(),
            ..req
        };
        if req.worktree_root.is_empty() || req.editor_app.is_empty() {
            return Err(AppError::invalid(
                "Cartella dei worktree ed editor sono obbligatori",
            ));
        }
        let root = git::resolve_worktree_root(&req.worktree_root, &s.home())?;
        for project in s.db.projects()? {
            let repo = Path::new(&project.repo_path);
            if root.starts_with(repo) || repo.starts_with(&root) {
                return Err(AppError::invalid(format!(
                    "La cartella dei worktree non può stare dentro il progetto {} né contenerlo",
                    project.name
                )));
            }
        }
        let old = s.db.settings()?;
        s.db.save_settings(&req)?;
        let rediscover = old.claude_path_override != req.claude_path_override
            || old.allow_env_api_key != req.allow_env_api_key;
        if rediscover {
            *s.probe.lock().await = None;
        }
        if rediscover || old.max_running != req.max_running {
            s.emit_env().await;
        }
        Ok(req)
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, AppError> {
        let mut projects = Vec::new();
        for row in self.inner.db.projects()? {
            projects.push(self.inner.project_view(&row).await);
        }
        Ok(projects)
    }

    /// Validates the repo (spec §8.3) and inserts it with its current branch as default
    /// target. Errors: `Invalid`, `Conflict` (already added).
    pub async fn add_project(&self, req: AddProjectReq) -> Result<AddProjectRes, AppError> {
        let s = &self.inner;
        let settings = s.db.settings()?;
        let root = git::resolve_worktree_root(&settings.worktree_root, &s.home())?;
        let git = s.git().await;
        let info = git.validate_repo(Path::new(req.path.trim()), &root).await?;
        let repo_path = info.toplevel.to_string_lossy().into_owned();
        if s.db.projects()?.iter().any(|p| p.repo_path == repo_path) {
            return Err(AppError::conflict("Questo repository è già stato aggiunto"));
        }
        let target = match info.current_branch {
            Some(branch) => branch,
            None => git
                .list_branches(&info.toplevel)
                .await?
                .branches
                .into_iter()
                .next()
                .ok_or_else(|| AppError::invalid("Il repository non ha branch locali"))?,
        };
        let name: String = info
            .toplevel
            .file_name()
            .map_or_else(|| repo_path.clone(), |n| n.to_string_lossy().into_owned())
            .chars()
            .take(200)
            .collect();
        let now = now_ms();
        let row = ProjectRow {
            id: new_id(),
            name,
            repo_path,
            default_target_branch: target,
            default_permission_mode: PermissionMode::AcceptEdits,
            default_model: None,
            config_policy: ConfigPolicy::Isolated,
            trusted_fingerprint: None,
            allow_bypass: false,
            created_at: now,
            updated_at: now,
        };
        s.db.insert_project(&row)?;
        s.emit_changed(None, None);
        Ok(AddProjectRes {
            project: row.to_project(false),
            warnings: info.warnings,
        })
    }

    /// Errors: `NotFound`, `Invalid` (target branch not in `refs/heads`).
    pub async fn update_project(&self, req: UpdateProjectReq) -> Result<Project, AppError> {
        let s = &self.inner;
        let row = s.db.project(&req.id)?;
        let name = req.name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(AppError::invalid("Il nome va da 1 a 200 caratteri"));
        }
        check_bypass(req.default_permission_mode, &row)?;
        s.git()
            .await
            .branch_tip(Path::new(&row.repo_path), &req.default_target_branch)
            .await
            .map_err(|e| match e.code {
                ErrorCode::NotFound => AppError::invalid(e.message),
                _ => e,
            })?;
        let req = UpdateProjectReq {
            name: name.to_owned(),
            default_model: non_empty(req.default_model),
            ..req
        };
        let row = s.db.update_project(&req, now_ms())?;
        s.emit_changed(None, None);
        Ok(s.project_view(&row).await)
    }

    /// M6: Trusted stores the fingerprint of the main checkout. The shell has already
    /// obtained the native confirmation when raising the effective level (Trusted while not
    /// `Project::trusted`, or enabling bypass).
    pub async fn set_project_security(
        &self,
        req: SetProjectSecurityReq,
    ) -> Result<Project, AppError> {
        let s = &self.inner;
        let row = s.db.project(&req.id)?;
        let fingerprint = match req.config_policy {
            ConfigPolicy::Trusted => {
                // Its fingerprint would read `~/.claude`, which the app never reads (§10.1).
                if s.is_home(Path::new(&row.repo_path)) {
                    return Err(AppError::invalid(
                        "Un repository nella cartella home non può essere considerato attendibile",
                    ));
                }
                Some(git::config_fingerprint(Path::new(&row.repo_path)).await?)
            }
            ConfigPolicy::Isolated => None,
        };
        let now = now_ms();
        let mut row = s.db.set_project_security(
            &req.id,
            req.config_policy,
            req.allow_bypass,
            fingerprint.as_deref(),
            now,
        )?;
        if !row.allow_bypass && row.default_permission_mode == PermissionMode::BypassPermissions {
            let update = UpdateProjectReq {
                id: row.id.clone(),
                name: row.name.clone(),
                default_target_branch: row.default_target_branch.clone(),
                default_permission_mode: PermissionMode::AcceptEdits,
                default_model: row.default_model.clone(),
            };
            row = s.db.update_project(&update, now)?;
        }
        s.emit_changed(None, None);
        Ok(s.project_view(&row).await)
    }

    /// Errors: `Busy` with running turns. Snapshots and removes the worktrees, keeps branches.
    pub async fn remove_project(&self, req: IdReq) -> Result<(), AppError> {
        let s = &self.inner;
        let project = s.db.project(&req.id)?;
        let busy = || AppError::busy("Ferma gli agenti del progetto prima di rimuoverlo");
        let running =
            |turns: &HashMap<Id, Arc<TurnHandle>>| turns.values().any(|t| t.project_id == req.id);
        if running(&guard(&s.turns)) {
            return Err(busy());
        }
        let git = s.git().await;
        for attempt in s.db.attempts_with_worktree(Some(&req.id))? {
            let _attempt = s.lock(attempt_key(&attempt.id)).await;
            // A follow-up may have launched a turn while this waited for the lock.
            if s.turn(&attempt.id).is_some() {
                return Err(busy());
            }
            let _repo = s.lock(repo_key(&project.repo_path)).await;
            tolerate_missing(s.remove_worktree(&git, &project.repo_path, &attempt).await)?;
        }
        {
            // `start_attempt` reserves its slot before writing any row: checked together with
            // the deletion, a new turn is either seen here or finds no project.
            let turns = guard(&s.turns);
            if running(&turns) {
                return Err(busy());
            }
            s.db.delete_project(&req.id)?;
        }
        s.emit_changed(None, None);
        Ok(())
    }

    pub async fn list_branches(&self, req: ProjectIdReq) -> Result<BranchList, AppError> {
        let project = self.inner.db.project(&req.project_id)?;
        let git = self.inner.git().await;
        git.list_branches(Path::new(&project.repo_path)).await
    }

    /// DB board merged with the live registry (`running`, `pending_approvals`).
    pub async fn get_board(&self, req: ProjectIdReq) -> Result<Vec<TaskCard>, AppError> {
        let s = &self.inner;
        s.db.project(&req.project_id)?;
        let mut cards = s.db.board(&req.project_id)?;
        for card in &mut cards {
            s.merge_live(card);
        }
        Ok(cards)
    }

    pub async fn create_task(&self, req: CreateTaskReq) -> Result<TaskCard, AppError> {
        let s = &self.inner;
        let req = CreateTaskReq {
            title: req.title.trim().to_owned(),
            ..req
        };
        let task = s.db.insert_task(&new_id(), &req, now_ms())?;
        s.emit_changed(Some(&task.project_id), Some(&task.id));
        s.card(&task.id)
    }

    pub async fn update_task(&self, req: UpdateTaskReq) -> Result<TaskCard, AppError> {
        let s = &self.inner;
        let req = UpdateTaskReq {
            title: req.title.trim().to_owned(),
            ..req
        };
        let task = s.db.update_task(&req, now_ms())?;
        s.emit_changed(Some(&task.project_id), Some(&task.id));
        s.card(&task.id)
    }

    /// Errors: `Busy` if running and the destination is done or cancelled.
    pub async fn move_task(&self, req: MoveTaskReq) -> Result<(), AppError> {
        let s = &self.inner;
        let task = s.db.task(&req.id)?;
        if matches!(req.status, TaskStatus::Done | TaskStatus::Cancelled)
            && s.task_running(&task.id)
        {
            return Err(AppError::busy(
                "Il task è in esecuzione: ferma l'agente prima di spostarlo",
            ));
        }
        s.db.move_task(&req.id, req.status, req.before_id.as_deref(), now_ms())?;
        s.emit_changed(Some(&task.project_id), Some(&task.id));
        Ok(())
    }

    /// Errors: `Busy` if running. Discards the active attempt first.
    pub async fn delete_task(&self, req: IdReq) -> Result<(), AppError> {
        let s = &self.inner;
        let task = s.db.task(&req.id)?;
        let busy = || AppError::busy("Il task è in esecuzione: ferma l'agente prima di eliminarlo");
        // Serialized with `start_attempt`; a follow-up is caught under its attempt's lock, and
        // none can start on a removed worktree.
        let _task = s.lock(task_key(&task.id)).await;
        if s.task_running(&task.id) {
            return Err(busy());
        }
        let project = s.db.project(&task.project_id)?;
        let git = s.git().await;
        for attempt in s.db.task_attempts(&task.id)? {
            if attempt.worktree_state == WorktreeState::Removed {
                continue;
            }
            let _attempt = s.lock(attempt_key(&attempt.id)).await;
            if s.turn(&attempt.id).is_some() {
                return Err(busy());
            }
            let _repo = s.lock(repo_key(&project.repo_path)).await;
            tolerate_missing(s.remove_worktree(&git, &project.repo_path, &attempt).await)?;
        }
        s.db.delete_task(&task.id)?;
        s.emit_changed(Some(&task.project_id), Some(&task.id));
        Ok(())
    }

    pub async fn get_task_detail(&self, req: IdReq) -> Result<TaskDetail, AppError> {
        let s = &self.inner;
        let task = s.db.task(&req.id)?;
        let (active, closed): (Vec<AttemptRow>, Vec<AttemptRow>) =
            s.db.task_attempts(&task.id)?
                .into_iter()
                .partition(|a| a.state == AttemptState::Active);
        let active = active.into_iter().next();
        let processes = match &active {
            Some(a) => s.db.attempt_processes(&a.id)?,
            None => Vec::new(),
        };
        Ok(TaskDetail {
            task,
            attempt: active.map(|a| s.attempt_view(&a)),
            processes: processes.iter().map(|p| p.info()).collect(),
            closed_attempts: closed.iter().map(|a| a.view(false, 0)).collect(),
        })
    }

    /// Preflight (spec §7.7 step 2), worktree under the repo lock, rows, then spawns the
    /// first turn in the background. Errors: `ClaudeNotFound`, `NotLoggedIn`, `UsageLimited`,
    /// `ConcurrencyLimit`, `Conflict` (active attempt exists), `Invalid` (bypass without the
    /// project's `allow_bypass`), `Git`.
    pub async fn start_attempt(&self, req: StartAttemptReq) -> Result<AttemptView, AppError> {
        let s = &self.inner;
        let task = s.db.task(&req.task_id)?;
        let project = s.db.project(&task.project_id)?;
        check_bypass(req.permission_mode, &project)?;
        let _task = s.lock(task_key(&task.id)).await;
        if s.db.active_attempt(&task.id)?.is_some() {
            return Err(AppError::conflict("Il task ha già un tentativo attivo"));
        }
        let settings = s.db.settings()?;
        let attempt_id = new_id();
        let preflight = s
            .preflight(&attempt_id, &task.id, &project.id, &settings)
            .await?;
        let git = s.git().await;
        let repo = Path::new(&project.repo_path);
        let base = git
            .branch_tip(repo, &req.target_branch)
            .await
            .map_err(|e| match e.code {
                ErrorCode::NotFound => AppError::invalid(e.message),
                _ => e,
            })?;
        let root = git::resolve_worktree_root(&settings.worktree_root, &s.home())?;
        let (branch, worktree) = {
            let _repo = s.lock(repo_key(&project.repo_path)).await;
            let branch = git.unique_branch(repo, &attempt_id, &task.title).await?;
            let path = git::worktree_path(&root, &attempt_id);
            let worktree = git
                .add_worktree(repo, &path, &branch, &base, &attempt_id)
                .await?;
            (branch, worktree)
        };
        // The security settings as they are now, not as before the preflight and `worktree
        // add`: a bypass revoked meanwhile must not reach the argv.
        let fresh = s.db.project(&project.id).and_then(|p| {
            check_bypass(req.permission_mode, &p)?;
            Ok(p)
        });
        let project = match fresh {
            Ok(project) => project,
            Err(e) => {
                s.undo_worktree(&git, &project.repo_path, &worktree, &branch)
                    .await;
                return Err(e);
            }
        };
        let now = now_ms();
        let attempt = AttemptRow {
            id: attempt_id,
            task_id: task.id.clone(),
            state: AttemptState::Active,
            branch,
            target_branch: req.target_branch,
            base_commit: base.clone(),
            worktree_path: worktree.to_string_lossy().into_owned(),
            worktree_state: WorktreeState::Present,
            session_id: new_id(),
            session_started: false,
            permission_mode: req.permission_mode,
            model: non_empty(req.model)
                .or_else(|| project.default_model.clone())
                .or_else(|| settings.default_model.clone()),
            effort: req.effort,
            allow_rules: Vec::new(),
            merge_commit: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
        };
        let prompt = first_prompt(&task.title, &task.description);
        let ctx = AttemptCtx {
            attempt,
            task,
            project,
        };
        let plan = s
            .plan_turn(runner::TurnRequest {
                ctx: &ctx,
                settings: &settings,
                preflight: &preflight,
                seq: 1,
                prompt: prompt.clone(),
                stdin_prompt: prompt,
                mode: req.permission_mode,
                resume: false,
                head_before: Some(base),
                next_idx: 0,
            })
            .await;
        if let Err(e) = s.db.begin_attempt(&ctx.attempt, &plan.process, now) {
            let (repo, branch) = (&ctx.project.repo_path, &ctx.attempt.branch);
            s.undo_worktree(&git, repo, &worktree, branch).await;
            return Err(e);
        }
        s.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        let view = ctx.attempt.view(true, 0);
        s.launch(preflight, plan).await;
        Ok(view)
    }

    /// `permission_mode` applies to this turn only (`processes.permission_mode`); the
    /// attempt's mode is unchanged. Errors: `Busy` (turn running), `WorktreeMissing`,
    /// `BranchMismatch`, plus the preflight errors of `start_attempt`.
    pub async fn send_follow_up(&self, req: SendFollowUpReq) -> Result<ProcessInfo, AppError> {
        let s = &self.inner;
        if req.prompt.trim().is_empty() {
            return Err(AppError::invalid("Il messaggio è vuoto"));
        }
        let _attempt = s.lock(attempt_key(&req.attempt_id)).await;
        let mut ctx = s.db.attempt_ctx(&req.attempt_id)?;
        if ctx.attempt.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è chiuso"));
        }
        let mode = req.permission_mode.unwrap_or(ctx.attempt.permission_mode);
        check_bypass(mode, &ctx.project)?;
        if s.turn(&ctx.attempt.id).is_some() {
            return Err(busy_turn());
        }
        let settings = s.db.settings()?;
        let preflight = s
            .preflight(&ctx.attempt.id, &ctx.task.id, &ctx.project.id, &settings)
            .await?;
        let git = s.git().await;
        s.check_worktree(&git, &ctx).await?;
        // The security settings as they are after the preflight (see `start_attempt`).
        ctx.project = s.db.project(&ctx.project.id)?;
        check_bypass(mode, &ctx.project)?;
        let worktree = Path::new(&ctx.attempt.worktree_path);
        let head_before = git.head(worktree).await.ok();
        let resume = ctx.attempt.session_started && !req.fresh_session;
        if !resume {
            // A session that never started (or a failed resume) gets a new id (spec §7.9).
            ctx.attempt.session_id = new_id();
            ctx.attempt.session_started = false;
            s.db.set_session(&ctx.attempt.id, &ctx.attempt.session_id, false, now_ms())?;
        }
        let stdin_prompt = if req.fresh_session {
            let log = git
                .log_oneline(worktree, &ctx.attempt.base_commit)
                .await
                .unwrap_or_default();
            fresh_prompt(&ctx.task.title, &ctx.task.description, &log, &req.prompt)
        } else {
            req.prompt.clone()
        };
        let plan = s
            .plan_turn(runner::TurnRequest {
                ctx: &ctx,
                settings: &settings,
                preflight: &preflight,
                seq: s.db.next_seq(&ctx.attempt.id)?,
                prompt: req.prompt,
                stdin_prompt,
                mode,
                resume,
                head_before,
                next_idx: s.db.next_entry_idx(&ctx.attempt.id)?,
            })
            .await;
        s.db.begin_turn(&plan.process, now_ms())?;
        s.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        let info = plan.process.info();
        s.launch(preflight, plan).await;
        Ok(info)
    }

    /// Returns at once; the stop sequence runs in the background.
    pub async fn stop_attempt(&self, req: AttemptIdReq) -> Result<(), AppError> {
        self.inner.db.attempt(&req.attempt_id)?;
        if let Some(turn) = self.inner.turn(&req.attempt_id) {
            turn.stop(StopCause::User, StopTimings::NORMAL);
        }
        Ok(())
    }

    /// Errors: `NotFound` (approval no longer pending).
    pub async fn respond_approval(&self, req: RespondApprovalReq) -> Result<(), AppError> {
        let turn = self
            .inner
            .turn(&req.attempt_id)
            .ok_or_else(runner::not_pending)?;
        turn.respond(&req.approval_id, req.decision).await
    }

    /// Registers a forwarder into `sink` and returns its id at once (spec §6.5). An unknown
    /// attempt gets an empty `Snapshot` (`has_more: false`), not an error.
    pub async fn subscribe_transcript(
        &self,
        req: AttemptIdReq,
        sink: TranscriptSink,
    ) -> Result<Id, AppError> {
        let s = &self.inner;
        s.live.subscribe(Arc::clone(&s.db), &req.attempt_id, sink)
    }

    pub async fn unsubscribe_transcript(
        &self,
        req: UnsubscribeTranscriptReq,
    ) -> Result<(), AppError> {
        self.inner.live.unsubscribe(&req.subscription_id);
        Ok(())
    }

    /// Errors: `Invalid` if `limit` > 200.
    pub async fn get_entries(&self, req: GetEntriesReq) -> Result<EntryPage, AppError> {
        if req.limit > db::MAX_ENTRY_PAGE {
            return Err(AppError::invalid(format!(
                "limit oltre {}",
                db::MAX_ENTRY_PAGE
            )));
        }
        self.inner
            .db
            .entries_before(&req.attempt_id, req.before_idx, req.limit)
    }

    pub async fn get_diff(&self, req: AttemptIdReq) -> Result<DiffResult, AppError> {
        let s = &self.inner;
        let _attempt = s.lock(attempt_key(&req.attempt_id)).await;
        let ctx = s.db.attempt_ctx(&req.attempt_id)?;
        let worktree = s.present_worktree(&ctx)?;
        let git = s.git().await;
        let diff = git
            .snapshot_diff(worktree, &ctx.attempt.target_branch)
            .await;
        diff.map_err(|e| s.missing_on(e, &ctx))
    }

    pub async fn get_branch_status(&self, req: AttemptIdReq) -> Result<BranchStatus, AppError> {
        let s = &self.inner;
        let ctx = s.db.attempt_ctx(&req.attempt_id)?;
        let worktree = s.present_worktree(&ctx)?;
        let git = s.git().await;
        let a = &ctx.attempt;
        let mut status = git
            .branch_status(
                Path::new(&ctx.project.repo_path),
                worktree,
                &a.branch,
                &a.target_branch,
            )
            .await
            .map_err(|e| s.missing_on(e, &ctx))?;
        if s.turn(&a.id).is_some() {
            status.merge_blocked = Some("Un turno dell'agente è in corso".into());
        }
        Ok(status)
    }

    /// Spec §8.7. Errors: `Busy`, `WorktreeMissing`, `BranchMismatch`,
    /// `TargetCheckoutDirty`, `GitIdentityMissing`, `Git`.
    pub async fn merge_attempt(&self, req: MergeAttemptReq) -> Result<MergeOutcome, AppError> {
        let s = &self.inner;
        let _attempt = s.lock(attempt_key(&req.attempt_id)).await;
        let ctx = s.db.attempt_ctx(&req.attempt_id)?;
        let a = &ctx.attempt;
        if a.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è chiuso"));
        }
        if s.turn(&a.id).is_some() {
            return Err(AppError::busy(
                "Un turno è in corso: fermalo prima del merge",
            ));
        }
        let worktree = s.present_worktree(&ctx)?;
        let git = s.git().await;
        let repo = Path::new(&ctx.project.repo_path);
        let _repo = s.lock(repo_key(&ctx.project.repo_path)).await;
        let outcome = git
            .squash_merge(
                repo,
                worktree,
                &a.branch,
                &a.target_branch,
                &req.message,
                &a.id,
            )
            .await
            .map_err(|e| s.missing_on(e, &ctx))?;
        let MergeOutcome::Merged {
            commit, strategy, ..
        } = outcome
        else {
            return Ok(outcome);
        };
        s.db.finish_merge(&a.id, &commit, now_ms())?;
        let mut cleanup_warning = None;
        if s.db.settings()?.remove_worktree_after_merge
            && let Err(e) = s.remove_worktree(&git, &ctx.project.repo_path, a).await
        {
            cleanup_warning = Some(e.message);
        }
        s.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        Ok(MergeOutcome::Merged {
            commit,
            strategy,
            cleanup_warning,
        })
    }

    /// Stops the turn, snapshot commit, removes the worktree (spec §5.4).
    pub async fn discard_attempt(&self, req: AttemptIdReq) -> Result<(), AppError> {
        let s = &self.inner;
        if s.db.attempt(&req.attempt_id)?.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è già chiuso"));
        }
        if let Some(turn) = s.turn(&req.attempt_id) {
            turn.stop(StopCause::User, StopTimings::NORMAL);
            turn.finished().await;
        }
        let _attempt = s.lock(attempt_key(&req.attempt_id)).await;
        if s.turn(&req.attempt_id).is_some() {
            return Err(busy_turn());
        }
        let ctx = s.db.attempt_ctx(&req.attempt_id)?;
        // A merge or another discard may have closed it while this waited.
        if ctx.attempt.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è già chiuso"));
        }
        if ctx.attempt.worktree_state != WorktreeState::Removed {
            let git = s.git().await;
            let _repo = s.lock(repo_key(&ctx.project.repo_path)).await;
            tolerate_missing(
                s.remove_worktree(&git, &ctx.project.repo_path, &ctx.attempt)
                    .await,
            )?;
        }
        s.db.finish_discard(&ctx.attempt.id, now_ms())?;
        s.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        Ok(())
    }

    /// Errors: `Invalid` unless the attempt is merged and the branch is `atm/…`.
    pub async fn delete_branch(&self, req: AttemptIdReq) -> Result<(), AppError> {
        let s = &self.inner;
        let ctx = s.db.attempt_ctx(&req.attempt_id)?;
        if ctx.attempt.state != AttemptState::Merged || !ctx.attempt.branch.starts_with("atm/") {
            return Err(AppError::invalid(
                "Si possono eliminare solo i branch atm/… dei tentativi mergiati",
            ));
        }
        let git = s.git().await;
        let _attempt = s.lock(attempt_key(&ctx.attempt.id)).await;
        let _repo = s.lock(repo_key(&ctx.project.repo_path)).await;
        git.delete_branch(Path::new(&ctx.project.repo_path), &ctx.attempt.branch)
            .await?;
        s.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        Ok(())
    }

    /// `open <wt>`, `open -a Terminal <wt>` or `open -a <editor_app> <wt>`; path from the DB.
    pub async fn open_attempt(&self, req: OpenAttemptReq) -> Result<(), AppError> {
        let s = &self.inner;
        let ctx = s.db.attempt_ctx(&req.attempt_id)?;
        let worktree = s.present_worktree(&ctx)?.to_string_lossy().into_owned();
        let args = match req.target {
            OpenTarget::Finder => vec![worktree],
            OpenTarget::Terminal => vec!["-a".into(), "Terminal".into(), worktree],
            OpenTarget::Editor => vec!["-a".into(), s.db.settings()?.editor_app, worktree],
        };
        s.open(&args).await
    }

    /// `open <url>`. Errors: `Invalid` unless `http(s)`.
    pub async fn open_url(&self, req: OpenUrlReq) -> Result<(), AppError> {
        let url = req.url.trim();
        let lower = url.to_ascii_lowercase();
        let web = lower.starts_with("https://") || lower.starts_with("http://");
        if !web || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(AppError::invalid(
                "Si possono aprire solo indirizzi http(s)",
            ));
        }
        self.inner.open(&[url.to_owned()]).await
    }
}

impl Inner {
    fn emit(&self, event: AppEvent) {
        (self.notify)(event);
    }

    fn emit_changed(&self, project_id: Option<&str>, task_id: Option<&str>) {
        self.emit(AppEvent::Changed(Changed {
            project_id: project_id.map(str::to_owned),
            task_id: task_id.map(str::to_owned),
        }));
    }

    /// Emits and returns the current [`EnvStatus`].
    async fn emit_env(&self) -> EnvStatus {
        let env = self.env_status(false).await;
        self.emit(AppEvent::EnvChanged(env.clone()));
        env
    }

    /// The app's environment plus `extra_env` (later entries win).
    fn base_env(&self) -> Vec<(OsString, OsString)> {
        std::env::vars_os()
            .chain(self.config.extra_env.iter().cloned())
            .collect()
    }

    fn home(&self) -> PathBuf {
        self.base_env()
            .into_iter()
            .rev()
            .find(|(k, _)| k == "HOME")
            .map_or_else(|| PathBuf::from("/"), |(_, v)| PathBuf::from(v))
    }

    fn child_env(&self, tools: &Tools, settings: &Settings) -> ChildEnv {
        ChildEnv::new(self.base_env(), &tools.path, settings.allow_env_api_key)
    }

    async fn tools(&self, force: bool) -> Arc<Tools> {
        let mut cache = self.tools.lock().await;
        if !force && let Some(tools) = cache.as_ref() {
            return Arc::clone(tools);
        }
        let path = match &self.config.path_env {
            Some(path) => path.clone(),
            None => claude::login_shell_path().await,
        };
        let git =
            Git::new(git::find_git(&path), path.clone()).with_env(self.config.extra_env.clone());
        let tools = Arc::new(Tools { path, git });
        *cache = Some(Arc::clone(&tools));
        tools
    }

    async fn git(&self) -> Git {
        self.tools(false).await.git.clone()
    }

    /// Discovery, `claude --version`, `auth status` and `git --version`, cached [`ENV_TTL`].
    async fn probe(&self, force: bool) -> Arc<Probe> {
        let mut cache = self.probe.lock().await;
        if !force
            && let Some(probe) = cache.as_ref()
            && probe.at.elapsed() < ENV_TTL
        {
            return Arc::clone(probe);
        }
        let tools = self.tools(force).await;
        let settings = self.db.settings().unwrap_or_default();
        let env = self.child_env(&tools, &settings);
        let claude = match &self.config.claude_path {
            Some(path) => claude::probe_version(path, &env)
                .await
                .ok()
                .map(|version| Discovered {
                    path: path.clone(),
                    version,
                }),
            None => {
                let override_path = settings.claude_path_override.as_deref().map(Path::new);
                claude::discover(override_path, &env).await
            }
        };
        let auth = match &claude {
            Some(found) => claude::auth_status(&found.path, &env).await,
            None => AuthState::Unknown {
                reason: "Claude Code non trovato".into(),
            },
        };
        let probe = Arc::new(Probe {
            at: tokio::time::Instant::now(),
            claude,
            auth,
            git_version: tools.git.version().await.map_err(|e| e.message),
        });
        *cache = Some(Arc::clone(&probe));
        probe
    }

    async fn env_status(&self, force: bool) -> EnvStatus {
        let probe = self.probe(force).await;
        let base = self.base_env();
        EnvStatus {
            claude: claude::claude_info(probe.claude.as_ref()),
            auth: probe.auth.clone(),
            git_version: probe.git_version.clone().ok(),
            api_key_in_env: claude::api_key_in_env(&base),
            cloud_provider_env: claude::cloud_provider_env(&base),
            paused: guard(&self.paused).clone(),
            running: guard(&self.turns).len() as u32,
            max_running: self.db.settings().unwrap_or_default().max_running,
            problems: probe.git_version.clone().err().into_iter().collect(),
            checked_at: now_ms(),
        }
    }

    async fn lock(&self, key: String) -> OwnedMutexGuard<()> {
        let mutex = {
            let mut locks = guard(&self.locks);
            // Holders and waiters keep a clone: a key only the map owns is unused.
            locks.retain(|_, m| Arc::strong_count(m) > 1);
            Arc::clone(locks.entry(key).or_default())
        };
        mutex.lock_owned().await
    }

    /// `dir` is the home directory (`HOME` as given or canonical).
    fn is_home(&self, dir: &Path) -> bool {
        let home = self.home();
        dir == home || std::fs::canonicalize(&home).is_ok_and(|h| h == dir)
    }

    /// Removes the worktree and branch of an attempt whose rows were never written.
    async fn undo_worktree(&self, git: &Git, repo: &str, worktree: &Path, branch: &str) {
        let _repo = self.lock(repo_key(repo)).await;
        let _ = git.remove_worktree(Path::new(repo), worktree).await;
        let _ = git.delete_branch(Path::new(repo), branch).await;
    }

    /// With `CoreConfig::open_log` (debug builds): appends `args` to it and returns `true`, the
    /// `open` must not run. Errors: `Io`.
    async fn record_open(&self, args: &[String]) -> Result<bool, AppError> {
        use tokio::io::AsyncWriteExt as _;
        let Some(log) = self
            .config
            .open_log
            .as_ref()
            .filter(|_| cfg!(debug_assertions))
        else {
            return Ok(false);
        };
        let line = serde_json::to_string(args)? + "\n";
        let io_err = |e: std::io::Error| AppError::io(format!("{}: {e}", log.display()));
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .await
            .map_err(io_err)?;
        // One write per line, as the recorded calls may overlap.
        file.write_all(line.as_bytes()).await.map_err(io_err)?;
        // tokio's `File` finishes a write in the background: done before the caller reads.
        file.flush().await.map_err(io_err)?;
        Ok(true)
    }

    /// `/usr/bin/open <args>` (argv, no shell). An app it launches inherits this environment
    /// (open(1)), so credentials, nesting and git variables are removed as for every child.
    async fn open(&self, args: &[String]) -> Result<(), AppError> {
        if self.record_open(args).await? {
            return Ok(());
        }
        let allow_api_key = self.db.settings()?.allow_env_api_key;
        let scrubbed = |k: &str| {
            claude::CLAUDE_NESTING_VARS.contains(&k)
                || git::is_scrubbed_git_var(k)
                || (!allow_api_key && claude::API_KEY_VARS.contains(&k))
        };
        let env = self
            .base_env()
            .into_iter()
            .filter(|(k, _)| !k.to_str().is_some_and(scrubbed));
        let status = tokio::process::Command::new("/usr/bin/open")
            .env_clear()
            .envs(env)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(AppError::io(format!("open non riuscito ({status})")))
        }
    }

    fn turn(&self, attempt_id: &str) -> Option<Arc<TurnHandle>> {
        guard(&self.turns).get(attempt_id).cloned()
    }

    fn task_running(&self, task_id: &str) -> bool {
        guard(&self.turns).values().any(|t| t.task_id == task_id)
    }

    /// `running` and `pending_approvals` from the live registry.
    fn live_state(&self, attempt_id: &str) -> (bool, u32) {
        self.turn(attempt_id)
            .map_or((false, 0), |t| (true, t.pending_count()))
    }

    fn merge_live(&self, card: &mut TaskCard) {
        (card.running, card.pending_approvals) = card
            .attempt_id
            .as_deref()
            .map_or((false, 0), |id| self.live_state(id));
    }

    fn card(&self, task_id: &str) -> Result<TaskCard, AppError> {
        let mut card = self.db.task_card(task_id)?;
        self.merge_live(&mut card);
        Ok(card)
    }

    fn attempt_view(&self, attempt: &AttemptRow) -> AttemptView {
        let (running, pending) = self.live_state(&attempt.id);
        attempt.view(running, pending)
    }

    /// `trusted` = policy Trusted and the approved fingerprint still matches the checkout.
    async fn project_view(&self, row: &ProjectRow) -> Project {
        let trusted = row.config_policy == ConfigPolicy::Trusted
            && self
                .fingerprint_matches(row, Path::new(&row.repo_path))
                .await;
        row.to_project(trusted)
    }

    async fn fingerprint_matches(&self, row: &ProjectRow, dir: &Path) -> bool {
        match (&row.trusted_fingerprint, git::config_fingerprint(dir).await) {
            (Some(approved), Ok(current)) => *approved == current,
            _ => false,
        }
    }

    /// The worktree of `ctx`, which the DB says is present. Errors: `WorktreeMissing`.
    fn present_worktree<'a>(&self, ctx: &'a AttemptCtx) -> Result<&'a Path, AppError> {
        match ctx.attempt.worktree_state {
            WorktreeState::Present => Ok(Path::new(&ctx.attempt.worktree_path)),
            _ => Err(worktree_missing(&ctx.attempt.worktree_path)),
        }
    }

    /// Worktree present with its `.git`, HEAD on the attempt's branch (spec §7.7 step 2).
    /// Errors: `WorktreeMissing` (and marks it missing), `BranchMismatch`, `Git`.
    async fn check_worktree(&self, git: &Git, ctx: &AttemptCtx) -> Result<(), AppError> {
        let worktree = self.present_worktree(ctx)?;
        if !worktree.join(".git").exists() {
            return Err(self.missing_on(worktree_missing(&ctx.attempt.worktree_path), ctx));
        }
        let expected = format!("refs/heads/{}", ctx.attempt.branch);
        match git.head_ref(worktree).await? {
            Some(head) if head == expected => Ok(()),
            _ => Err(AppError::new(
                ErrorCode::BranchMismatch,
                format!(
                    "Il worktree non è più sul branch {}: riportalo sul branch e riprova",
                    ctx.attempt.branch
                ),
            )),
        }
    }

    /// Records a `WorktreeMissing` error in the DB (the worktree lost its `.git`); returns `e`.
    fn missing_on(&self, e: AppError, ctx: &AttemptCtx) -> AppError {
        if e.code == ErrorCode::WorktreeMissing {
            self.mark_missing(&ctx.attempt.id);
            self.emit_changed(Some(&ctx.project.id), Some(&ctx.task.id));
        }
        e
    }

    fn mark_missing(&self, attempt_id: &str) {
        if let Err(e) = self
            .db
            .set_worktree_state(attempt_id, WorktreeState::Missing, now_ms())
        {
            eprintln!("attempt {attempt_id}: worktree missing not recorded: {e}");
        }
    }

    /// Snapshot commit and removal (spec §8.4), then `worktree_state = removed`. The caller
    /// holds the attempt and repo locks. `WorktreeMissing` is recorded before returning it.
    async fn remove_worktree(
        &self,
        git: &Git,
        repo: &str,
        attempt: &AttemptRow,
    ) -> Result<(), AppError> {
        match git
            .remove_worktree(Path::new(repo), Path::new(&attempt.worktree_path))
            .await
        {
            Ok(()) => self
                .db
                .set_worktree_state(&attempt.id, WorktreeState::Removed, now_ms()),
            Err(e) => {
                if e.code == ErrorCode::WorktreeMissing {
                    self.mark_missing(&attempt.id);
                }
                Err(e)
            }
        }
    }

    /// Worktree states against `worktree list` (spec §8.4); a listed worktree without its
    /// `.git` is missing too. Errors are logged: startup goes on.
    async fn reconcile_worktrees(&self) {
        let projects = match self.db.projects() {
            Ok(projects) => projects,
            Err(e) => return eprintln!("reconcile: {e}"),
        };
        let git = self.git().await;
        for project in projects {
            let attempts = match self.db.attempts_with_worktree(Some(&project.id)) {
                Ok(attempts) if !attempts.is_empty() => attempts,
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("reconcile {}: {e}", project.repo_path);
                    continue;
                }
            };
            let paths: Vec<PathBuf> = attempts
                .iter()
                .map(|a| PathBuf::from(&a.worktree_path))
                .collect();
            let states = match git
                .reconcile_worktrees(Path::new(&project.repo_path), &paths)
                .await
            {
                Ok(states) => states,
                Err(e) => {
                    eprintln!("reconcile {}: {e}", project.repo_path);
                    continue;
                }
            };
            for ((attempt, path), mut state) in attempts.iter().zip(&paths).zip(states) {
                if state == WorktreeState::Present && !path.join(".git").exists() {
                    state = WorktreeState::Missing;
                }
                if state != attempt.worktree_state
                    && let Err(e) = self.db.set_worktree_state(&attempt.id, state, now_ms())
                {
                    eprintln!("reconcile {}: {e}", attempt.id);
                }
            }
        }
    }
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn task_key(task_id: &str) -> String {
    format!("task:{task_id}")
}

fn attempt_key(attempt_id: &str) -> String {
    format!("attempt:{attempt_id}")
}

fn repo_key(repo_path: &str) -> String {
    format!("repo:{repo_path}")
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn check_bypass(mode: PermissionMode, project: &ProjectRow) -> Result<(), AppError> {
    if mode == PermissionMode::BypassPermissions && !project.allow_bypass {
        return Err(AppError::invalid(
            "La modalità Autonoma richiede di abilitarla nella sicurezza del progetto",
        ));
    }
    Ok(())
}

fn claude_not_found() -> AppError {
    AppError::new(
        ErrorCode::ClaudeNotFound,
        "Claude Code non trovato: installalo o indica il percorso nelle impostazioni",
    )
}

fn busy_turn() -> AppError {
    AppError::busy("Un turno dell'agente è già in esecuzione")
}

fn worktree_missing(path: &str) -> AppError {
    AppError::new(
        ErrorCode::WorktreeMissing,
        format!("Il worktree {path} non è più disponibile: si può solo scartare il tentativo"),
    )
}

/// Removal of a worktree that lost its `.git` is left to the user; the rest goes on.
fn tolerate_missing(result: Result<(), AppError>) -> Result<(), AppError> {
    match result {
        Err(e) if e.code != ErrorCode::WorktreeMissing => Err(e),
        _ => Ok(()),
    }
}

/// First turn (spec §7.4 step 2): `# {title}\n\n{description}`.
fn first_prompt(title: &str, description: &str) -> String {
    format!("# {title}\n\n{description}").trim_end().to_owned()
}

/// `fresh_session` (spec §7.4 step 2): the task, the attempt's commits and the user's text.
fn fresh_prompt(title: &str, description: &str, log: &str, text: &str) -> String {
    let mut prompt = first_prompt(title, description);
    if !log.trim().is_empty() {
        prompt.push_str("\n\nCommits already made on this branch:\n");
        prompt.push_str(log.trim_end());
    }
    prompt.push_str("\n\n");
    prompt.push_str(text);
    prompt
}
