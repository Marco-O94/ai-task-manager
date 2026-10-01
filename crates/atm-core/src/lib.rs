//! Application core (spec §3, §4): the services behind every IPC command, the turn runner,
//! the live transcript fan-out, git and SQLite. No Tauri types: the shell adapts [`Notify`]
//! to `app.emit` and [`TranscriptSink`] to a `Channel`.
//!
//! Module owners after M1 (spec §11.2): `db` M2-DB, `git` M2-GIT, `claude`/`wire`/`normalize`
//! M2-CLAUDE, `lib`/`runner`/`live` M3-CORE; `attachments` CORE-RUNNER (feature round of
//! 2026-09-29). Public signatures are frozen; owners only add.

pub mod attachments;
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use atm_types::{
    AddProjectReq, AddProjectRes, AddTaskAttachmentsReq, AppError, Attachment, AttemptIdReq,
    AttemptState, AttemptView, AuthState, BranchList, BranchStatus, Changed, ConfigPolicy,
    CreateTaskReq, DiffResult, EVENT_CHANGED, EVENT_ENV_CHANGED, EntryPage, EnvStatus, ErrorCode,
    GetEntriesReq, GetEnvReq, Id, IdReq, MAX_ATTACHMENTS_PER_TASK, MAX_PROJECT_DESCRIPTION,
    MAX_SUBAGENTS, MODEL_ALIASES, MergeAttemptReq, MergeOutcome, Millis, MoveTaskReq,
    OpenAttemptReq, OpenLoginTerminalReq, OpenTarget, OpenUrlReq, PermissionMode, PickedFile,
    ProcessInfo, Project, ProjectIdReq, ProjectOverview, RespondApprovalReq, SendFollowUpReq,
    SetProjectSecurityReq, Settings, StartAttemptReq, Task, TaskCard, TaskDetail, TaskStatus,
    UnsubscribeTranscriptReq, UpdateProjectReq, UpdateTaskReq, WorktreeState,
};
use tokio::sync::OwnedMutexGuard;

use crate::claude::{ChildEnv, Discovered};
use crate::db::{AttemptCtx, AttemptRow, Db, ProjectRow};
use crate::git::{ConfigSnapshot, Git};
use crate::live::Live;
use crate::runner::{StopCause, StopTimings, TurnHandle};

pub use db::SecurityState;
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
    /// Per-attempt, per-task and per-repo mutexes (spec §8.1: always attempt → repo), plus
    /// one per project for its security and one for the settings (M6).
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// `HOME` (the app's, or `extra_env`'s) and its canonical form, read once (never on a
    /// tokio worker again).
    home: PathBuf,
    home_canonical: Option<PathBuf>,
    /// Configurations of commits by `(repo, commit id)`: a commit never changes, and a
    /// project's trust is computed on every read (at most [`MAX_COMMIT_CONFIGS`]). An
    /// `Invalid` answer (a limit, a link, …) is as final as a snapshot and kept too: a crafted
    /// tip is walked once, not on every read of a Trusted project.
    commit_configs: Mutex<HashMap<(String, String), Result<ConfigSnapshot, AppError>>>,
    /// Files the picker returned, by one-use token (spec F5).
    picks: Mutex<attachments::Staging>,
}

/// Size of [`Inner::commit_configs`] past which it starts over.
const MAX_COMMIT_CONFIGS: usize = 64;

/// `Busy` of a task deleted while its agent runs.
const REMOVE_BUSY: &str = "Il task è in esecuzione: ferma l'agente prima di eliminarlo";

/// Login-shell `PATH` and the git found on it: imported once, re-read by `get_env{force}`.
struct Tools {
    path: OsString,
    git: Git,
}

/// What the shell's security confirmation is built from, read once ([`Core::security_snapshot`]).
#[derive(Debug, Clone)]
pub struct SecuritySnapshot {
    /// `trusted` and `trust_error` computed from `current`.
    pub project: Project,
    /// The state a change applies to ([`Core::apply_project_security`]'s `expected`).
    pub stored: SecurityState,
    /// The configuration committed at the tip of the project's default target branch (what a
    /// Trusted approval approves, spec §8.9); `Err` if it cannot be approved: it cannot be
    /// fingerprinted, the branch cannot be read, the repository is the home directory, or it
    /// would bill the agents outside the subscription.
    pub current: Result<ConfigSnapshot, AppError>,
    /// Where `current` was read; `None` if the branch could not be read.
    pub base: Option<ConfigBase>,
}

/// A commit whose configuration is approved or compared: a branch and its tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigBase {
    pub branch: String,
    /// Full hex id.
    pub commit: String,
}

impl ConfigBase {
    /// The first 7 hex digits of the commit.
    pub fn short(&self) -> &str {
        self.commit.get(..7).unwrap_or(&self.commit)
    }
}

impl SecuritySnapshot {
    /// The fingerprint `req` would approve: `Some` when it asks for Trusted while the project
    /// is not trusted now (Isolated, or its approved configuration changed), `None` when there
    /// is nothing to approve. Errors: the configuration cannot be fingerprinted (the reason
    /// the approval is refused, before any dialog).
    pub fn approval(&self, req: &SetProjectSecurityReq) -> Result<Option<String>, AppError> {
        if req.config_policy != ConfigPolicy::Trusted || self.project.trusted {
            return Ok(None);
        }
        self.current
            .as_ref()
            .map(|s| Some(s.fingerprint.clone()))
            .map_err(Clone::clone)
    }
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
        let home = std::env::vars_os()
            .chain(config.extra_env.iter().cloned())
            .filter(|(k, _)| k == "HOME")
            .last()
            .map_or_else(|| PathBuf::from("/"), |(_, v)| PathBuf::from(v));
        let home_canonical = std::fs::canonicalize(&home).ok();
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
                home,
                home_canonical,
                commit_configs: Mutex::default(),
                picks: Mutex::default(),
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

    /// One project, `trusted` and `trust_error` computed from this project's fingerprint only.
    /// Errors: `NotFound`.
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

    /// The git every call runs: `git` on the imported `PATH`, else `/usr/bin/git` (spec §8.1).
    /// The shell logs it next to the env status (an app launched from the Finder has launchd's
    /// `PATH`, not the Terminal's).
    pub async fn git_path(&self) -> PathBuf {
        self.inner.tools(false).await.git.bin().to_path_buf()
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

    /// [`Core::update_settings_checked`] against the settings stored now: for callers that
    /// confirm nothing (tests).
    pub async fn update_settings(&self, req: Settings) -> Result<Settings, AppError> {
        let current = self.inner.db.settings()?;
        self.update_settings_checked(req, &current).await
    }

    /// Validates (`max_running` 1..=6, …) and saves, under the settings lock and only if
    /// `allow_env_api_key` and `claude_path_override` are still those of `expected` (what the
    /// shell read before its confirmation), else `Conflict`. The shell has already obtained
    /// the native confirmation when `allow_env_api_key` is being enabled or the override
    /// changed. A new override must be an absolute path to an existing file outside every
    /// project and the worktree root (M6: a repository must not pick the CLI the app runs).
    /// Errors: `Invalid`, `Conflict`.
    pub async fn update_settings_checked(
        &self,
        req: Settings,
        expected: &Settings,
    ) -> Result<Settings, AppError> {
        let s = &self.inner;
        let _settings = s.lock("settings".to_owned()).await;
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
        let old = s.db.settings()?;
        if (old.allow_env_api_key, &old.claude_path_override)
            != (
                expected.allow_env_api_key,
                &non_empty(expected.claude_path_override.clone()),
            )
        {
            return Err(AppError::conflict(
                "Le impostazioni sono cambiate nel frattempo: riaprile e riprova",
            ));
        }
        let root = git::resolve_worktree_root(&req.worktree_root, &s.home())?;
        let projects = s.db.projects()?;
        for project in &projects {
            let repo = Path::new(&project.repo_path);
            if root.starts_with(repo) || repo.starts_with(&root) {
                return Err(AppError::invalid(format!(
                    "La cartella dei worktree non può stare dentro il progetto {} né contenerlo",
                    project.name
                )));
            }
        }
        if let Some(path) = &req.claude_path_override
            && old.claude_path_override.as_ref() != Some(path)
        {
            check_claude_override(Path::new(path), &projects, &root).await?;
        }
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
            .filter(|c| !is_hidden_char(*c))
            .take(200)
            .collect();
        let name = if name.trim().is_empty() {
            "progetto".to_owned()
        } else {
            name
        };
        let now = now_ms();
        let row = ProjectRow {
            id: new_id(),
            name,
            description: String::new(),
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

    /// The description is trimmed. Errors: `NotFound`, `Invalid` (target branch not in
    /// `refs/heads`, description over [`MAX_PROJECT_DESCRIPTION`] characters).
    pub async fn update_project(&self, req: UpdateProjectReq) -> Result<Project, AppError> {
        let s = &self.inner;
        let row = s.db.project(&req.id)?;
        let name = req.name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(AppError::invalid("Il nome va da 1 a 200 caratteri"));
        }
        // It is shown in native text (spec §10.2): no line breaks nor direction overrides.
        if name.chars().any(is_hidden_char) {
            return Err(AppError::invalid(
                "Il nome non può contenere a capo, caratteri di controllo o di direzione",
            ));
        }
        let description = req.description.trim();
        if description.chars().count() > MAX_PROJECT_DESCRIPTION {
            return Err(AppError::invalid(format!(
                "La descrizione può avere al massimo {MAX_PROJECT_DESCRIPTION} caratteri"
            )));
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
            description: description.to_owned(),
            ..req
        };
        let row = s.db.update_project(&req, now_ms())?;
        s.emit_changed(None, None);
        Ok(s.project_view(&row).await)
    }

    /// What the shell builds its confirmation from, read once (M6): the project with `trusted`
    /// computed from `current`, its stored security state and the configuration committed at
    /// the tip of its default target branch now, the one worktrees are created from (spec
    /// §8.9). Errors: `NotFound`.
    pub async fn security_snapshot(&self, id: &str) -> Result<SecuritySnapshot, AppError> {
        let s = &self.inner;
        let row = s.db.project(id)?;
        let (base, current) = s.target_config(&row).await;
        Ok(SecuritySnapshot {
            project: s.view_with(&row, &current),
            stored: row.security(),
            current,
            base,
        })
    }

    /// [`Core::security_snapshot`] then [`Core::apply_project_security`] at once: for callers
    /// that confirm nothing (tests). The shell asks for its native confirmation in between.
    pub async fn set_project_security(
        &self,
        req: SetProjectSecurityReq,
    ) -> Result<Project, AppError> {
        let snapshot = self.security_snapshot(&req.id).await?;
        let approve = snapshot.approval(&req)?;
        self.apply_project_security(req, &snapshot.stored, approve.as_deref())
            .await
    }

    /// M6 (spec §8.9): stores policy and bypass opt-in, under the project's lock and only if
    /// the stored state is still `expected` (what the user saw), else `Conflict`. Trusted
    /// stores `approve`, the fingerprint the confirmation described
    /// ([`SecuritySnapshot::approval`]), once a fresh computation on the target branch's tip
    /// still gives it (else `Conflict`: the configuration changed while the dialog was open;
    /// a new tip with the same configuration is the same approval); without `approve`
    /// it keeps the approved fingerprint (the policy stays Trusted, nothing is re-approved);
    /// Isolated clears it. Revoking `allow_bypass` resets a default mode of Autonomo to
    /// Auto-edit in the same statement; revoking the bypass or Trusted stops the project's
    /// running turns that use it, with a Notice. The shell has already obtained the native
    /// confirmation when raising the effective level. Errors: `NotFound`, `Conflict`,
    /// `Invalid` (Trusted without an approval to keep, the home directory, a configuration
    /// that cannot be fingerprinted or that would bill outside the subscription, a target
    /// branch that cannot be read), `Io`.
    pub async fn apply_project_security(
        &self,
        req: SetProjectSecurityReq,
        expected: &SecurityState,
        approve: Option<&str>,
    ) -> Result<Project, AppError> {
        let s = &self.inner;
        let _project = s.lock(project_key(&req.id)).await;
        let row = s.db.project(&req.id)?;
        let fingerprint = match (req.config_policy, approve) {
            (ConfigPolicy::Isolated, _) => None,
            (ConfigPolicy::Trusted, Some(approved)) => {
                let now = s.target_config(&row).await.1?;
                if now.fingerprint != approved {
                    return Err(AppError::conflict(
                        "La configurazione Claude del repository è cambiata mentre confermavi: \
                         riprova per vedere e approvare quella attuale",
                    ));
                }
                Some(approved.to_owned())
            }
            (ConfigPolicy::Trusted, None) => match expected {
                SecurityState {
                    config_policy: ConfigPolicy::Trusted,
                    trusted_fingerprint: Some(kept),
                    ..
                } => Some(kept.clone()),
                _ => {
                    return Err(AppError::invalid(
                        "La configurazione Attendibile va approvata con una conferma",
                    ));
                }
            },
        };
        let row = s.db.set_project_security(
            &req.id,
            expected,
            req.config_policy,
            req.allow_bypass,
            fingerprint.as_deref(),
            now_ms(),
        )?;
        s.stop_revoked_turns(&req.id, expected, &row);
        s.emit_changed(None, None);
        Ok(s.project_view(&row).await)
    }

    /// Errors: `Busy` with running turns. Snapshots and removes the worktrees, keeps branches;
    /// once the rows are gone, removes the attachments and the attempts' raw logs.
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
        let attempt_ids = {
            // `start_attempt` reserves its slot before writing any row: checked together with
            // the deletion, a new turn is either seen here or finds no project.
            let turns = guard(&s.turns);
            if running(&turns) {
                return Err(busy());
            }
            let attempt_ids = s.db.project_attempt_ids(&req.id)?;
            s.db.delete_project(&req.id)?;
            attempt_ids
        };
        let folder = attachments::project_dir(&s.config.data_dir, &req.id);
        s.remove_files(folder, &attempt_ids).await;
        s.emit_changed(None, None);
        Ok(())
    }

    pub async fn list_branches(&self, req: ProjectIdReq) -> Result<BranchList, AppError> {
        let project = self.inner.db.project(&req.project_id)?;
        let git = self.inner.git().await;
        git.list_branches(Path::new(&project.repo_path)).await
    }

    /// The overview page (spec F3): what the agents find in the repository, read by
    /// [`git::overview::read`] from the commit at the tip of the default target branch (where
    /// worktrees start, what a Trusted approval approves), with the records of that commit's
    /// configuration (cached) and whether the agents load it (Trusted and trusted). Errors:
    /// `NotFound`, `Invalid` (the repository is the home directory, whose `.claude` is the
    /// user's own and never read, spec §10.1), the branch's (it cannot be read), the reader's.
    pub async fn get_project_overview(
        &self,
        req: ProjectIdReq,
    ) -> Result<ProjectOverview, AppError> {
        let s = &self.inner;
        let row = s.db.project(&req.project_id)?;
        let repo = Path::new(&row.repo_path);
        if s.is_home(repo) {
            return Err(home_refusal());
        }
        let git = s.git().await;
        let branch = &row.default_target_branch;
        let commit = git.branch_tip(repo, branch).await.map_err(|e| {
            AppError::new(
                e.code,
                format!(
                    "Il branch target predefinito {branch} non si può leggere ({})",
                    e.message
                ),
            )
        })?;
        let base = ConfigBase {
            branch: branch.clone(),
            commit,
        };
        // The configuration as committed: its records list `.claude/` even when billing keys
        // keep it from being approved (then the agents do not load it).
        let config = s.commit_config(&git, repo, &base.commit).await;
        let approvable = config
            .clone()
            .and_then(|c| billing_refusal(&c, &base).map_or(Ok(c), Err));
        let source = git::overview::Source {
            repo,
            project_id: &row.id,
            branch: &base.branch,
            commit: &base.commit,
            agents_load_config: s.view_with(&row, &approvable).trusted,
            records: config.as_ref().map_or(&[], |c| c.records.as_slice()),
        };
        git::overview::read(&git, &source).await
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
        // Serialized with the parent's delete: a sub-task never outlives it unseen.
        let _parent = match &req.parent_id {
            Some(parent_id) => Some(s.lock(task_key(parent_id)).await),
            None => None,
        };
        if let Some(parent_id) = &req.parent_id {
            check_parent(&s.db, &req.project_id, parent_id)?;
        }
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

    /// Errors: `Busy` if the task or one of its sub-tasks is running, nothing removed. The
    /// sub-tasks go first, each with the same cleanup as the task ([`Inner::remove_task`]).
    pub async fn delete_task(&self, req: IdReq) -> Result<(), AppError> {
        let s = &self.inner;
        let task = s.db.task(&req.id)?;
        let project = s.db.project(&task.project_id)?;
        let git = s.git().await;
        // Every task's lock across the whole cascade, parent first: none of them starts, and
        // no sub-task is created (`create_task` takes the parent's) until it is over.
        let _task = s.lock(task_key(&task.id)).await;
        let children = s.db.task_children(&task.id)?;
        let mut child_locks = Vec::with_capacity(children.len());
        for child in &children {
            child_locks.push(s.lock(task_key(child)).await);
        }
        if s.task_running(&task.id) {
            return Err(AppError::busy(REMOVE_BUSY));
        }
        if children.iter().any(|id| s.task_running(id)) {
            return Err(AppError::busy("Un sotto task è in esecuzione"));
        }
        for child in &children {
            s.remove_task(&git, &project, child).await?;
        }
        s.remove_task(&git, &project, &task.id).await?;
        if let Some(parent_id) = &task.parent_id {
            s.emit_changed(Some(&task.project_id), Some(parent_id));
        }
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
        let attachments: Vec<Attachment> =
            s.db.task_attachments(&task.id)?
                .iter()
                .map(|a| attachments::view(&s.config.data_dir, &task.project_id, a))
                .collect();
        let mut subtasks = s.db.subtask_cards(&task.id)?;
        for card in &mut subtasks {
            s.merge_live(card);
        }
        Ok(TaskDetail {
            task,
            attempt: active.map(|a| s.attempt_view(&a)),
            processes: processes.iter().map(|p| p.info()).collect(),
            closed_attempts: closed.iter().map(|a| a.view(false, 0)).collect(),
            attachments,
            subtasks,
        })
    }

    /// Stages files the native picker returned (the shell's `pick_attachment_files`, spec
    /// F5): each one checked ([`attachments::check_pick`]: outside the user's Claude Code
    /// configuration, `~/.ssh`, `~/.aws`, the Keychain and the app's own folders, a regular
    /// file within the size limit, a usable name), then given a one-use token that expires
    /// ([`attachments::Staging`]); the webview only ever gets the tokens. All or nothing.
    /// Errors: `Invalid` (more than `MAX_ATTACHMENTS_PER_TASK` files, a file refused, named).
    pub async fn stage_picks(&self, paths: Vec<PathBuf>) -> Result<Vec<PickedFile>, AppError> {
        let s = &self.inner;
        if paths.len() > MAX_ATTACHMENTS_PER_TASK {
            return Err(AppError::invalid(format!(
                "Si possono scegliere al massimo {MAX_ATTACHMENTS_PER_TASK} file alla volta"
            )));
        }
        let home = s.home();
        let config_dir = s.claude_config_dir();
        let (data_dir, cache_dir) = (s.config.data_dir.clone(), s.config.cache_dir.clone());
        let picked = tokio::task::spawn_blocking(move || {
            let deny =
                attachments::DenyList::new(&home, config_dir.as_deref(), &data_dir, &cache_dir);
            paths
                .iter()
                .map(|path| attachments::check_pick(path, &deny))
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| AppError::internal(format!("controllo dei file: {e}")))??;
        Ok(guard(&s.picks).stage(picked, Instant::now()))
    }

    /// Copies the staged files of `req.tokens` into the task's folder
    /// ([`attachments::copy_picks`]) and records them in one transaction that counts the
    /// task's attachments again; on any error the copies are removed. Every token is used up,
    /// whatever the outcome. Runs under the task's lock (other changes of its attachments, its
    /// deletion). Emits `changed`. Errors: `NotFound` (task), `Invalid` (unknown, used or
    /// expired token, past `MAX_ATTACHMENTS_PER_TASK`, a file that changed since it was
    /// picked), `Io`.
    pub async fn add_task_attachments(
        &self,
        req: AddTaskAttachmentsReq,
    ) -> Result<Vec<Attachment>, AppError> {
        let s = &self.inner;
        let _task = s.lock(task_key(&req.task_id)).await;
        let redeemed: Vec<Option<attachments::Picked>> = {
            let mut picks = guard(&s.picks);
            let now = Instant::now();
            req.tokens.iter().map(|t| picks.redeem(t, now)).collect()
        };
        let task = s.db.task(&req.task_id)?;
        let picks: Vec<attachments::Picked> = redeemed
            .into_iter()
            .collect::<Option<_>>()
            .ok_or_else(|| AppError::invalid("File non più disponibile: sceglilo di nuovo"))?;
        if picks.is_empty() {
            return Ok(Vec::new());
        }
        let dir = attachments::task_dir(&s.config.data_dir, &task.project_id, &task.id);
        let rows = {
            let (dir, task_id) = (dir.clone(), task.id.clone());
            tokio::task::spawn_blocking(move || {
                attachments::copy_picks(&picks, &dir, &task_id, now_ms())
            })
            .await
            .map_err(|e| AppError::internal(format!("copia degli allegati: {e}")))??
        };
        if let Err(e) = s.db.insert_attachments(&rows) {
            remove_dirs(rows.iter().map(|r| dir.join(&r.id)).collect()).await;
            return Err(e);
        }
        s.emit_changed(Some(&task.project_id), Some(&task.id));
        Ok(rows
            .iter()
            .map(|r| attachments::view(&s.config.data_dir, &task.project_id, r))
            .collect())
    }

    /// Deletes the attachment's row, then its copy (best effort), under the task's lock. A
    /// running agent sees it gone from its folder at once. Emits `changed`. Errors:
    /// `NotFound`.
    pub async fn remove_task_attachment(&self, req: IdReq) -> Result<(), AppError> {
        let s = &self.inner;
        let task_id = s.db.attachment(&req.id)?.task_id;
        let _task = s.lock(task_key(&task_id)).await;
        let task = s.db.task(&task_id)?;
        let row = s.db.delete_attachment(&req.id)?;
        let dir = attachments::task_dir(&s.config.data_dir, &task.project_id, &task.id);
        remove_dirs(vec![dir.join(&row.id)]).await;
        s.emit_changed(Some(&task.project_id), Some(&task.id));
        Ok(())
    }

    /// Preflight (spec §7.7 step 2), worktree under the repo lock, rows, then spawns the
    /// first turn in the background; the prompt lists the task's attachments. Errors:
    /// `ClaudeNotFound`, `NotLoggedIn`, `UsageLimited`, `ConcurrencyLimit`, `Conflict` (active
    /// attempt exists), `Invalid` (bypass without the project's `allow_bypass`, sub-agent
    /// options, before any worktree), `Git`.
    pub async fn start_attempt(&self, req: StartAttemptReq) -> Result<AttemptView, AppError> {
        self.start_attempt_by(req, None).await
    }

    /// [`Core::start_attempt`] recording the attempt whose agent asked for it (the board tool
    /// `start_task`, `attempts.started_by_attempt`); `None` = the user.
    pub(crate) async fn start_attempt_by(
        &self,
        req: StartAttemptReq,
        started_by: Option<Id>,
    ) -> Result<AttemptView, AppError> {
        let s = &self.inner;
        let task = s.db.task(&req.task_id)?;
        let project = s.db.project(&task.project_id)?;
        check_bypass(req.permission_mode, &project)?;
        let subagent_model = subagent_options(req.subagent_model, req.max_subagents)?;
        let _task = s.lock(task_key(&task.id)).await;
        if s.db.active_attempt(&task.id)?.is_some() {
            return Err(AppError::conflict("Il task ha già un tentativo attivo"));
        }
        // Under the task's lock: what the prompt lists is what the folder holds.
        let attached = s.agent_attachments(&task)?;
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
            subagent_model,
            max_subagents: req.max_subagents,
            subagents_used: 0,
            started_by_attempt: started_by,
            allow_rules: Vec::new(),
            merge_commit: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
        };
        let parent = s.parent_task(&task)?;
        let prompt = first_prompt(
            &task.title,
            &task.description,
            parent.as_ref(),
            attached_paths(attached.as_ref()),
        );
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
                attachments_dir: attached.map(|a| a.dir),
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
    /// attempt's mode is unchanged. The task's attachments folder is given on every turn, and
    /// a fresh session's prompt lists them. Errors: `Busy` (turn running), `WorktreeMissing`,
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
        let attached = s.agent_attachments(&ctx.task)?;
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
            let (task, attachments) = (&ctx.task, attached_paths(attached.as_ref()));
            let parent = s.parent_task(task)?;
            fresh_prompt(
                &task.title,
                &task.description,
                parent.as_ref(),
                attachments,
                &log,
                &req.prompt,
            )
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
                attachments_dir: attached.map(|a| a.dir),
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

    /// A sub-task's change is its parent's too (its panel lists the sub-tasks): `Changed` for
    /// the parent follows, when the task is still there.
    fn emit_changed(&self, project_id: Option<&str>, task_id: Option<&str>) {
        self.emit(AppEvent::Changed(Changed {
            project_id: project_id.map(str::to_owned),
            task_id: task_id.map(str::to_owned),
        }));
        let parent = task_id.and_then(|id| self.db.task(id).ok()?.parent_id);
        if let Some(parent_id) = parent {
            self.emit(AppEvent::Changed(Changed {
                project_id: project_id.map(str::to_owned),
                task_id: Some(parent_id),
            }));
        }
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
        self.home.clone()
    }

    /// `CLAUDE_CONFIG_DIR` of the app's environment plus `extra_env`, when set: the user's
    /// Claude Code configuration, never attached.
    fn claude_config_dir(&self) -> Option<PathBuf> {
        self.base_env()
            .into_iter()
            .rev()
            .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
            .map(|(_, v)| PathBuf::from(v))
            .filter(|dir| !dir.as_os_str().is_empty())
    }

    /// A sub-task's parent, for its prompt; `None` for a top-level task.
    fn parent_task(&self, task: &Task) -> Result<Option<Task>, AppError> {
        task.parent_id
            .as_deref()
            .map(|id| self.db.task(id))
            .transpose()
    }

    /// What the agent of `task` is given of its attachments (spec F5); `None` without any.
    fn agent_attachments(&self, task: &Task) -> Result<Option<attachments::ForAgent>, AppError> {
        let rows = self.db.task_attachments(&task.id)?;
        Ok(attachments::for_agent(
            &self.config.data_dir,
            &task.project_id,
            &task.id,
            &rows,
        ))
    }

    /// Once a task's or a project's rows are gone, best effort: its attachments `folder` and
    /// the raw logs of `attempt_ids`, which no cascade reaches.
    async fn remove_files(&self, folder: PathBuf, attempt_ids: &[Id]) {
        let logs = attempt_ids
            .iter()
            .map(|id| runner::attempt_log_dir(&self.config.data_dir, id));
        remove_dirs(std::iter::once(folder).chain(logs).collect()).await;
    }

    /// Deletes one task (a sub-task has none of its own), the caller holding its lock
    /// (serialized with `start_attempt`): `Busy` if running; removes the worktrees of its
    /// attempts first, then the rows, then the attachments and the attempts' raw logs
    /// ([`Inner::remove_files`]). Emits `changed`.
    async fn remove_task(
        &self,
        git: &Git,
        project: &ProjectRow,
        task_id: &str,
    ) -> Result<(), AppError> {
        let busy = || AppError::busy(REMOVE_BUSY);
        // A follow-up is caught under its attempt's lock, and none can start on a removed
        // worktree.
        if self.task_running(task_id) {
            return Err(busy());
        }
        for attempt in self.db.task_attempts(task_id)? {
            if attempt.worktree_state == WorktreeState::Removed {
                continue;
            }
            let _attempt = self.lock(attempt_key(&attempt.id)).await;
            if self.turn(&attempt.id).is_some() {
                return Err(busy());
            }
            let _repo = self.lock(repo_key(&project.repo_path)).await;
            tolerate_missing(
                self.remove_worktree(git, &project.repo_path, &attempt)
                    .await,
            )?;
        }
        let attempt_ids = self.db.task_attempt_ids(task_id)?;
        self.db.delete_task(task_id)?;
        let folder = attachments::task_dir(&self.config.data_dir, &project.id, task_id);
        self.remove_files(folder, &attempt_ids).await;
        self.emit_changed(Some(&project.id), Some(task_id));
        Ok(())
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
        // A git too old for the runner's protections runs nothing (spec §8.1).
        let git = Git::new(git::find_git(&path), path.clone())
            .with_env(self.config.extra_env.clone())
            .gated()
            .await;
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
            base_url_env: claude::base_url_env(&base),
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

    /// `dir` is the home directory (`HOME` as given or canonical, both read at startup).
    fn is_home(&self, dir: &Path) -> bool {
        dir == self.home || self.home_canonical.as_deref() == Some(dir)
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
            git::is_scrubbed_git_var(k) || (!allow_api_key && claude::API_KEY_VARS.contains(&k))
        };
        let mut env: std::collections::BTreeMap<OsString, OsString> =
            self.base_env().into_iter().collect();
        claude::scrub_host_env(&mut env);
        env.retain(|k, _| !k.to_str().is_some_and(scrubbed));
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

    /// `trusted` = policy Trusted and the approved fingerprint still matches the configuration
    /// committed at the tip of the default target branch (new attempts start there, spec
    /// §8.9); `trust_error` = why that could not be checked or approved.
    async fn project_view(&self, row: &ProjectRow) -> Project {
        if row.config_policy != ConfigPolicy::Trusted {
            return row.to_project(false);
        }
        self.view_with(row, &self.target_config(row).await.1)
    }

    /// [`Inner::project_view`] from the target branch's configuration already read
    /// ([`Inner::target_config`]).
    fn view_with(&self, row: &ProjectRow, target: &Result<ConfigSnapshot, AppError>) -> Project {
        let trusted = row.config_policy == ConfigPolicy::Trusted
            && target
                .as_ref()
                .is_ok_and(|s| row.trusted_fingerprint.as_deref() == Some(s.fingerprint.as_str()));
        let trust_error = target
            .as_ref()
            .err()
            .filter(|_| row.config_policy == ConfigPolicy::Trusted)
            .map(|e| e.message.clone());
        Project {
            trust_error,
            ..row.to_project(trusted)
        }
    }

    /// The configuration of the checkout at `dir` (spec §8.9): a worktree before its turns.
    /// Never computed for the home directory, whose `.claude` is the user's own and never read
    /// (spec §10.1). Errors: `Invalid` (home, a limit, a link out of `dir`, a change during
    /// the walk), `Io`.
    async fn config_snapshot(&self, dir: &Path) -> Result<ConfigSnapshot, AppError> {
        if self.is_home(dir) {
            return Err(home_refusal());
        }
        git::config_snapshot(dir).await
    }

    /// The configuration committed in `commit` of `repo` (spec §8.9), cached with its
    /// `Invalid` errors (the others, a timeout or a reader that failed, may not happen again).
    /// Never computed for a repository that is the home directory. Errors: those of
    /// [`Git::commit_config_snapshot`], `Invalid` (home).
    async fn commit_config(
        &self,
        git: &Git,
        repo: &Path,
        commit: &str,
    ) -> Result<ConfigSnapshot, AppError> {
        if self.is_home(repo) {
            return Err(home_refusal());
        }
        let key = (repo.to_string_lossy().into_owned(), commit.to_owned());
        if let Some(known) = guard(&self.commit_configs).get(&key) {
            return known.clone();
        }
        let snapshot = git.commit_config_snapshot(repo, commit).await;
        if snapshot
            .as_ref()
            .err()
            .is_none_or(|e| e.code == ErrorCode::Invalid)
        {
            let mut cache = guard(&self.commit_configs);
            if cache.len() >= MAX_COMMIT_CONFIGS {
                cache.clear();
            }
            cache.insert(key, snapshot.clone());
        }
        snapshot
    }

    /// What a Trusted approval of `row` approves (spec §8.9): the configuration committed at
    /// the tip of its default target branch, where new worktrees start, and that tip. `Err`
    /// when it cannot be approved: home, branch unreadable, not fingerprintable, or a
    /// configuration that would bill outside the subscription ([`billing_refusal`]).
    async fn target_config(
        &self,
        row: &ProjectRow,
    ) -> (Option<ConfigBase>, Result<ConfigSnapshot, AppError>) {
        let repo = Path::new(&row.repo_path);
        if self.is_home(repo) {
            return (None, Err(home_refusal()));
        }
        let git = self.git().await;
        let branch = &row.default_target_branch;
        let commit = match git.branch_tip(repo, branch).await {
            Ok(commit) => commit,
            Err(e) => {
                return (
                    None,
                    Err(AppError::invalid(format!(
                        "Il branch target predefinito {branch} non si può leggere ({}): la \
                         configurazione da approvare è quella del suo ultimo commit",
                        e.message
                    ))),
                );
            }
        };
        let base = ConfigBase {
            branch: branch.clone(),
            commit,
        };
        let snapshot = self
            .commit_config(&git, repo, &base.commit)
            .await
            .and_then(|s| billing_refusal(&s, &base).map_or(Ok(s), Err));
        (Some(base), snapshot)
    }

    /// Stops the running turns of `project_id` that use what the change from `before` to
    /// `after` revoked (the bypass opt-in, the Trusted policy), with a Notice: a revocation
    /// must not wait for the end of the turn. A turn not launched yet counts as using it.
    fn stop_revoked_turns(&self, project_id: &str, before: &SecurityState, after: &ProjectRow) {
        let bypass = before.allow_bypass && !after.allow_bypass;
        let trust = before.config_policy == ConfigPolicy::Trusted
            && after.config_policy == ConfigPolicy::Isolated;
        if !bypass && !trust {
            return;
        }
        let turns: Vec<Arc<TurnHandle>> = guard(&self.turns)
            .values()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect();
        for turn in turns {
            let hit = turn
                .caps()
                .is_none_or(|c| (bypass && c.bypass) || (trust && c.trusted));
            if hit {
                turn.notice(atm_types::Level::Warn, runner::REVOKED_NOTICE);
                turn.stop(StopCause::User, runner::StopTimings::NORMAL);
            }
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

fn project_key(project_id: &str) -> String {
    format!("project:{project_id}")
}

/// The real user id of this process.
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// A character that changes how text reads without showing: line breaks and other controls,
/// bidirectional embeddings, overrides and isolates (U+202A–U+202E, U+2066–U+2069), marks.
pub fn is_hidden_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
        )
}

/// A new `claude_path_override`: absolute, an existing file (links followed, the CLI's own
/// is one), neither it nor its target inside a project or the worktree root.
async fn check_claude_override(
    path: &Path,
    projects: &[ProjectRow],
    worktree_root: &Path,
) -> Result<(), AppError> {
    let shown = path.display();
    if !path.is_absolute() {
        return Err(AppError::invalid(format!(
            "Il percorso di Claude Code deve essere assoluto: {shown}"
        )));
    }
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(AppError::invalid(format!(
                "Il percorso di Claude Code non è un file: {shown}"
            )));
        }
        Err(e) => {
            return Err(AppError::invalid(format!(
                "Il percorso di Claude Code non esiste: {shown} ({e})"
            )));
        }
    }
    let target = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| AppError::invalid(format!("{shown}: {e}")))?;
    let root = tokio::fs::canonicalize(worktree_root)
        .await
        .unwrap_or_else(|_| worktree_root.to_path_buf());
    for p in [path, target.as_path()] {
        if let Some(project) = projects
            .iter()
            .find(|project| p.starts_with(&project.repo_path))
        {
            return Err(AppError::invalid(format!(
                "Il percorso di Claude Code non può stare dentro il progetto {}: un repository \
                 non può scegliere il programma che l'app esegue",
                project.name
            )));
        }
        if p.starts_with(worktree_root) || p.starts_with(&root) {
            return Err(AppError::invalid(
                "Il percorso di Claude Code non può stare nella cartella dei worktree",
            ));
        }
    }
    Ok(())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// A sub-task's parent (spec §5, one level only): an existing task of the same project that
/// is not itself a sub-task. Errors: `Invalid`.
fn check_parent(db: &Db, project_id: &str, parent_id: &str) -> Result<(), AppError> {
    let parent = db.task(parent_id).map_err(|e| match e.code {
        ErrorCode::NotFound => AppError::invalid("Il task padre non esiste"),
        _ => e,
    })?;
    if parent.project_id != project_id {
        return Err(AppError::invalid(
            "Il task padre appartiene a un altro progetto",
        ));
    }
    if parent.parent_id.is_some() {
        return Err(AppError::invalid(
            "Un sotto task non può avere a sua volta sotto task",
        ));
    }
    Ok(())
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

/// Never Trusted, never fingerprinted: a repository that is the home directory (its `.claude`
/// is the user's own configuration, which the app never reads, spec §10.1).
fn home_refusal() -> AppError {
    AppError::invalid(
        "Un repository nella cartella home non può essere considerato attendibile: la sua \
         .claude è la configurazione dell'utente, che l'app non legge",
    )
}

/// The approval of a configuration that would bill the agents outside the subscription
/// (`ConfigSnapshot::billing`: `git::BILLING_SETTINGS_KEYS`, `git::BILLING_ENV_VARS`) is
/// refused (spec §8.9, §10.2): `Invalid`, naming what sets it. `None` when there is nothing.
fn billing_refusal(snapshot: &ConfigSnapshot, base: &ConfigBase) -> Option<AppError> {
    (!snapshot.billing.is_empty()).then(|| {
        AppError::invalid(format!(
            "Configurazione Claude non approvabile (branch {}, commit {}): {}. Gli agenti \
             usano solo l'abbonamento Claude, e queste impostazioni li farebbero fatturare via \
             API o da un altro provider: toglile dal branch o lascia il progetto Isolato",
            base.branch,
            base.short(),
            snapshot.billing.join("; ")
        ))
    })
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

/// `StartAttemptReq`'s sub-agent options (spec F6), checked before any worktree exists: the
/// model trimmed (empty = the CLI's default) and one of `MODEL_ALIASES`, the limit at most
/// `MAX_SUBAGENTS`. The model to store. Errors: `Invalid`.
fn subagent_options(model: Option<String>, max: Option<u8>) -> Result<Option<String>, AppError> {
    if max.is_some_and(|n| n > MAX_SUBAGENTS) {
        return Err(AppError::invalid(format!(
            "Il limite di sub-agent va da 0 a {MAX_SUBAGENTS}"
        )));
    }
    let model = non_empty(model);
    if model
        .as_deref()
        .is_some_and(|m| !MODEL_ALIASES.contains(&m))
    {
        return Err(AppError::invalid(format!(
            "Il modello dei sub-agent deve essere uno di: {}",
            MODEL_ALIASES.join(", ")
        )));
    }
    Ok(model)
}

/// Removes `dirs` with their content on the blocking pool, best effort
/// ([`attachments::remove_dir_logged`]).
async fn remove_dirs(dirs: Vec<PathBuf>) {
    let removed = tokio::task::spawn_blocking(move || {
        dirs.iter()
            .for_each(|dir| attachments::remove_dir_logged(dir));
    });
    if let Err(e) = removed.await {
        eprintln!("cleanup: {e}");
    }
}

/// The copies' paths of [`Inner::agent_attachments`] (none without attachments).
fn attached_paths(attached: Option<&attachments::ForAgent>) -> &[PathBuf] {
    attached.map_or(&[], |a| a.paths.as_slice())
}

/// First turn (spec §7.4 step 2): `# {title}\n\n{description}`, then a sub-task's parent
/// ([`parent_section`]) and the attachments ([`attachments::prompt_section`]).
fn first_prompt(
    title: &str,
    description: &str,
    parent: Option<&Task>,
    attachments: &[PathBuf],
) -> String {
    let mut prompt = format!("# {title}\n\n{description}").trim_end().to_owned();
    prompt.push_str(&parent.map_or_else(String::new, parent_section));
    prompt.push_str(&attachments::prompt_section(attachments));
    prompt
}

/// `## Parent task` with the parent's title and description, as context.
fn parent_section(parent: &Task) -> String {
    let section = format!(
        "\n\n## Parent task\n\nThis task is a sub-task of the one below, given for context: \
         work only on this task.\n\n### {}\n\n{}",
        parent.title, parent.description
    );
    section.trim_end().to_owned()
}

/// `fresh_session` (spec §7.4 step 2): the task, the attempt's commits and the user's text.
fn fresh_prompt(
    title: &str,
    description: &str,
    parent: Option<&Task>,
    attachments: &[PathBuf],
    log: &str,
    text: &str,
) -> String {
    let mut prompt = first_prompt(title, description, parent, attachments);
    if !log.trim().is_empty() {
        prompt.push_str("\n\nCommits already made on this branch:\n");
        prompt.push_str(log.trim_end());
    }
    prompt.push_str("\n\n");
    prompt.push_str(text);
    prompt
}
