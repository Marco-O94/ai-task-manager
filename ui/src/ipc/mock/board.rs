//! Mock of env, settings, projects and board commands (see `BOARD_COMMANDS` in `mod.rs`).
//! Owner: M2-UI-BOARD. An in-memory backend that lives as long as the page: it validates like
//! the real commands (`NotFound`, `Invalid`, `Conflict`, `Busy` for running tasks), orders
//! tasks with the positions of spec §5.2 and emits `changed` / `env_changed` after mutations.
//!
//! URL flags, combinable (`/?env=paused,billing&badges=0`):
//! - `env=` comma-separated: `no_cli` (CLI not found; "Ricontrolla" with a "Percorso di
//!   claude" finds it), `old` (CLI below the minimum), `logged_out` ("Accedi" completes the
//!   login 4 s later), `unknown` (`auth status` failed), `paused` (usage-limit pause),
//!   `billing` (Console login, billed via API), `api_key` (API key passthrough enabled),
//!   `problems` (a non-blocking environment problem);
//! - `projects=0`: no projects ("Aggiungi repository" cycles through a fixed list of folders);
//! - `badges=0`: no demo attempt fields on the cards `attempt.rs` knows nothing about (by
//!   default they show every agent state: waiting, failed, interrupted, ready, closed).

use std::cell::RefCell;

use atm_types::*;
use serde_json::Value;

use crate::state::board::move_to;

const PROJECT_ID: &str = "project-demo";
const BRANCHES: &[&str] = &["develop", "feature/login", "main"];
/// The fake `claude auth login` completes this long after "Accedi".
const LOGIN_DELAY_MS: Millis = 4_000;
/// Successive answers of `pick_repo_folder`: a repo with Claude config (warning), a folder
/// that is not a repo (`Invalid`), a cancelled picker, a plain repo.
const PICKS: &[Option<&str>] = &[
    Some("/Users/demo/Progetti/api-server"),
    Some("/Users/demo/Documenti"),
    None,
    Some("/Users/demo/Progetti/sito-web"),
];

struct Flags {
    env: Vec<String>,
    no_projects: bool,
    badges: bool,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            env: Vec::new(),
            no_projects: false,
            badges: true,
        }
    }
}

impl Flags {
    fn from_url() -> Self {
        let search = web_sys::window()
            .and_then(|w| w.location().search().ok())
            .unwrap_or_default();
        let mut flags = Self::default();
        for pair in search.trim_start_matches('?').split('&') {
            match pair.split_once('=').unwrap_or((pair, "")) {
                ("env", v) => flags.env = v.split(',').map(str::to_owned).collect(),
                ("projects", "0") => flags.no_projects = true,
                ("badges", "0") => flags.badges = false,
                _ => {}
            }
        }
        flags
    }

    fn has(&self, env: &str) -> bool {
        self.env.iter().any(|e| e == env)
    }
}

struct Mock {
    env: EnvStatus,
    settings: Settings,
    projects: Vec<Project>,
    tasks: Vec<Task>,
    /// When the fake Terminal login completes, and with which method.
    login: Option<(Millis, LoginMethod)>,
    picks: usize,
}

thread_local! {
    static FLAGS: Flags = Flags::from_url();
    static MOCK: RefCell<Mock> = RefCell::new(FLAGS.with(Mock::seed));
}

/// Runs `f` on the mock state. Never call `attempt::*` or `super::emit` inside: they may
/// call back into this file.
fn with<R>(f: impl FnOnce(&mut Mock) -> R) -> R {
    MOCK.with_borrow_mut(f)
}

/// Serves one board-side command: `req` is the serialized `C::Req`, the result the
/// serialized `C::Res`. Emits `changed` through `super::emit` after each mutation.
pub async fn handle(cmd: &str, req: Value) -> Result<Value, AppError> {
    match cmd {
        GetEnv::NAME => serve::<GetEnv>(req, get_env),
        OpenLoginTerminal::NAME => serve::<OpenLoginTerminal>(req, open_login_terminal),
        ResumeAgents::NAME => serve::<ResumeAgents>(req, |_| {
            update_env(|e| e.paused = None);
            Ok(with(|m| m.env.clone()))
        }),
        GetSettings::NAME => serve::<GetSettings>(req, |_| Ok(with(|m| m.settings.clone()))),
        UpdateSettings::NAME => serve::<UpdateSettings>(req, update_settings),
        ListProjects::NAME => serve::<ListProjects>(req, |_| Ok(with(|m| m.projects.clone()))),
        PickRepoFolder::NAME => serve::<PickRepoFolder>(req, |_| Ok(pick_repo_folder())),
        AddProject::NAME => serve::<AddProject>(req, add_project),
        UpdateProject::NAME => serve::<UpdateProject>(req, update_project),
        SetProjectSecurity::NAME => serve::<SetProjectSecurity>(req, set_project_security),
        RemoveProject::NAME => serve::<RemoveProject>(req, remove_project),
        ListBranches::NAME => serve::<ListBranches>(req, list_branches),
        GetBoard::NAME => serve::<GetBoard>(req, get_board),
        CreateTask::NAME => serve::<CreateTask>(req, create_task),
        UpdateTask::NAME => serve::<UpdateTask>(req, update_task),
        MoveTask::NAME => serve::<MoveTask>(req, move_task),
        DeleteTask::NAME => serve::<DeleteTask>(req, delete_task),
        OpenUrl::NAME => serve::<OpenUrl>(req, open_url),
        _ => Err(AppError::not_implemented(cmd)),
    }
}

fn serve<C: Command>(
    req: Value,
    f: impl FnOnce(C::Req) -> Result<C::Res, AppError>,
) -> Result<Value, AppError> {
    let res = f(serde_json::from_value(req)?)?;
    Ok(serde_json::to_value(res)?)
}

/// For `attempt.rs` and `attachments.rs`: the mock task with this id.
#[allow(dead_code)] // hook for attempt.rs
pub fn task(id: &str) -> Option<Task> {
    with(|m| m.tasks.iter().find(|t| t.id == id).cloned())
}

/// For `overview.rs`: the mock project with this id.
pub fn find_project(id: &str) -> Option<Project> {
    with(|m| m.projects.iter().find(|p| p.id == id).cloned())
}

/// For `attempt.rs`: the model an attempt of this project gets when the request names none,
/// as the core resolves it: the project's default, then the settings' default.
pub fn default_model(project_id: &str) -> Option<String> {
    with(|m| {
        m.projects
            .iter()
            .find(|p| p.id == project_id)
            .and_then(|p| p.default_model.clone())
            .or_else(|| m.settings.default_model.clone())
    })
}

/// For `attempt.rs`: lifecycle transition of a task (start → inprogress, turn end →
/// inreview, merge → done, discard → todo), appended to the end of the new column; emits
/// `changed`. A task already in `status` keeps its place (the user's drag order).
#[allow(dead_code)] // hook for attempt.rs
pub fn set_task_status(id: &str, status: TaskStatus) {
    let project = with(|m| {
        let task = m.tasks.iter().find(|t| t.id == id)?;
        if task.status == status {
            return Some(task.project_id.clone());
        }
        move_to(&mut m.tasks, id, status, None).then(|| m.touch(id))
    });
    if let Some(project) = project {
        task_changed(project, id);
    }
}

/// For `attempt.rs`: changes the mock env (usage-limit `paused`, `auth` after an auth
/// failure, `running`) and emits `env_changed`, as the runner does. `f` runs outside the
/// mock's borrow, so it may call the other hooks.
pub fn update_env(f: impl FnOnce(&mut EnvStatus)) {
    let mut env = with(|m| m.env.clone());
    f(&mut env);
    env.checked_at = super::now_ms();
    with(|m| m.env = env.clone());
    super::emit(EVENT_ENV_CHANGED, &env);
}

fn changed(project_id: Option<Id>, task_id: Option<Id>) {
    super::emit(
        EVENT_CHANGED,
        &Changed {
            project_id,
            task_id,
        },
    );
}

/// `changed` for a task, and for its parent if it is a sub-task (the parent's panel lists it).
fn task_changed(project_id: Id, task_id: &str) {
    let parent = with(|m| {
        let task = m.tasks.iter().find(|t| t.id == task_id)?;
        task.parent_id.clone()
    });
    changed(Some(project_id.clone()), Some(task_id.to_owned()));
    if parent.is_some() {
        changed(Some(project_id), parent);
    }
}

fn get_env(req: GetEnvReq) -> Result<EnvStatus, AppError> {
    Ok(with(|m| {
        let now = super::now_ms();
        if let Some((_, method)) = m.login.filter(|(at, _)| *at <= now) {
            m.login = None;
            m.env.auth = logged_in(method);
        }
        // Discovery re-reads `claude_path_override` only on a forced check.
        if req.force
            && m.env.claude.path.is_none()
            && let Some(path) = m.settings.claude_path_override.clone()
        {
            m.env.claude = claude_info(Some(path), CLAUDE_TESTED_VERSION);
        }
        m.env.checked_at = now;
        m.env.clone()
    }))
}

fn open_login_terminal(req: OpenLoginTerminalReq) -> Result<(), AppError> {
    with(|m| {
        if m.env.claude.path.is_none() {
            return Err(AppError::new(
                ErrorCode::ClaudeNotFound,
                "Claude Code non trovato",
            ));
        }
        m.login = Some((super::now_ms() + LOGIN_DELAY_MS, req.method));
        Ok(())
    })
}

fn update_settings(req: Settings) -> Result<Settings, AppError> {
    if !(1..=6).contains(&req.max_running) {
        return Err(AppError::invalid("Gli agenti in parallelo vanno da 1 a 6"));
    }
    if req.worktree_root.trim().is_empty() || req.editor_app.trim().is_empty() {
        return Err(AppError::invalid(
            "Cartella dei worktree ed editor sono obbligatori",
        ));
    }
    let max_changed = with(|m| {
        m.settings = req.clone();
        m.env.max_running != req.max_running
    });
    if max_changed {
        update_env(|e| e.max_running = req.max_running);
    }
    Ok(req)
}

fn pick_repo_folder() -> Option<String> {
    with(|m| {
        m.picks += 1;
        PICKS[(m.picks - 1) % PICKS.len()].map(str::to_owned)
    })
}

fn add_project(req: AddProjectReq) -> Result<AddProjectRes, AppError> {
    let path = req.path.trim_end_matches('/').to_owned();
    // The fake file system: only the folders under ~/Progetti are git repositories.
    if !path.starts_with("/Users/demo/Progetti/") {
        return Err(AppError::invalid(format!("{path} non è un repository git")));
    }
    let project = with(|m| {
        if m.projects.iter().any(|p| p.repo_path == path) {
            return Err(AppError::conflict("Questo repository è già stato aggiunto"));
        }
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let project = project(&super::new_id(), &name, &path);
        m.projects.push(project.clone());
        Ok(project)
    })?;
    let warnings = if path.ends_with("api-server") {
        vec![
            "Il repository contiene .claude/ e .mcp.json: non verranno caricati finché il \
             progetto è Isolato"
                .into(),
        ]
    } else {
        Vec::new()
    };
    changed(None, None);
    Ok(AddProjectRes { project, warnings })
}

fn update_project(req: UpdateProjectReq) -> Result<Project, AppError> {
    let name = req.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(AppError::invalid("Il nome va da 1 a 200 caratteri"));
    }
    let description = req.description.trim().to_owned();
    if description.chars().count() > MAX_PROJECT_DESCRIPTION {
        return Err(AppError::invalid(format!(
            "La descrizione può avere al massimo {MAX_PROJECT_DESCRIPTION} caratteri"
        )));
    }
    if !BRANCHES.contains(&req.default_target_branch.as_str()) {
        return Err(AppError::invalid(format!(
            "Il branch {} non esiste",
            req.default_target_branch
        )));
    }
    let project = with(|m| {
        let p = m.project_mut(&req.id)?;
        if req.default_permission_mode == PermissionMode::BypassPermissions && !p.allow_bypass {
            return Err(AppError::invalid(
                "La modalità Autonoma richiede di abilitarla nella sicurezza del progetto",
            ));
        }
        p.name = name;
        p.description = description;
        p.default_target_branch = req.default_target_branch;
        p.default_permission_mode = req.default_permission_mode;
        p.default_model = req.default_model;
        p.updated_at = super::now_ms();
        Ok(p.clone())
    })?;
    changed(None, None);
    Ok(project)
}

fn set_project_security(req: SetProjectSecurityReq) -> Result<Project, AppError> {
    let project = with(|m| {
        let p = m.project_mut(&req.id)?;
        p.config_policy = req.config_policy;
        p.trusted = req.config_policy == ConfigPolicy::Trusted;
        p.allow_bypass = req.allow_bypass;
        if !p.allow_bypass && p.default_permission_mode == PermissionMode::BypassPermissions {
            p.default_permission_mode = PermissionMode::AcceptEdits;
        }
        p.updated_at = super::now_ms();
        Ok::<_, AppError>(p.clone())
    })?;
    changed(None, None);
    Ok(project)
}

fn remove_project(req: IdReq) -> Result<(), AppError> {
    let tasks = with(|m| {
        m.project_mut(&req.id)?;
        Ok::<_, AppError>(m.tasks_of(&req.id))
    })?;
    if tasks.into_iter().map(card).any(|c| c.running) {
        return Err(AppError::busy(
            "Ferma gli agenti del progetto prima di rimuoverlo",
        ));
    }
    let removed: Vec<Id> = with(|m| {
        m.projects.retain(|p| p.id != req.id);
        let (gone, kept): (Vec<Task>, Vec<Task>) = std::mem::take(&mut m.tasks)
            .into_iter()
            .partition(|t| t.project_id == req.id);
        m.tasks = kept;
        gone.into_iter().map(|t| t.id).collect()
    });
    for id in &removed {
        super::attempt::forget_task(id);
        super::attachments::forget_task(id);
    }
    changed(None, None);
    Ok(())
}

fn list_branches(req: ProjectIdReq) -> Result<BranchList, AppError> {
    with(|m| m.project_mut(&req.project_id).map(|_| ()))?;
    Ok(BranchList {
        current: Some("main".into()),
        branches: BRANCHES.iter().map(|b| (*b).to_owned()).collect(),
    })
}

fn get_board(req: ProjectIdReq) -> Result<Vec<TaskCard>, AppError> {
    let mut tasks = with(|m| {
        m.project_mut(&req.project_id)?;
        Ok::<_, AppError>(m.tasks_of(&req.project_id))
    })?;
    tasks.sort_by(|a, b| {
        column_index(a.status)
            .cmp(&column_index(b.status))
            .then(a.position.total_cmp(&b.position))
    });
    Ok(tasks.into_iter().map(card).collect())
}

fn create_task(req: CreateTaskReq) -> Result<TaskCard, AppError> {
    let (title, description) = validate_task(&req.title, req.description)?;
    let status = req.status.unwrap_or(TaskStatus::Todo);
    let task = with(|m| {
        m.project_mut(&req.project_id)?;
        // One level, same project (the core's messages).
        if let Some(parent_id) = &req.parent_id {
            let parent = m
                .task_mut(parent_id)
                .map_err(|_| AppError::invalid("Il task padre non esiste"))?;
            if parent.project_id != req.project_id {
                return Err(AppError::invalid(
                    "Il task padre appartiene a un altro progetto",
                ));
            }
            if parent.parent_id.is_some() {
                return Err(AppError::invalid(
                    "Un sotto task non può avere a sua volta sotto task",
                ));
            }
        }
        let now = super::now_ms();
        let id = super::new_id();
        m.tasks.push(Task {
            id: id.clone(),
            project_id: req.project_id.clone(),
            title,
            description,
            status,
            position: 0.0,
            created_at: now,
            updated_at: now,
            parent_id: req.parent_id.clone(),
        });
        move_to(&mut m.tasks, &id, status, None);
        m.task_mut(&id).cloned()
    })?;
    task_changed(task.project_id.clone(), &task.id);
    Ok(card(task))
}

fn update_task(req: UpdateTaskReq) -> Result<TaskCard, AppError> {
    let (title, description) = validate_task(&req.title, req.description)?;
    let task = with(|m| {
        let t = m.task_mut(&req.id)?;
        t.title = title;
        t.description = description;
        t.updated_at = super::now_ms();
        Ok::<_, AppError>(t.clone())
    })?;
    task_changed(task.project_id.clone(), &task.id);
    Ok(card(task))
}

fn move_task(req: MoveTaskReq) -> Result<(), AppError> {
    let current = with(|m| m.task_mut(&req.id).cloned())?;
    if matches!(req.status, TaskStatus::Done | TaskStatus::Cancelled) && card(current).running {
        return Err(AppError::busy(
            "Il task è in esecuzione: ferma l'agente prima di spostarlo",
        ));
    }
    let project = with(|m| {
        if !move_to(&mut m.tasks, &req.id, req.status, req.before_id.as_deref()) {
            return Err(AppError::invalid(
                "before_id non è nella colonna di destinazione",
            ));
        }
        Ok(m.touch(&req.id))
    })?;
    task_changed(project, &req.id);
    Ok(())
}

fn delete_task(req: IdReq) -> Result<(), AppError> {
    let task = with(|m| m.task_mut(&req.id).cloned())?;
    if card(task.clone()).running {
        return Err(AppError::busy(
            "Il task è in esecuzione: ferma l'agente prima di eliminarlo",
        ));
    }
    // Sub-tasks go with their parent (the DB's cascade).
    let children = subtask_cards(&req.id);
    if children.iter().any(|c| c.running) {
        return Err(AppError::busy("Un sotto task è in esecuzione"));
    }
    let ids: Vec<Id> = children
        .into_iter()
        .map(|c| c.task.id)
        .chain(std::iter::once(req.id.clone()))
        .collect();
    with(|m| m.tasks.retain(|t| !ids.contains(&t.id)));
    for id in &ids {
        super::attempt::forget_task(id);
        super::attachments::forget_task(id);
    }
    // Like the core: `changed` for each sub-task removed, then for the task and its parent.
    for id in &ids[..ids.len() - 1] {
        changed(Some(task.project_id.clone()), Some(id.clone()));
    }
    if let Some(parent) = &task.parent_id {
        changed(Some(task.project_id.clone()), Some(parent.clone()));
    }
    changed(Some(task.project_id), Some(req.id));
    Ok(())
}

fn open_url(req: OpenUrlReq) -> Result<(), AppError> {
    if !(req.url.starts_with("https://") || req.url.starts_with("http://")) {
        return Err(AppError::invalid("Solo indirizzi http(s)"));
    }
    leptos::logging::log!("mock open_url: {}", req.url);
    Ok(())
}

/// Title 1..=200 characters (trimmed), description up to 100 000 (spec §5.2 CHECKs).
fn validate_task(title: &str, description: String) -> Result<(String, String), AppError> {
    let title = title.trim().to_owned();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(AppError::invalid("Il titolo va da 1 a 200 caratteri"));
    }
    if description.chars().count() > 100_000 {
        return Err(AppError::invalid(
            "La descrizione supera i 100 000 caratteri",
        ));
    }
    Ok((title, description))
}

fn column_index(status: TaskStatus) -> usize {
    TaskStatus::ALL
        .iter()
        .position(|s| *s == status)
        .unwrap_or(0)
}

impl Mock {
    fn seed(flags: &Flags) -> Self {
        let mut env = EnvStatus {
            claude: claude_info(
                Some("/Users/demo/.local/bin/claude".into()),
                CLAUDE_TESTED_VERSION,
            ),
            auth: logged_in(LoginMethod::ClaudeAi),
            git_version: Some("2.50.1".into()),
            api_key_in_env: false,
            cloud_provider_env: false,
            base_url_env: false,
            paused: None,
            running: 0,
            max_running: Settings::default().max_running,
            problems: Vec::new(),
            checked_at: super::now_ms(),
        };
        let mut settings = Settings::default();
        if flags.has("no_cli") {
            env.claude = ClaudeInfo {
                path: None,
                version: None,
                supported: false,
                ..env.claude
            };
        }
        if flags.has("old") {
            env.claude = claude_info(env.claude.path, "2.1.100");
        }
        if flags.has("logged_out") {
            env.auth = AuthState::LoggedOut;
        }
        if flags.has("unknown") {
            env.auth = AuthState::Unknown {
                reason: "claude auth status non ha risposto entro 10 s".into(),
            };
        }
        if flags.has("billing") {
            env.auth = logged_in(LoginMethod::Console);
        }
        if flags.has("paused") {
            env.paused = Some("Limite di utilizzo raggiunto: riprova alle 18:00".into());
        }
        if flags.has("api_key") {
            env.api_key_in_env = true;
            settings.allow_env_api_key = true;
        }
        if flags.has("problems") {
            env.problems
                .push("git 2.43.0 è troppo vecchio: serve almeno la versione 2.44".into());
        }
        if flags.badges {
            env.running = 1;
        }

        let (projects, tasks) = if flags.no_projects {
            (Vec::new(), Vec::new())
        } else {
            seed_board()
        };
        Self {
            env,
            settings,
            projects,
            tasks,
            login: None,
            picks: 0,
        }
    }

    fn project_mut(&mut self, id: &str) -> Result<&mut Project, AppError> {
        self.projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::not_found(format!("progetto {id}")))
    }

    fn task_mut(&mut self, id: &str) -> Result<&mut Task, AppError> {
        self.tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| AppError::not_found(format!("task {id}")))
    }

    fn tasks_of(&self, project_id: &str) -> Vec<Task> {
        self.tasks
            .iter()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect()
    }

    /// Marks a moved task as updated; returns its project.
    fn touch(&mut self, id: &str) -> Id {
        let now = super::now_ms();
        self.tasks
            .iter_mut()
            .find(|t| t.id == id)
            .map(|t| {
                t.updated_at = now;
                t.project_id.clone()
            })
            .unwrap_or_default()
    }
}

fn claude_info(path: Option<String>, version: &str) -> ClaudeInfo {
    ClaudeInfo {
        supported: version_at_least(version, CLAUDE_MIN_VERSION),
        path,
        version: Some(version.to_owned()),
        min_version: CLAUDE_MIN_VERSION.into(),
        tested_version: CLAUDE_TESTED_VERSION.into(),
    }
}

fn version_at_least(version: &str, min: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').filter_map(|n| n.parse().ok()).collect() };
    parse(version) >= parse(min)
}

fn logged_in(method: LoginMethod) -> AuthState {
    let (auth_method, org_name, subscription_type) = match method {
        LoginMethod::ClaudeAi => ("claude.ai", None, Some("max")),
        LoginMethod::Console => ("console", Some("Demo S.r.l."), None),
        LoginMethod::Sso => ("claude.ai", Some("Demo S.r.l."), Some("enterprise")),
    };
    AuthState::LoggedIn {
        auth_method: Some(auth_method.into()),
        api_provider: Some("firstParty".into()),
        email: Some("demo@example.com".into()),
        org_name: org_name.map(Into::into),
        subscription_type: subscription_type.map(Into::into),
    }
}

fn project(id: &str, name: &str, repo_path: &str) -> Project {
    let now = super::now_ms();
    Project {
        id: id.into(),
        name: name.into(),
        description: String::new(),
        repo_path: repo_path.into(),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: None,
        config_policy: ConfigPolicy::Isolated,
        trusted: false,
        allow_bypass: false,
        created_at: now,
        updated_at: now,
        trust_error: None,
    }
}

/// Two projects; the M1 ids (`task-todo`, `task-inprogress`, `task-inreview`, `task-done`)
/// are kept for `attempt.rs` and `/?task=<id>`; `task-pagination` has three sub-tasks. Tasks
/// were created over the last days, the first listed the oldest, and updated some hours later.
fn seed_board() -> (Vec<Project>, Vec<Task>) {
    use TaskStatus::*;
    let demo: &[(&str, &str, TaskStatus)] = &[
        ("task-todo", "Aggiungi una riga al README", Todo),
        ("task-parser-tests", "Scrivi i test del parser", Todo),
        ("task-api-docs", "Documenta l'API pubblica", Todo),
        (
            "task-inprogress",
            "Rinomina il modulo di parsing",
            InProgress,
        ),
        (
            "task-pagination",
            "Aggiungi la paginazione alla lista utenti",
            InProgress,
        ),
        ("task-inreview", "Correggi il test instabile", InReview),
        ("task-config", "Migra la configurazione a TOML", InReview),
        ("task-logger", "Refactor del logger", InReview),
        ("task-done", "Aggiorna le dipendenze", Done),
        ("task-dead-code", "Rimuovi il codice morto", Done),
        ("task-websocket", "Prova con i WebSocket", Cancelled),
    ];
    // Sub-tasks of "task-pagination", in three columns.
    let pagination: &[(&str, &str, TaskStatus)] = &[
        (
            "task-pagination-api",
            "Aggiungi limit e offset all'API",
            Done,
        ),
        (
            "task-pagination-ui",
            "Pulsanti di pagina nella tabella",
            InProgress,
        ),
        ("task-pagination-tests", "Test della paginazione", Todo),
    ];
    let web: &[(&str, &str, TaskStatus)] = &[
        ("task-contacts", "Nuova pagina contatti", Todo),
        ("task-images", "Ottimizza le immagini", InReview),
    ];
    let projects = vec![
        Project {
            description: "Servizio di esempio del mock: API REST con parser e lista utenti.".into(),
            ..project(PROJECT_ID, "demo", "/Users/demo/demo")
        },
        project("project-web", "sito-web", "/Users/demo/Progetti/sito-web"),
    ];
    const HOUR: Millis = 3_600_000;
    let now = super::now_ms();
    let total = (demo.len() + pagination.len() + web.len()) as Millis;
    let mut tasks = Vec::new();
    for (project_id, parent, rows) in [
        (PROJECT_ID, None, demo),
        (PROJECT_ID, Some("task-pagination"), pagination),
        ("project-web", None, web),
    ] {
        for &(id, title, status) in rows {
            let created_at = now - (total - tasks.len() as Millis) * 9 * HOUR;
            tasks.push(Task {
                id: id.into(),
                project_id: project_id.into(),
                title: title.into(),
                description: "Task di esempio del mock.".into(),
                status,
                position: 0.0,
                created_at,
                updated_at: created_at + (tasks.len() as Millis % 4 + 1) * HOUR,
                parent_id: parent.map(Into::into),
            });
            move_to(&mut tasks, id, status, None);
        }
    }
    (projects, tasks)
}

/// For `attempt.rs`: the cards of the task's sub-tasks, in board order.
pub fn subtask_cards(task_id: &str) -> Vec<TaskCard> {
    let mut tasks: Vec<Task> = with(|m| {
        m.tasks
            .iter()
            .filter(|t| t.parent_id.as_deref() == Some(task_id))
            .cloned()
            .collect()
    });
    tasks.sort_by(|a, b| {
        column_index(a.status)
            .cmp(&column_index(b.status))
            .then(a.position.total_cmp(&b.position))
    });
    tasks.into_iter().map(card).collect()
}

fn card(task: Task) -> TaskCard {
    let (subtasks_done, subtasks_total) = with(|m| {
        let children = m
            .tasks
            .iter()
            .filter(|t| t.parent_id.as_deref() == Some(task.id.as_str()));
        children.fold((0, 0), |(done, total), t| {
            (done + u32::from(t.status == TaskStatus::Done), total + 1)
        })
    });
    let mut card = TaskCard {
        task,
        attempt_id: None,
        attempt_state: None,
        branch: None,
        running: false,
        pending_approvals: 0,
        last_status: None,
        last_stop_reason: None,
        worktree_state: None,
        subtasks_done,
        subtasks_total,
    };
    super::attempt::decorate(&mut card);
    if card.attempt_id.is_none() && FLAGS.with(|f| f.badges) {
        demo_badges(&mut card);
    }
    card
}

/// Demo attempt fields (off with `?badges=0`) that exercise every card badge.
fn demo_badges(card: &mut TaskCard) {
    use ProcessStatus::*;
    let (state, last_status, stop, worktree, running, approvals) = match card.task.id.as_str() {
        "task-pagination" => (
            AttemptState::Active,
            Running,
            None,
            WorktreeState::Present,
            true,
            1,
        ),
        "task-config" => (
            AttemptState::Active,
            Failed,
            Some(StopReason::Crash),
            WorktreeState::Missing,
            false,
            0,
        ),
        "task-logger" => (
            AttemptState::Active,
            Failed,
            Some(StopReason::AppRestart),
            WorktreeState::Present,
            false,
            0,
        ),
        "task-pagination-ui" => (
            AttemptState::Active,
            Running,
            None,
            WorktreeState::Present,
            true,
            0,
        ),
        "task-pagination-api" => (
            AttemptState::Merged,
            Completed,
            None,
            WorktreeState::Removed,
            false,
            0,
        ),
        "task-dead-code" => (
            AttemptState::Merged,
            Completed,
            None,
            WorktreeState::Removed,
            false,
            0,
        ),
        "task-images" => (
            AttemptState::Active,
            Completed,
            None,
            WorktreeState::Present,
            false,
            0,
        ),
        "task-websocket" => (
            AttemptState::Discarded,
            Killed,
            Some(StopReason::UserStop),
            WorktreeState::Removed,
            false,
            0,
        ),
        _ => return,
    };
    let slug = card.task.id.trim_start_matches("task-");
    card.attempt_id = Some(format!("attempt-{slug}"));
    card.attempt_state = Some(state);
    card.branch = Some(format!("atm/1a2b3c4d-{slug}"));
    card.running = running;
    card.pending_approvals = approvals;
    card.last_status = Some(last_status);
    card.last_stop_reason = stop;
    card.worktree_state = Some(worktree);
}
