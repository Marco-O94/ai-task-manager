//! NDJSON wire protocol with `claude -p --input-format stream-json` (spec §7.4, §7.5, §7.8):
//! capped line reader, inbound classification, outbound frames, approval responses.
//! Owner: M2-CLAUDE. Everything except the reader and the writer is pure.

use atm_types::{ApprovalDecision, Id};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
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
    buf.clear();
    let mut len = 0usize;
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(match (too_long, len) {
                (true, _) => Line::TooLong(len),
                (false, 0) => Line::Eof,
                (false, _) => Line::Complete,
            });
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let chunk = &available[..newline.unwrap_or(available.len())];
        len += chunk.len();
        if !too_long && buf.len() + chunk.len() > max {
            too_long = true;
            buf.clear();
        }
        if !too_long {
            buf.extend_from_slice(chunk);
        }
        let used = newline.map_or(chunk.len(), |i| i + 1);
        reader.consume(used);
        if newline.is_some() {
            return Ok(if too_long {
                Line::TooLong(len)
            } else {
                Line::Complete
            });
        }
    }
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

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Classifies one stdout line. A control frame without a `request_id` cannot be answered
/// and falls back to `Message` (the normalizer ignores it).
pub fn parse(line: &[u8]) -> Inbound {
    if line.trim_ascii_start().first() != Some(&b'{') {
        return Inbound::NotJson;
    }
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return Inbound::NotJson;
    };
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "control_response" => {
            let response = &v["response"];
            let request_id =
                str_field(response, "request_id").or_else(|| str_field(&v, "request_id"));
            let Some(request_id) = request_id else {
                return Inbound::Message(v);
            };
            let result = if response["subtype"] == "error" {
                Err(match &response["error"] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            } else {
                Ok(response.get("response").cloned().unwrap_or(Value::Null))
            };
            Inbound::ControlResponse { request_id, result }
        }
        "control_request" => {
            let Some(request_id) = str_field(&v, "request_id") else {
                return Inbound::Message(v);
            };
            let request = &v["request"];
            let subtype = str_field(request, "subtype").unwrap_or_default();
            if subtype == "can_use_tool"
                && let (Some(tool_name), Some(input), Some(tool_use_id)) = (
                    str_field(request, "tool_name"),
                    request.get("input").filter(|i| !i.is_null()),
                    str_field(request, "tool_use_id"),
                )
            {
                return Inbound::CanUseTool(CanUseTool {
                    request_id,
                    tool_name,
                    input: input.clone(),
                    tool_use_id,
                    permission_suggestions: request
                        .get("permission_suggestions")
                        .cloned()
                        .unwrap_or(Value::Null),
                    request: request.clone(),
                });
            }
            Inbound::ControlRequest {
                request_id,
                subtype,
            }
        }
        "control_cancel_request" => match str_field(&v, "request_id") {
            Some(request_id) => Inbound::ControlCancel { request_id },
            None => Inbound::Message(v),
        },
        "keep_alive" => Inbound::KeepAlive,
        "stream_event" => Inbound::StreamEvent(v),
        _ => Inbound::Message(v),
    }
}

/// `atm_<n>_<8 hex>`: id of a request sent by the host.
pub fn request_id(n: u64) -> String {
    format!("atm_{n}_{:08x}", uuid::Uuid::new_v4().as_u128() as u32)
}

/// `{"type":"control_request","request_id":…,"request":{"subtype":"initialize","hooks":null}}`.
pub fn initialize_request(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "initialize", "hooks": null},
    })
}

/// `{"type":"control_request","request_id":…,"request":{"subtype":"interrupt"}}`.
pub fn interrupt_request(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"},
    })
}

/// `{"type":"user","message":{"role":"user","content":<prompt>},"parent_tool_use_id":null}`
/// (no `session_id`).
pub fn user_message(prompt: &str) -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": prompt},
        "parent_tool_use_id": null,
    })
}

/// `control_response` with subtype `success` echoing `request_id`.
pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
}

/// `control_response` with `{"subtype":"error","request_id":…,"error":<error>}`, e.g.
/// `"Unsupported control request subtype: X"`.
pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": error},
    })
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

impl Pending {
    /// Registers a `can_use_tool` under a new app-side `approval_id`.
    pub fn new(approval_id: Id, req: &CanUseTool) -> Pending {
        Pending {
            approval_id,
            request_id: req.request_id.clone(),
            tool_use_id: req.tool_use_id.clone(),
            tool_name: req.tool_name.clone(),
            input: req.input.clone(),
            suggestions: req.permission_suggestions.clone(),
        }
    }
}

/// True only if every suggestion is `addRules` with `behavior:"allow"` and every rule has
/// a non-empty `ruleContent` (whole-tool rules are never rememberable).
pub fn can_remember(suggestions: &Value) -> bool {
    let rememberable = |s: &Value| {
        s["type"] == "addRules"
            && s["behavior"] == "allow"
            && s["rules"].as_array().is_some_and(|rules| {
                !rules.is_empty()
                    && rules
                        .iter()
                        .all(|r| r["ruleContent"].as_str().is_some_and(|c| !c.is_empty()))
            })
    };
    suggestions
        .as_array()
        .is_some_and(|all| !all.is_empty() && all.iter().all(rememberable))
}

/// Complete `control_response` frame for a decision (spec §7.8 table), echoing
/// `pending.request_id`. Allow always carries `updatedInput` (the original input); remember
/// adds `updatedPermissions` with every destination rewritten to `"session"`; deny uses
/// [`DENY_PREFIX`] + message and `interrupt`; `AskUserQuestion` is always denied with
/// [`ASK_USER_QUESTION_DENY`]. `remember` is ignored unless [`can_remember`] holds, so a
/// whole-tool rule is never sent.
pub fn approval_response(pending: &Pending, decision: &ApprovalDecision) -> Value {
    let response = if pending.tool_name == "AskUserQuestion" {
        json!({"behavior": "deny", "message": ASK_USER_QUESTION_DENY, "interrupt": false})
    } else {
        match decision {
            ApprovalDecision::Allow { remember } => {
                let mut allow = json!({"behavior": "allow", "updatedInput": pending.input});
                if *remember && can_remember(&pending.suggestions) {
                    let mut suggestions = pending.suggestions.clone();
                    for s in suggestions.as_array_mut().into_iter().flatten() {
                        s["destination"] = "session".into();
                    }
                    allow["updatedPermissions"] = suggestions;
                }
                allow
            }
            ApprovalDecision::Deny { message, interrupt } => json!({
                "behavior": "deny",
                "message": format!("{DENY_PREFIX}{message}"),
                "interrupt": interrupt,
            }),
        }
    };
    control_success(&pending.request_id, response)
}

/// `Tool(ruleContent)` strings added to `attempts.allow_rules` on "Consenti sempre"; empty
/// unless [`can_remember`] holds.
pub fn remembered_rules(pending: &Pending) -> Vec<String> {
    if !can_remember(&pending.suggestions) {
        return Vec::new();
    }
    let rules = pending.suggestions.as_array().into_iter().flatten();
    rules
        .flat_map(|s| s["rules"].as_array().into_iter().flatten())
        .map(|r| {
            let tool = r["toolName"].as_str().unwrap_or(&pending.tool_name);
            format!("{tool}({})", r["ruleContent"].as_str().unwrap_or_default())
        })
        .collect()
}

/// The single stdin writer: for each frame writes the JSON, `\n`, flushes, and appends the
/// same line to `log` (`stdin.jsonl`) if given. Returns when `rx` is closed (all senders
/// dropped = stdin closed, spec §7.4 step 3) or on a write error.
pub async fn write_frames<W: AsyncWrite + Unpin>(
    mut stdin: W,
    mut rx: mpsc::Receiver<Value>,
    mut log: Option<tokio::fs::File>,
) -> std::io::Result<()> {
    while let Some(frame) = rx.recv().await {
        let mut line = serde_json::to_vec(&frame)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        // A failing diagnostic log must not stop the turn: stop logging instead. tokio's
        // File buffers internally, hence the flush.
        if let Some(file) = &mut log
            && (file.write_all(&line).await.is_err() || file.flush().await.is_err())
        {
            log = None;
        }
    }
    stdin.shutdown().await
}
