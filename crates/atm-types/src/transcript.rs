//! Normalized transcript and its live channel (spec §6.2, §6.5, §7.6).

use serde::{Deserialize, Serialize};

use crate::{Id, Millis};

/// One transcript row. `(attempt, idx)` is the key; `rev` grows on every upsert, and the UI
/// applies an upsert only if its `rev` is greater than the one it holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Monotonic per attempt, across turns.
    pub idx: u32,
    pub rev: u32,
    pub process_id: Id,
    pub ts: Millis,
    /// Set for rows produced inside a subagent (`Task`/`Agent` tool): nesting in the UI.
    pub parent_tool_use_id: Option<String>,
    pub body: EntryBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EntryBody {
    UserMessage {
        text: String,
    },
    SessionInit {
        model: Option<String>,
        permission_mode: Option<String>,
        api_key_source: Option<String>,
        mcp_servers: u32,
        warnings: Vec<String>,
    },
    AssistantText {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        tool_use_id: String,
        name: String,
        /// One line, paths relative to the worktree (spec §7.6).
        summary: String,
        /// Tool input as JSON text, cut at 4 KiB; a call that asked for approval keeps up to
        /// 256 KiB, so the approval card can show it in full (spec §7.6, §7.8).
        input: String,
        status: ToolStatus,
        output: Option<ToolOutput>,
    },
    /// One per process, updated in place.
    ApiRetry {
        attempt: u32,
        max_retries: u32,
        delay_ms: u64,
        error: String,
    },
    TurnEnd {
        subtype: String,
        is_error: bool,
        duration_ms: Option<u64>,
        num_turns: Option<u32>,
        cost_usd_estimate: Option<f64>,
        permission_denials: u32,
        text: Option<String>,
        limit: Option<LimitKind>,
    },
    Notice {
        level: Level,
        text: String,
        /// A button the UI shows with the notice.
        action: Option<NoticeAction>,
    },
    /// Lines less than 2 s apart share one entry (at most 64 KiB, ANSI stripped).
    Stderr {
        text: String,
    },
}

impl EntryBody {
    /// The serde tag (`"ToolCall"`, ...), stored in `entries.kind`.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "UserMessage",
            Self::SessionInit { .. } => "SessionInit",
            Self::AssistantText { .. } => "AssistantText",
            Self::Thinking { .. } => "Thinking",
            Self::ToolCall { .. } => "ToolCall",
            Self::ApiRetry { .. } => "ApiRetry",
            Self::TurnEnd { .. } => "TurnEnd",
            Self::Notice { .. } => "Notice",
            Self::Stderr { .. } => "Stderr",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum ToolStatus {
    Running,
    /// `approval_id` is the app's id for `respond_approval`; `can_remember` enables
    /// "Consenti sempre (attempt)"; `reason` is why the CLI asks, when it says (spec §7.8).
    AwaitingApproval {
        approval_id: Id,
        can_remember: bool,
        reason: Option<String>,
    },
    Denied {
        message: String,
    },
    Succeeded,
    Failed,
    /// Still open when the process ended.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// At most 8 KiB: head and tail of the original.
    pub text: String,
    pub truncated_bytes: u64,
    pub is_error: bool,
}

/// Action attached to a `Notice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NoticeAction {
    /// "Nuova sessione" after a failed resume (`No conversation found`, spec §7.9): sends
    /// `send_follow_up{fresh_session: true}`.
    NewSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Classification of a failed `result` (`LIMIT_PATTERNS`, spec §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LimitKind {
    UsageLimit,
    RateLimit,
    AuthFailure,
    Billing,
}

/// Message on the per-view transcript `Channel` (spec §6.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum TranscriptMsg {
    /// Replaces the UI store: the last entries (ascending `idx`) and the typing preview.
    Snapshot {
        entries: Vec<Entry>,
        has_more: bool,
        typing: Option<String>,
    },
    /// Apply per `idx`, only if `rev` is greater than the current one.
    Upsert { entries: Vec<Entry> },
    /// Ephemeral typing preview; `None` clears it.
    Typing { text: Option<String> },
}

/// A page of entries in ascending `idx`; `has_more` = older entries exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryPage {
    pub entries: Vec<Entry>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ApprovalDecision {
    /// `remember` = "Consenti sempre (attempt)": only honoured when `can_remember`.
    Allow { remember: bool },
    /// `interrupt` = "Nega e ferma".
    Deny { message: String, interrupt: bool },
}
