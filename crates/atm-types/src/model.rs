//! Board, project, attempt and environment types (spec §5.3, §6.2).

use serde::{Deserialize, Serialize};

use crate::{AppError, Id, Millis};

/// Oldest CLI accepted without the "Continua comunque" banner (spec §7.1).
pub const CLAUDE_MIN_VERSION: &str = "2.1.223";
/// Newest CLI the app was tested with; newer ones only get an informative badge.
pub const CLAUDE_TESTED_VERSION: &str = "2.1.283";

/// Enums whose strings are shared with the DB `CHECK`s and the CLI (spec §5.3). Each gets
/// `ALL` (declaration order), `as_str()`, `Display` and `FromStr` (`Invalid` on unknown input),
/// all driven by the same literals as the serde renames.
macro_rules! db_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $s)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The exact string stored in the DB and passed to the CLI.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $s),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = AppError;

            fn from_str(s: &str) -> Result<Self, AppError> {
                match s {
                    $($s => Ok(Self::$variant),)+
                    _ => Err(AppError::invalid(format!(
                        concat!("invalid ", stringify!($name), ": {:?}"),
                        s
                    ))),
                }
            }
        }
    };
}

db_enum! {
    /// Kanban column, in board order.
    TaskStatus {
        Todo = "todo",
        InProgress = "inprogress",
        InReview = "inreview",
        Done = "done",
        Cancelled = "cancelled",
    }
}

db_enum! {
    AttemptState {
        Active = "active",
        Merged = "merged",
        Discarded = "discarded",
    }
}

db_enum! {
    WorktreeState {
        Present = "present",
        Removed = "removed",
        /// Directory gone outside the app; only discard is allowed (spec §8.4).
        Missing = "missing",
    }
}

db_enum! {
    ProcessStatus {
        Running = "running",
        Completed = "completed",
        Failed = "failed",
        Killed = "killed",
    }
}

db_enum! {
    /// `--permission-mode=<value>` (spec D6): Supervisionato, Auto-edit, Autonomo.
    PermissionMode {
        Default = "default",
        AcceptEdits = "acceptEdits",
        BypassPermissions = "bypassPermissions",
    }
}

db_enum! {
    /// Claude configuration of the repo (spec D7).
    ConfigPolicy {
        Isolated = "isolated",
        Trusted = "trusted",
    }
}

db_enum! {
    /// `--effort=<value>`.
    Effort {
        Low = "low",
        Medium = "medium",
        High = "high",
        XHigh = "xhigh",
        Max = "max",
    }
}

db_enum! {
    /// `processes.stop_reason` (spec §5.2, classification §7.7).
    StopReason {
        UserStop = "user_stop",
        AppShutdown = "app_shutdown",
        AppRestart = "app_restart",
        SpawnError = "spawn_error",
        InitTimeout = "init_timeout",
        ExitTimeout = "exit_timeout",
        Crash = "crash",
        AuthFailure = "auth_failure",
        UsageLimit = "usage_limit",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    /// Canonical toplevel of the main checkout.
    pub repo_path: String,
    pub default_target_branch: String,
    pub default_permission_mode: PermissionMode,
    pub default_model: Option<String>,
    pub config_policy: ConfigPolicy,
    /// Trust in effect: `config_policy` is Trusted and the approved fingerprint still matches
    /// the main checkout (spec §8.9). Trusted with a stale fingerprint → `false` (turns run
    /// Isolated until the user approves again, with a native confirmation).
    pub trusted: bool,
    pub allow_bypass: bool,
    pub created_at: Millis,
    pub updated_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Id,
    pub project_id: Id,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    /// Order inside the column; computed only by the backend (spec §5.2).
    pub position: f64,
    pub created_at: Millis,
    pub updated_at: Millis,
}

/// One board card: the task plus its active (else most recent) attempt, merged with the
/// in-memory registry (`running`, `pending_approvals`). The attempt fields are all `None`
/// when the task has no attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCard {
    pub task: Task,
    pub attempt_id: Option<Id>,
    /// `Some(Active)` = the task has an active attempt (a drop on "In corso" moves it instead
    /// of opening the Start dialog; "Interrotto – Continua" only applies to it).
    pub attempt_state: Option<AttemptState>,
    pub branch: Option<String>,
    pub running: bool,
    pub pending_approvals: u32,
    /// Status of the attempt's latest process.
    pub last_status: Option<ProcessStatus>,
    pub last_stop_reason: Option<StopReason>,
    pub worktree_state: Option<WorktreeState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptView {
    pub id: Id,
    pub task_id: Id,
    pub state: AttemptState,
    pub branch: String,
    pub target_branch: String,
    pub base_commit: String,
    pub worktree_path: String,
    pub worktree_state: WorktreeState,
    pub permission_mode: PermissionMode,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub session_started: bool,
    pub merge_commit: Option<String>,
    pub running: bool,
    pub pending_approvals: u32,
    pub created_at: Millis,
    pub closed_at: Option<Millis>,
}

/// One turn (one `claude -p` process).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub id: Id,
    pub seq: u32,
    pub prompt: String,
    pub status: ProcessStatus,
    pub stop_reason: Option<StopReason>,
    pub result_subtype: Option<String>,
    pub is_error: Option<bool>,
    /// Client-side estimate reported by the CLI; the UI labels it "≈ stima API".
    pub cost_usd_estimate: Option<f64>,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u32>,
    pub head_after: Option<String>,
    pub started_at: Millis,
    pub finished_at: Option<Millis>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task: Task,
    /// The active attempt, if any.
    pub attempt: Option<AttemptView>,
    /// Turns of `attempt`, by `seq`.
    pub processes: Vec<ProcessInfo>,
    /// Merged and discarded attempts, oldest first.
    pub closed_attempts: Vec<AttemptView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchList {
    /// Branch checked out in the main checkout (`None` if detached).
    pub current: Option<String>,
    /// Local branches (`refs/heads`), short names.
    pub branches: Vec<String>,
}

/// The `settings` table (spec §5.2); `Default` gives the documented defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub claude_path_override: Option<String>,
    pub default_model: Option<String>,
    /// 1..=6.
    pub max_running: u32,
    pub allow_env_api_key: bool,
    pub worktree_root: String,
    pub editor_app: String,
    pub remove_worktree_after_merge: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            claude_path_override: None,
            default_model: None,
            max_running: 2,
            allow_env_api_key: false,
            worktree_root: "~/.ai-task-manager/worktrees".into(),
            editor_app: "Visual Studio Code".into(),
            remove_worktree_after_merge: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeInfo {
    /// Symlink path as discovered, never canonicalized (the CLI self-updates).
    pub path: Option<String>,
    pub version: Option<String>,
    /// `version >= min_version`.
    pub supported: bool,
    /// [`CLAUDE_MIN_VERSION`].
    pub min_version: String,
    /// [`CLAUDE_TESTED_VERSION`].
    pub tested_version: String,
}

/// Result of `claude auth status --json` (spec §7.10). Email and org live only in memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum AuthState {
    LoggedIn {
        auth_method: Option<String>,
        api_provider: Option<String>,
        email: Option<String>,
        org_name: Option<String>,
        subscription_type: Option<String>,
    },
    LoggedOut,
    /// Exit code other than 0/1, timeout or unparsable output.
    Unknown {
        reason: String,
    },
}

/// Payload of `get_env`, `resume_agents` and the `env_changed` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvStatus {
    pub claude: ClaudeInfo,
    pub auth: AuthState,
    pub git_version: Option<String>,
    /// `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` present in the app's environment.
    pub api_key_in_env: bool,
    /// A third-party provider (Bedrock, Vertex, ...) is selected by the environment.
    pub cloud_provider_env: bool,
    /// Usage-limit text while new turns are paused (spec §7.9).
    pub paused: Option<String>,
    /// Turns running now across all projects, and `settings.max_running`: the topbar's
    /// "in esecuzione x/y". Core emits `env_changed` whenever either changes.
    pub running: u32,
    pub max_running: u32,
    pub problems: Vec<String>,
    /// When this status was assembled (`paused` and `running` are always current, the rest
    /// may come from the 60 s cache). `get_env` replies and `env_changed` events can arrive
    /// out of order: the UI keeps the newest.
    pub checked_at: Millis,
}

/// `claude auth login` flavour: ClaudeAi → no flag, Console → `--console`, Sso → `--sso`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoginMethod {
    ClaudeAi,
    Console,
    Sso,
}

/// `open_attempt` target: Finder, Terminal.app or `settings.editor_app`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpenTarget {
    Finder,
    Terminal,
    Editor,
}
