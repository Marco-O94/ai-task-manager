//! Board, project, attempt and environment types (spec §5.3, §6.2).

use serde::{Deserialize, Serialize};

use crate::{AppError, Id, Millis};

/// Oldest CLI accepted without the "Continua comunque" banner (spec §7.1).
pub const CLAUDE_MIN_VERSION: &str = "2.1.223";
/// Newest CLI the app was tested with; newer ones only get an informative badge.
pub const CLAUDE_TESTED_VERSION: &str = "2.1.283";

/// `--model` aliases of the CLI 2.1.284, the only list the UI offers (the main agent's model,
/// the sub-agents' `CLAUDE_CODE_SUBAGENT_MODEL`). A full model id stays accepted where a
/// model is free text (`Project::default_model`, `Settings::default_model`).
pub const MODEL_ALIASES: &[&str] = &["opus", "sonnet", "haiku", "fable"];
/// Upper bound of `StartAttemptReq::max_subagents` (sub-agents one task's main agent may
/// spawn; `attempts.max_subagents` CHECK).
pub const MAX_SUBAGENTS: u8 = 10;
/// Attachments one task may have (re-counted in the insertion's transaction).
pub const MAX_ATTACHMENTS_PER_TASK: usize = 20;
/// Largest attachment accepted, in bytes (25 MiB).
pub const MAX_ATTACHMENT_BYTES: u64 = 25 << 20;
/// Longest `Project::description`, in characters (`projects.description` CHECK).
pub const MAX_PROJECT_DESCRIPTION: usize = 10_000;
/// Bounds and default of `Project::verify_timeout_secs` (`projects` CHECK).
pub const VERIFY_TIMEOUT_SECS: std::ops::RangeInclusive<u32> = 10..=3600;
pub const DEFAULT_VERIFY_TIMEOUT_SECS: u32 = 600;
/// Upper bound and default of `Project::autopilot_max_fixes` (`projects` CHECK).
pub const MAX_AUTOPILOT_FIXES: u32 = 5;
pub const DEFAULT_AUTOPILOT_MAX_FIXES: u32 = 2;
/// Longest `Project::verify_command`, in characters.
pub const MAX_VERIFY_COMMAND: usize = 1000;

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

db_enum! {
    /// `attempts.verify_state`: the autopilot's run of the project's `verify_command` on the
    /// attempt's worktree (`None` = never verified).
    VerifyState {
        Running = "running",
        Passed = "passed",
        Failed = "failed",
        /// Could not run (spawn error, timeout, the app closed while it ran).
        Error = "error",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    /// Free text shown on the overview page, at most [`MAX_PROJECT_DESCRIPTION`] characters;
    /// empty by default.
    pub description: String,
    /// Canonical toplevel of the main checkout.
    pub repo_path: String,
    pub default_target_branch: String,
    pub default_permission_mode: PermissionMode,
    pub default_model: Option<String>,
    pub config_policy: ConfigPolicy,
    /// Trust in effect: `config_policy` is Trusted and the approved fingerprint still matches
    /// the configuration committed at the tip of the default target branch, where new
    /// worktrees start (spec §8.9). Trusted with a stale fingerprint → `false` (new attempts
    /// run Isolated until the user approves again, with a native confirmation).
    pub trusted: bool,
    pub allow_bypass: bool,
    pub created_at: Millis,
    pub updated_at: Millis,
    /// Policy Trusted and the target branch's configuration cannot be fingerprinted or
    /// approved (a limit, a link out of the repository, the home directory, a branch that
    /// cannot be read, settings that would bill outside the subscription): why, for the
    /// settings (M6). `None` otherwise, and absent from the JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_error: Option<String>,
    /// Autopilot on: the app starts, verifies, fixes and (with `autopilot_merge`) merges the
    /// project's tasks with `Task::auto` on its own.
    #[serde(default)]
    pub autopilot: bool,
    /// Merge an attempt once its verification passed; off = notify "pronto per il merge".
    #[serde(default)]
    pub autopilot_merge: bool,
    /// Run by `/bin/sh -c` in the attempt's worktree after a completed turn; `None` = no
    /// verification (counts as passed). Trimmed, at most [`MAX_VERIFY_COMMAND`] characters.
    #[serde(default)]
    pub verify_command: Option<String>,
    /// In [`VERIFY_TIMEOUT_SECS`].
    #[serde(default = "default_verify_timeout_secs")]
    pub verify_timeout_secs: u32,
    /// Follow-ups sent after a failed verification, per attempt, 0..=[`MAX_AUTOPILOT_FIXES`].
    #[serde(default = "default_autopilot_max_fixes")]
    pub autopilot_max_fixes: u32,
}

pub(crate) fn default_verify_timeout_secs() -> u32 {
    DEFAULT_VERIFY_TIMEOUT_SECS
}

pub(crate) fn default_autopilot_max_fixes() -> u32 {
    DEFAULT_AUTOPILOT_MAX_FIXES
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
    /// Parent task for a sub-task (same project, one level only: a parent is never itself a
    /// sub-task); `None` = top-level task. Deleting the parent deletes its sub-tasks.
    #[serde(default)]
    pub parent_id: Option<Id>,
    /// Entrusted to the project's autopilot.
    #[serde(default)]
    pub auto: bool,
    /// Starts only once this task (same project, not itself) is done; `None` = no dependency.
    /// Deleting that task clears it.
    #[serde(default)]
    pub after_id: Option<Id>,
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
    /// Sub-tasks of this task in status `done` (the parent's progress "n/m").
    #[serde(default)]
    pub subtasks_done: u32,
    /// Every sub-task of this task, whatever its status; 0 for a sub-task.
    #[serde(default)]
    pub subtasks_total: u32,
    /// The autopilot is running the verification of the active attempt now (live registry).
    #[serde(default)]
    pub verifying: bool,
    /// `AttemptView::verify_state` of the active attempt; `None` without one.
    #[serde(default)]
    pub verify_state: Option<VerifyState>,
    /// `AttemptView::verify_fixes` of the active attempt; 0 without one.
    #[serde(default)]
    pub verify_fixes: u32,
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
    /// `CLAUDE_CODE_SUBAGENT_MODEL` of every turn (one of [`MODEL_ALIASES`]); `None` = the
    /// CLI's default. A sub-agent that asks for its own model may still use another one.
    pub subagent_model: Option<String>,
    /// Sub-agents the main agent may spawn over the whole attempt (0..=[`MAX_SUBAGENTS`]);
    /// `None` = no limit (the argv is the one without this feature).
    pub max_subagents: Option<u8>,
    /// Sub-agent spawns allowed so far (counted by the host, persisted).
    pub subagents_used: u32,
    pub session_started: bool,
    pub merge_commit: Option<String>,
    pub running: bool,
    pub pending_approvals: u32,
    pub created_at: Millis,
    pub closed_at: Option<Millis>,
    /// Attempt whose agent started this one through the board tools (`start_task`); `None` =
    /// started by the user. Plain id, not a foreign key: it survives the starter's deletion.
    #[serde(default)]
    pub started_by_attempt: Option<Id>,
    /// Latest verification; `None` = never verified.
    #[serde(default)]
    pub verify_state: Option<VerifyState>,
    /// Commit the latest verification ran on (HEAD before it started).
    #[serde(default)]
    pub verify_head: Option<String>,
    /// Follow-ups the autopilot sent after a failed verification (or a merge conflict).
    #[serde(default)]
    pub verify_fixes: u32,
    /// Tail of the latest verification's output (at most 8 KiB); the full output is in the
    /// attempt's logs.
    #[serde(default)]
    pub verify_summary: Option<String>,
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
    /// Files attached to the task, oldest first.
    pub attachments: Vec<Attachment>,
    /// Cards of the task's sub-tasks, in board order (column, then position).
    #[serde(default)]
    pub subtasks: Vec<TaskCard>,
}

/// A file attached to a task: a copy in the app's data dir (never in a worktree, never
/// committed), read-only for the agents (spec F5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Id,
    pub task_id: Id,
    /// File name as picked (sanitized), also the copy's name.
    pub name: String,
    /// Bytes, at most [`MAX_ATTACHMENT_BYTES`].
    pub size: u64,
    /// Absolute path of the copy, computed by the core:
    /// `<data_dir>/attachments/<project_id>/<task_id>/<attachment_id>/<name>`.
    pub path: String,
    pub created_at: Millis,
}

/// A file chosen in the native picker, staged by the core: the webview only ever sees the
/// token, never the path (`add_task_attachments` redeems it once; it expires after 10 min).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickedFile {
    pub token: Id,
    pub name: String,
    pub size: u64,
}

/// What the agents of a project find in its repository (spec F3): read from the commit at
/// the tip of `default_target_branch`, where new worktrees start and what a Trusted approval
/// approves, never from the main checkout nor from `$HOME`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectOverview {
    pub project_id: Id,
    /// `default_target_branch`.
    pub branch: String,
    /// Full hex id of the commit read.
    pub commit: String,
    /// The agents load the repository's configuration (`.claude/`, `.mcp.json`, CLAUDE.md as
    /// memory): policy Trusted and `Project::trusted`.
    pub agents_load_config: bool,
    /// `CLAUDE.md`, `.claude/CLAUDE.md`, `AGENTS.md`, `README.md`, `.claude/settings.json`,
    /// `.mcp.json`: those present in the commit, in this order.
    pub files: Vec<ContextFile>,
    /// Servers of `.mcp.json` (never those of `~/.claude.json`, which the app does not read).
    pub mcp_servers: Vec<McpServer>,
    /// Names under `.claude/agents`, `.claude/commands`, `.claude/skills`, sorted.
    pub claude_agents: Vec<String>,
    pub claude_commands: Vec<String>,
    pub claude_skills: Vec<String>,
}

/// One file of [`ProjectOverview::files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFile {
    /// Relative to the repository root, e.g. `.claude/settings.json`.
    pub path: String,
    pub kind: ContextFileKind,
    /// Bytes of the blob (of the link target string for a symlink).
    pub size: u64,
    /// UTF-8 text with hidden and bidirectional characters shown as `⟨U+XXXX⟩`, secrets masked
    /// (values of `env` and `headers`, `*Helper` and credential helpers, URLs without
    /// userinfo/query/fragment); `None` if too large, binary, a symlink or invalid JSON.
    pub content: Option<String>,
    /// Why `content` is missing or altered (Italian), e.g. `troppo grande (80 KiB)`,
    /// `→ AGENTS.md`, `binario`, `JSON non valido`, `valori segreti mascherati`.
    pub note: Option<String>,
    /// `content`, or the symlink target shown in `note`, had hidden or bidirectional
    /// characters, replaced.
    pub hidden_chars: bool,
    /// The CLI loads this file on its own in this project's agents (not merely readable).
    pub used_by_agents: bool,
    /// How the agents get this file (Italian), e.g. `caricato all'avvio` or `letto
    /// dall'agente su istruzione del prompt`.
    pub usage_note: String,
}

/// What a [`ContextFile`] is to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContextFileKind {
    /// `CLAUDE.md`, `.claude/CLAUDE.md`.
    Memory,
    /// `AGENTS.md`.
    Agents,
    /// `README.md`.
    Readme,
    /// `.claude/settings.json`.
    Settings,
    /// `.mcp.json`.
    Mcp,
}

/// One server of `.mcp.json`: names only, never an env or header value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    /// `type` as declared (`stdio`, `http`, `sse`, …); else `stdio` with a `command`, `http`
    /// with a `url`, else `sconosciuto`.
    pub transport: String,
    /// The command and its arguments (stdio) or the URL without userinfo, query and fragment,
    /// at most 200 characters; empty when there is neither.
    pub target: String,
    /// Names of the `env` entries.
    pub env_keys: Vec<String>,
    /// Names of the `headers` entries.
    pub header_keys: Vec<String>,
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
    /// macOS notifications from the autopilot.
    #[serde(default = "yes")]
    pub notifications: bool,
}

fn yes() -> bool {
    true
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
            notifications: true,
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
    /// `ANTHROPIC_BASE_URL` (or a provider's base URL) is set in the app's environment: the
    /// agents' requests, with the subscription's credentials, go to that endpoint.
    pub base_url_env: bool,
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
