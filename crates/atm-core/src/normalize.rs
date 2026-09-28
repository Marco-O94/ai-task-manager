//! Pure normalizer: CLI stream-json lines → transcript entry operations (spec §7.6).
//! Owner: M2-CLAUDE. Golden-tested with insta; no I/O, no clock (timestamps are passed in).
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code)]

use atm_types::{ApprovalDecision, Entry, Id, Level, LimitKind, Millis, NoticeAction};
use serde_json::Value;

/// Text field cap (spec §7.6 "Limiti").
pub const MAX_TEXT: usize = 256 << 10;
/// `ToolCall.input` cap.
pub const MAX_INPUT: usize = 4 << 10;
/// `ToolCall.input` cap once the call asked for approval: the card shows the full input.
pub const MAX_APPROVAL_INPUT: usize = MAX_TEXT;
/// `ToolOutput.text` cap (head + tail).
pub const MAX_OUTPUT: usize = 8 << 10;
/// `Stderr` entry cap; lines closer than [`STDERR_MERGE_MS`] share one entry.
pub const MAX_STDERR_ENTRY: usize = 64 << 10;
pub const STDERR_MERGE_MS: Millis = 2_000;
/// Minimum interval between two `Typing(Some)` ops.
pub const TYPING_INTERVAL_MS: Millis = 100;

/// Case-insensitive substrings of a failed `result` (subtype, text) and their class, checked
/// in order (spec §7.6: "Not logged in", "/login", "Login expired", "limit",
/// `billing_error`). M5 refines them against real captures.
pub const LIMIT_PATTERNS: &[(&str, LimitKind)] = &[
    ("not logged in", LimitKind::AuthFailure),
    ("/login", LimitKind::AuthFailure),
    ("login expired", LimitKind::AuthFailure),
    ("billing_error", LimitKind::Billing),
    ("rate limit", LimitKind::RateLimit),
    ("limit", LimitKind::UsageLimit),
];

/// Case-insensitive substring of the CLI's stderr (or `result` text) when `--resume` names a
/// session it cannot find (spec §7.9): the runner then adds a Notice with
/// [`NoticeAction::NewSession`]. M5 checks it against the real CLI.
pub const RESUME_FAILED_PATTERN: &str = "no conversation found";

/// One output of the normalizer: persist then broadcast an entry, or update the ephemeral
/// typing preview (never persisted).
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // short-lived, consumed at once; shape fixed by spec §7.6
pub enum EntryOp {
    Upsert(Entry),
    Typing(Option<String>),
}

/// The fields of a `result` line the runner needs (process row, pause, auth invalidation).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    pub subtype: String,
    pub is_error: bool,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u32>,
    pub cost_usd_estimate: Option<f64>,
    pub permission_denials: u32,
    pub text: Option<String>,
    pub limit: Option<LimitKind>,
}

/// Parses a `{"type":"result",…}` line, classifying `limit` with [`LIMIT_PATTERNS`] when
/// `is_error`. `None` for any other line.
pub fn parse_result(line: &Value) -> Option<TurnResult> {
    todo!("M2-CLAUDE: parse_result")
}

/// One-line summary of a tool call (spec §7.6 table); paths relative to `worktree`.
pub fn tool_summary(name: &str, input: &Value, worktree: &str) -> String {
    todo!("M2-CLAUDE: tool_summary")
}

/// State for one turn (one process). Entries get consecutive `idx` from `next_idx`; every
/// update of an existing entry bumps its `rev`.
#[derive(Debug)]
pub struct Normalizer {
    process_id: Id,
    next_idx: u32,
    worktree: String,
}

impl Normalizer {
    /// `next_idx` = `Db::next_entry_idx`; `worktree` (canonical) makes summaries relative and
    /// is compared with the `cwd` of `system/init`.
    pub fn new(process_id: Id, next_idx: u32, worktree: String) -> Normalizer {
        Normalizer {
            process_id,
            next_idx,
            worktree,
        }
    }

    /// `UserMessage` for the prompt sent on stdin.
    pub fn on_user_message(&mut self, text: &str, ts: Millis) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_user_message")
    }

    /// A routed stdout line (`Inbound::Message` or `Inbound::StreamEvent`), per the §7.6
    /// table; unknown types yield nothing.
    pub fn on_line(&mut self, line: &Value, ts: Millis) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_line")
    }

    /// One stderr line (ANSI stripped, merged with the previous one if < 2 s apart).
    pub fn on_stderr(&mut self, line: &str, ts: Millis) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_stderr")
    }

    /// The matching `ToolCall` → `AwaitingApproval`; created from `request` (the
    /// `can_use_tool` object) if the `tool_use` has not arrived yet. `reason` comes from the
    /// request's optional `decision_reason` (shape to confirm in M5: a string, else a short
    /// rendering of the object); `input` is kept up to [`MAX_APPROVAL_INPUT`].
    pub fn on_approval_requested(
        &mut self,
        approval_id: &str,
        request: &Value,
        can_remember: bool,
        ts: Millis,
    ) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_approval_requested")
    }

    /// Allow → `Running`; deny → `Denied{message}`.
    pub fn on_approval_resolved(
        &mut self,
        approval_id: &str,
        decision: &ApprovalDecision,
    ) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_approval_resolved")
    }

    /// `control_cancel_request`: the `ToolCall` leaves `AwaitingApproval` (→ `Cancelled`).
    pub fn on_approval_cancelled(&mut self, approval_id: &str) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_approval_cancelled")
    }

    pub fn on_notice(
        &mut self,
        level: Level,
        text: &str,
        action: Option<NoticeAction>,
        ts: Millis,
    ) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::on_notice")
    }

    /// End of the process: every `ToolCall` still `Running`/`AwaitingApproval` → `Cancelled`,
    /// plus `Typing(None)`.
    pub fn finish(&mut self, ts: Millis) -> Vec<EntryOp> {
        todo!("M2-CLAUDE: Normalizer::finish")
    }
}
