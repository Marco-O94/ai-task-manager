//! Commands and events (spec §6.1, §6.3, §6.4).
//!
//! Every command is `invoke(NAME, {req})` → `Res`, or a rejection carrying [`AppError`].
//! `NAME` is also the name of the `#[tauri::command]` fn. Commands whose request is [`Empty`]
//! take no `req` argument on the Rust side (the UI still sends `{req: {}}`, which is ignored).
//!
//! [`AppError`]: crate::AppError

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{
    ApprovalDecision, Attachment, AttemptView, BranchList, BranchStatus, ConfigPolicy, DiffResult,
    Effort, EntryPage, EnvStatus, Id, LoginMethod, MergeOutcome, OpenTarget, PermissionMode,
    PickedFile, ProcessInfo, Project, ProjectOverview, Settings, TaskCard, TaskDetail, TaskStatus,
};

/// Typed marker of one IPC command.
pub trait Command {
    const NAME: &'static str;
    type Req: Serialize + DeserializeOwned;
    type Res: Serialize + DeserializeOwned;
}

macro_rules! cmd {
    ($(#[$meta:meta])* $t:ident, $n:literal, $req:ty => $res:ty) => {
        $(#[$meta])*
        pub struct $t;
        impl Command for $t {
            const NAME: &'static str = $n;
            type Req = $req;
            type Res = $res;
        }
    };
}
pub(crate) use cmd;

/// Global event: a task, attempt or process changed (emitted after the DB commit).
pub const EVENT_CHANGED: &str = "changed";
/// Global event carrying the new [`EnvStatus`].
pub const EVENT_ENV_CHANGED: &str = "env_changed";

/// Payload of [`EVENT_CHANGED`]. The UI refetches (100 ms debounce): the board if `project_id`
/// is the selected project, the detail if `task_id` is the open task, the project list if
/// `project_id` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub project_id: Option<Id>,
    pub task_id: Option<Id>,
}

/// Follow-up sent by "Continua" on a turn interrupted by the app closing (`app_shutdown` or
/// `app_restart`; card and task panel, spec §7.9); it resumes the session with `--resume`.
pub const CONTINUE_PROMPT: &str =
    "The previous run was interrupted when the app closed. Continue the task.";

/// Request of the commands that take no arguments: serializes as `{}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdReq {
    pub id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectIdReq {
    pub project_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptIdReq {
    pub attempt_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetEnvReq {
    /// Re-read PATH, `claude --version`, `auth status` and git instead of the 60 s cache.
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenLoginTerminalReq {
    pub method: LoginMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddProjectReq {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddProjectRes {
    pub project: Project,
    /// Non-blocking, e.g. `.claude/` or `.mcp.json` present (spec §8.3).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateProjectReq {
    pub id: Id,
    pub name: String,
    pub default_target_branch: String,
    pub default_permission_mode: PermissionMode,
    pub default_model: Option<String>,
    /// Trimmed, at most `MAX_PROJECT_DESCRIPTION` characters (`Invalid`). Absent = empty.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub autopilot: bool,
    #[serde(default)]
    pub autopilot_merge: bool,
    /// Trimmed, empty = `None`, at most `MAX_VERIFY_COMMAND` characters (`Invalid`).
    #[serde(default)]
    pub verify_command: Option<String>,
    /// In `VERIFY_TIMEOUT_SECS` (`Invalid`).
    #[serde(default = "crate::model::default_verify_timeout_secs")]
    pub verify_timeout_secs: u32,
    /// 0..=`MAX_AUTOPILOT_FIXES` (`Invalid`).
    #[serde(default = "crate::model::default_autopilot_max_fixes")]
    pub autopilot_max_fixes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetProjectSecurityReq {
    pub id: Id,
    pub config_policy: ConfigPolicy,
    pub allow_bypass: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTaskReq {
    pub project_id: Id,
    pub title: String,
    pub description: String,
    /// Column to append to; `None` = todo.
    pub status: Option<TaskStatus>,
    /// Creates a sub-task of this task (same project, not itself a sub-task: `Invalid`
    /// otherwise); `None` = top-level task.
    #[serde(default)]
    pub parent_id: Option<Id>,
    /// Entrusted to the autopilot.
    #[serde(default)]
    pub auto: bool,
    /// A task of the same project to start after (`Invalid` otherwise); `None` = none.
    #[serde(default)]
    pub after_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTaskReq {
    pub id: Id,
    pub title: String,
    pub description: String,
    /// `None` (absent) = unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    /// Absent = unchanged, `null` = no dependency, an id = a task of the same project, not
    /// this one (`Invalid` otherwise).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub after_id: Option<Option<Id>>,
}

/// A field that may be `null`: present (even as `null`) → `Some`, absent → `None` (through
/// `#[serde(default)]`).
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveTaskReq {
    pub id: Id,
    pub status: TaskStatus,
    /// Insert before this task of the destination column; `None` = at the end.
    pub before_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartAttemptReq {
    pub task_id: Id,
    pub target_branch: String,
    /// `BypassPermissions` needs the project's `allow_bypass` (`Invalid` otherwise).
    pub permission_mode: PermissionMode,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// One of `MODEL_ALIASES`; `None` = the CLI's default (`Invalid` otherwise).
    pub subagent_model: Option<String>,
    /// 0..=`MAX_SUBAGENTS` (`Invalid` otherwise); `None` = no limit.
    pub max_subagents: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddTaskAttachmentsReq {
    pub task_id: Id,
    /// `PickedFile::token`s from `pick_attachment_files`, each usable once.
    pub tokens: Vec<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendFollowUpReq {
    pub attempt_id: Id,
    pub prompt: String,
    /// Mode of this turn only (stored in the process row); `None` = the attempt's mode, which
    /// never changes. `BypassPermissions` needs the project's `allow_bypass` (`Invalid`).
    pub permission_mode: Option<PermissionMode>,
    /// Start a new CLI session (after a failed resume): the prompt is prefixed with the task
    /// and the attempt's `git log` (spec §7.4).
    pub fresh_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespondApprovalReq {
    pub attempt_id: Id,
    pub approval_id: Id,
    pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribeTranscriptReq {
    pub subscription_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetEntriesReq {
    pub attempt_id: Id,
    /// Entries with `idx < before_idx`, the newest `limit` of them.
    pub before_idx: u32,
    /// At most 200.
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeAttemptReq {
    pub attempt_id: Id,
    /// Squash commit message (editable default, spec §8.7).
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAttemptReq {
    pub attempt_id: Id,
    pub target: OpenTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenUrlReq {
    /// `http(s)` only.
    pub url: String,
}

cmd!(
    /// Cached for 60 s; `force` re-checks everything.
    GetEnv, "get_env", GetEnvReq => EnvStatus
);
cmd!(
    /// Writes the login `.command` script and opens it in Terminal.app (spec §7.10).
    OpenLoginTerminal, "open_login_terminal", OpenLoginTerminalReq => ()
);
cmd!(
    /// Clears the usage-limit pause.
    ResumeAgents, "resume_agents", Empty => EnvStatus
);
cmd!(GetSettings, "get_settings", Empty => Settings);
cmd!(
    /// Enabling `allow_env_api_key` asks for a native confirmation (M6).
    UpdateSettings, "update_settings", Settings => Settings
);
cmd!(ListProjects, "list_projects", Empty => Vec<Project>);
cmd!(
    /// Native folder picker; `None` if cancelled.
    PickRepoFolder, "pick_repo_folder", Empty => Option<String>
);
cmd!(AddProject, "add_project", AddProjectReq => AddProjectRes);
cmd!(UpdateProject, "update_project", UpdateProjectReq => Project);
cmd!(
    /// Raising the level (Trusted, bypass) asks for a native confirmation (M6).
    SetProjectSecurity, "set_project_security", SetProjectSecurityReq => Project
);
cmd!(
    /// `Busy` with running turns; snapshots and removes the worktrees, keeps the branches.
    RemoveProject, "remove_project", IdReq => ()
);
cmd!(ListBranches, "list_branches", ProjectIdReq => BranchList);
cmd!(
    /// Read from the tip of the default target branch, never from `$HOME` (`Invalid`).
    GetProjectOverview, "get_project_overview", ProjectIdReq => ProjectOverview
);
cmd!(GetBoard, "get_board", ProjectIdReq => Vec<TaskCard>);
cmd!(CreateTask, "create_task", CreateTaskReq => TaskCard);
cmd!(UpdateTask, "update_task", UpdateTaskReq => TaskCard);
cmd!(
    /// `Busy` if the task is running and the destination is done or cancelled.
    MoveTask, "move_task", MoveTaskReq => ()
);
cmd!(
    /// `Busy` if running; discards the active attempt.
    DeleteTask, "delete_task", IdReq => ()
);
cmd!(GetTaskDetail, "get_task_detail", IdReq => TaskDetail);
cmd!(
    /// Native multi-file picker; the core stages the files and returns one-use tokens (the
    /// webview never passes a path). Empty if cancelled.
    PickAttachmentFiles, "pick_attachment_files", Empty => Vec<PickedFile>
);
cmd!(
    /// Copies the staged files into the task's folder; `Invalid` past
    /// `MAX_ATTACHMENTS_PER_TASK` or for an unknown or expired token.
    AddTaskAttachments, "add_task_attachments", AddTaskAttachmentsReq => Vec<Attachment>
);
cmd!(
    /// Deletes the attachment's row and its copy.
    RemoveTaskAttachment, "remove_task_attachment", IdReq => ()
);
cmd!(
    /// Returns once the worktree and the rows exist; the spawn is asynchronous.
    StartAttempt, "start_attempt", StartAttemptReq => AttemptView
);
cmd!(
    /// `Busy` while a turn is running.
    SendFollowUp, "send_follow_up", SendFollowUpReq => ProcessInfo
);
cmd!(
    /// Returns at once; the stop escalation continues in the background.
    StopAttempt, "stop_attempt", AttemptIdReq => ()
);
cmd!(RespondApproval, "respond_approval", RespondApprovalReq => ());
cmd!(
    /// Also takes `onEvent: Channel<TranscriptMsg>`; returns the subscription id at once and
    /// then sends a `Snapshot` followed by `Upsert`/`Typing` (spec §6.5). An unknown
    /// `attempt_id` is not an error: the `Snapshot` is empty (`has_more: false`) and the
    /// subscription lives until unsubscribed or the page reloads.
    SubscribeTranscript, "subscribe_transcript", AttemptIdReq => Id
);
cmd!(UnsubscribeTranscript, "unsubscribe_transcript", UnsubscribeTranscriptReq => ());
cmd!(GetEntries, "get_entries", GetEntriesReq => EntryPage);
cmd!(GetDiff, "get_diff", AttemptIdReq => DiffResult);
cmd!(
    /// Includes the conflict preview.
    GetBranchStatus, "get_branch_status", AttemptIdReq => BranchStatus
);
cmd!(MergeAttempt, "merge_attempt", MergeAttemptReq => MergeOutcome);
cmd!(DiscardAttempt, "discard_attempt", AttemptIdReq => ());
cmd!(
    /// Only for `merged` attempts and `atm/…` branches.
    DeleteBranch, "delete_branch", AttemptIdReq => ()
);
cmd!(OpenAttempt, "open_attempt", OpenAttemptReq => ());
cmd!(OpenUrl, "open_url", OpenUrlReq => ());

/// Names of every §6.3 command (debug probes excluded), in table order.
pub const COMMAND_NAMES: &[&str] = &[
    GetEnv::NAME,
    OpenLoginTerminal::NAME,
    ResumeAgents::NAME,
    GetSettings::NAME,
    UpdateSettings::NAME,
    ListProjects::NAME,
    PickRepoFolder::NAME,
    AddProject::NAME,
    UpdateProject::NAME,
    SetProjectSecurity::NAME,
    RemoveProject::NAME,
    ListBranches::NAME,
    GetProjectOverview::NAME,
    GetBoard::NAME,
    CreateTask::NAME,
    UpdateTask::NAME,
    MoveTask::NAME,
    DeleteTask::NAME,
    GetTaskDetail::NAME,
    PickAttachmentFiles::NAME,
    AddTaskAttachments::NAME,
    RemoveTaskAttachment::NAME,
    StartAttempt::NAME,
    SendFollowUp::NAME,
    StopAttempt::NAME,
    RespondApproval::NAME,
    SubscribeTranscript::NAME,
    UnsubscribeTranscript::NAME,
    GetEntries::NAME,
    GetDiff::NAME,
    GetBranchStatus::NAME,
    MergeAttempt::NAME,
    DiscardAttempt::NAME,
    DeleteBranch::NAME,
    OpenAttempt::NAME,
    OpenUrl::NAME,
];
