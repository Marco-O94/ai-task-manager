//! NDJSON wire protocol with `claude -p --input-format stream-json` (spec §7.4, §7.5, §7.8):
//! capped line reader, inbound classification, outbound frames, approval responses.
//! Owner: M2-CLAUDE. Everything except the reader and the writer is pure.
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code, clippy::ptr_arg)]

use atm_types::{ApprovalDecision, Id};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::mpsc;

/// Per-line caps (spec §7.5, §7.11).
pub const MAX_STDOUT_LINE: usize = 16 << 20;
pub const MAX_STDERR_LINE: usize = 64 << 10;
/// Capacity of the stdin frame channel (single writer task).
pub const STDIN_CHANNEL: usize = 64;

/// Deny text sent before the user's message (spec §7.8).
pub const DENY_PREFIX: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said: ";
/// Deny message for `AskUserQuestion`, which is disallowed in v1 but may still arrive.
pub const ASK_USER_QUESTION_DENY: &str = "Ask your question in plain text in your reply instead.";

/// Outcome of [`read_line_capped`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// `buf` holds one line without its `\n` (the last line may lack it).
    Complete,
    /// The line exceeded the cap and was discarded up to its `\n`; the payload is its length.
    TooLong(usize),
    Eof,
}

/// Reads one line into `buf` (cleared first) using `fill_buf`/`consume`; past `max` bytes
/// discards the rest of the line and returns `TooLong` so the caller keeps reading.
pub async fn read_line_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Line> {
    Err(std::io::Error::other("M2-CLAUDE: read_line_capped"))
}

/// Fields of a `control_request` with subtype `can_use_tool` (spec §7.8).
#[derive(Debug, Clone, PartialEq)]
pub struct CanUseTool {
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: String,
    /// Raw `permission_suggestions` (`Value::Null` when absent).
    pub permission_suggestions: Value,
    /// The whole `request` object, for `Normalizer::on_approval_requested`.
    pub request: Value,
}

/// One stdout line, classified for routing (spec §7.4 table).
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// Resolves a pending host request: `Ok(response)` or `Err(error text)`.
    ControlResponse {
        request_id: String,
        result: Result<Value, String>,
    },
    CanUseTool(CanUseTool),
    /// Any other subtype, or a `can_use_tool` missing a required field: answered with
    /// [`control_error`] plus a warning Notice.
    ControlRequest {
        request_id: String,
        subtype: String,
    },
    /// Cancels the approval with this `request_id`; never answered.
    ControlCancel {
        request_id: String,
    },
    KeepAlive,
    /// Typing preview only; not written to the raw log.
    StreamEvent(Value),
    /// Everything else: to the normalizer and the raw log.
    Message(Value),
    /// Not starting with `{` or not JSON: raw log only.
    NotJson,
}

pub fn parse(line: &[u8]) -> Inbound {
    todo!("M2-CLAUDE: wire::parse")
}

/// `atm_<n>_<8 hex>`: id of a request sent by the host.
pub fn request_id(n: u64) -> String {
    todo!("M2-CLAUDE: request_id")
}

/// `{"type":"control_request","request_id":…,"request":{"subtype":"initialize","hooks":null}}`.
pub fn initialize_request(request_id: &str) -> Value {
    todo!("M2-CLAUDE: initialize_request")
}

/// `{"type":"control_request","request_id":…,"request":{"subtype":"interrupt"}}`.
pub fn interrupt_request(request_id: &str) -> Value {
    todo!("M2-CLAUDE: interrupt_request")
}

/// `{"type":"user","message":{"role":"user","content":<prompt>},"parent_tool_use_id":null}`
/// (no `session_id`).
pub fn user_message(prompt: &str) -> Value {
    todo!("M2-CLAUDE: user_message")
}

/// `control_response` with subtype `success` echoing `request_id`.
pub fn control_success(request_id: &str, response: Value) -> Value {
    todo!("M2-CLAUDE: control_success")
}

/// `control_response` with `{"subtype":"error","request_id":…,"error":<error>}`, e.g.
/// `"Unsupported control request subtype: X"`.
pub fn control_error(request_id: &str, error: &str) -> Value {
    todo!("M2-CLAUDE: control_error")
}

/// A `can_use_tool` waiting for the user (in memory only, no timeout; spec §7.8).
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    /// App-side id shown in `ToolStatus::AwaitingApproval`.
    pub approval_id: Id,
    /// The CLI's id, echoed in the response.
    pub request_id: String,
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: Value,
    /// Raw `permission_suggestions`.
    pub suggestions: Value,
}

/// True only if every suggestion is `addRules` with `behavior:"allow"` and every rule has
/// a non-empty `ruleContent` (whole-tool rules are never rememberable).
pub fn can_remember(suggestions: &Value) -> bool {
    todo!("M2-CLAUDE: can_remember")
}

/// Complete `control_response` frame for a decision (spec §7.8 table), echoing
/// `pending.request_id`. Allow always carries `updatedInput` (the original input); remember
/// adds `updatedPermissions` with every destination rewritten to `"session"`; deny uses
/// [`DENY_PREFIX`] + message and `interrupt`; `AskUserQuestion` is always denied with
/// [`ASK_USER_QUESTION_DENY`].
pub fn approval_response(pending: &Pending, decision: &ApprovalDecision) -> Value {
    todo!("M2-CLAUDE: approval_response")
}

/// `Tool(ruleContent)` strings added to `attempts.allow_rules` on "Consenti sempre".
pub fn remembered_rules(pending: &Pending) -> Vec<String> {
    todo!("M2-CLAUDE: remembered_rules")
}

/// The single stdin writer: for each frame writes the JSON, `\n`, flushes, and appends the
/// same line to `log` (`stdin.jsonl`) if given. Returns when `rx` is closed (all senders
/// dropped = stdin closed, spec §7.4 step 3) or on a write error.
pub async fn write_frames<W: AsyncWrite + Unpin>(
    stdin: W,
    rx: mpsc::Receiver<Value>,
    log: Option<tokio::fs::File>,
) -> std::io::Result<()> {
    Err(std::io::Error::other("M2-CLAUDE: write_frames"))
}
