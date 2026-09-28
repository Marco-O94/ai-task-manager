//! Mock of env, settings, projects and board commands (see `BOARD_COMMANDS` in `mod.rs`).
//! Owner: M2-UI-BOARD, which replaces this M1 baseline with a stateful mock and keeps the
//! `pub fn` hooks. The baseline is fixed and read-only (logged in, one project, one task per
//! column), so the task panel can be built without the board: `/?task=task-inreview`
//! opens a panel (see `app.rs`); mutations answer `NotImplemented`.

use std::cell::RefCell;

use atm_types::*;
use serde_json::Value;

const PROJECT_ID: &str = "project-demo";

thread_local! {
    static ENV: RefCell<EnvStatus> = RefCell::new(baseline_env());
}

/// Serves one board-side command: `req` is the serialized `C::Req`, the result the
/// serialized `C::Res`. Emits `changed` through `super::emit` after each mutation.
pub async fn handle(cmd: &str, req: Value) -> Result<Value, AppError> {
    let _ = req;
    let res = match cmd {
        GetEnv::NAME => serde_json::to_value(ENV.with_borrow(Clone::clone)),
        GetSettings::NAME => serde_json::to_value(Settings::default()),
        ListProjects::NAME => serde_json::to_value(vec![project()]),
        ListBranches::NAME => serde_json::to_value(BranchList {
            current: Some("main".into()),
            branches: vec!["develop".into(), "main".into()],
        }),
        GetBoard::NAME => serde_json::to_value(tasks().into_iter().map(card).collect::<Vec<_>>()),
        _ => return Err(AppError::not_implemented(cmd)),
    };
    Ok(res?)
}

/// For `attempt.rs`: the mock task with this id.
#[allow(dead_code)] // hook for attempt.rs
pub fn task(id: &str) -> Option<Task> {
    tasks().into_iter().find(|t| t.id == id)
}

/// For `attempt.rs`: lifecycle transition of a task (start → inprogress, turn end →
/// inreview, merge → done, discard → todo), appended to the end of the new column.
#[allow(dead_code)] // hook for attempt.rs; the baseline is read-only
pub fn set_task_status(id: &str, status: TaskStatus) {
    let _ = (id, status);
}

/// For `attempt.rs`: changes the mock env (usage-limit `paused`, `auth` after an auth
/// failure, `running`) and emits `env_changed`, as the runner does.
#[allow(dead_code)] // hook for attempt.rs
pub fn update_env(f: impl FnOnce(&mut EnvStatus)) {
    let env = ENV.with_borrow_mut(|env| {
        f(env);
        env.checked_at = super::now_ms();
        env.clone()
    });
    super::emit(EVENT_ENV_CHANGED, &env);
}

fn baseline_env() -> EnvStatus {
    EnvStatus {
        claude: ClaudeInfo {
            path: Some("/Users/demo/.local/bin/claude".into()),
            version: Some(CLAUDE_TESTED_VERSION.into()),
            supported: true,
            min_version: CLAUDE_MIN_VERSION.into(),
            tested_version: CLAUDE_TESTED_VERSION.into(),
        },
        auth: AuthState::LoggedIn {
            auth_method: Some("claude.ai".into()),
            api_provider: Some("firstParty".into()),
            email: Some("demo@example.com".into()),
            org_name: None,
            subscription_type: Some("max".into()),
        },
        git_version: Some("2.50.1".into()),
        api_key_in_env: false,
        cloud_provider_env: false,
        paused: None,
        running: 0,
        max_running: Settings::default().max_running,
        problems: Vec::new(),
        checked_at: super::now_ms(),
    }
}

fn project() -> Project {
    Project {
        id: PROJECT_ID.into(),
        name: "demo".into(),
        repo_path: "/Users/demo/demo".into(),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: None,
        config_policy: ConfigPolicy::Isolated,
        trusted: false,
        allow_bypass: false,
        created_at: 0,
        updated_at: 0,
    }
}

fn tasks() -> Vec<Task> {
    [
        ("task-todo", "Aggiungi una riga al README", TaskStatus::Todo),
        (
            "task-inprogress",
            "Rinomina il modulo di parsing",
            TaskStatus::InProgress,
        ),
        (
            "task-inreview",
            "Correggi il test instabile",
            TaskStatus::InReview,
        ),
        ("task-done", "Aggiorna le dipendenze", TaskStatus::Done),
    ]
    .into_iter()
    .map(|(id, title, status)| Task {
        id: id.into(),
        project_id: PROJECT_ID.into(),
        title: title.into(),
        description: "Task di esempio del mock.".into(),
        status,
        position: 1024.0,
        created_at: 0,
        updated_at: 0,
    })
    .collect()
}

fn card(task: Task) -> TaskCard {
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
    };
    super::attempt::decorate(&mut card);
    card
}
