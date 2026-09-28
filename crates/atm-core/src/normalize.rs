//! Pure normalizer: CLI stream-json lines → transcript entry operations (spec §7.6).
//! Owner: M2-CLAUDE. Golden-tested with insta; no I/O, no clock (timestamps are passed in).

use std::collections::HashMap;

use atm_types::{
    ApprovalDecision, Entry, EntryBody, Id, Level, LimitKind, Millis, NoticeAction, ToolOutput,
    ToolStatus,
};
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
/// `AwaitingApproval.reason` cap (a one-line rendering of `decision_reason`).
pub const MAX_REASON: usize = 512;
/// Bash summaries keep the first line of the command, at most this many characters.
pub const MAX_BASH_SUMMARY: usize = 200;

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

/// Case-insensitive substring of the CLI's stderr (or failed `result` text) when `--resume`
/// names a session it cannot find (spec §7.9). The [`Normalizer`] itself then adds, once per
/// turn, a warning Notice with [`NoticeAction::NewSession`]: the runner does not add another.
/// M5 checks it against the real CLI.
pub const RESUME_FAILED_PATTERN: &str = "no conversation found";

/// `apiKeySource` values of `system/init` that mean "no API key" (subscription login); any
/// other value warns. [DA VERIFICARE → M5].
pub const NO_API_KEY_SOURCES: &[&str] = &["none"];

/// Notice texts (Italian, shown as is).
pub const COMPACTED_NOTICE: &str = "Contesto compattato";
pub const RESUME_FAILED_NOTICE: &str =
    "La sessione precedente non è stata trovata dal CLI: avvia una nuova sessione.";

const ELLIPSIS: &str = "…";
const OUTPUT_SEPARATOR: &str = "\n…\n";

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

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

// `str::floor_char_boundary` is newer than the workspace MSRV.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `s` cut to at most `max` bytes on a char boundary, ending with `…` when cut.
fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let end = floor_boundary(s, max.saturating_sub(ELLIPSIS.len()));
    format!("{}{ELLIPSIS}", &s[..end])
}

/// Head and tail of `s` within `max` bytes, and how many bytes were dropped in between.
fn head_tail(s: &str, max: usize) -> (String, u64) {
    if s.len() <= max {
        return (s.to_owned(), 0);
    }
    let budget = max - OUTPUT_SEPARATOR.len();
    let head = floor_boundary(s, budget / 2);
    let tail = ceil_boundary(s, s.len() - (budget - budget / 2));
    let text = format!("{}{OUTPUT_SEPARATOR}{}", &s[..head], &s[tail..]);
    (text, (tail - head) as u64)
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// Removes ANSI escape sequences (CSI, OSC and two-byte escapes) and carriage returns.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters up to a final byte in @..~
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST (ESC \)
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// `path` relative to `worktree` (`.` for the worktree itself); unchanged if outside.
fn relative(path: &str, worktree: &str) -> String {
    let root = worktree.trim_end_matches('/');
    match path.strip_prefix(root) {
        Some("") if !root.is_empty() => ".".into(),
        Some(rest) if !root.is_empty() && rest.starts_with('/') => rest[1..].into(),
        _ => path.into(),
    }
}

/// The `RESUME_FAILED_PATTERN` test (case-insensitive).
pub fn is_resume_failure(text: &str) -> bool {
    text.to_lowercase().contains(RESUME_FAILED_PATTERN)
}

/// `session_id` of a `system/init` line: the runner sets `session_started` and compares it
/// with the expected id (spec §7.6).
pub fn init_session_id(line: &Value) -> Option<&str> {
    if line["type"] == "system" && line["subtype"] == "init" {
        str_of(line, "session_id")
    } else {
        None
    }
}

fn classify_limit(subtype: &str, text: Option<&str>) -> Option<LimitKind> {
    let haystack = format!("{subtype}\n{}", text.unwrap_or_default()).to_lowercase();
    LIMIT_PATTERNS
        .iter()
        .find(|(pattern, _)| haystack.contains(pattern))
        .map(|&(_, kind)| kind)
}

/// Parses a `{"type":"result",…}` line, classifying `limit` with [`LIMIT_PATTERNS`] when
/// `is_error`. `None` for any other line. `text` is `result`, else the `errors` joined.
pub fn parse_result(line: &Value) -> Option<TurnResult> {
    if line["type"] != "result" {
        return None;
    }
    let subtype = str_of(line, "subtype").unwrap_or_default().to_owned();
    let is_error = line["is_error"].as_bool().unwrap_or(false);
    let text = match (&line["result"], line["errors"].as_array()) {
        (Value::String(s), _) => Some(s.clone()),
        (_, Some(errors)) if !errors.is_empty() => Some(
            errors
                .iter()
                .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
    .map(|t| cap(&t, MAX_TEXT));
    let limit = if is_error {
        classify_limit(&subtype, text.as_deref())
    } else {
        None
    };
    Some(TurnResult {
        subtype,
        is_error,
        duration_ms: line["duration_ms"].as_u64(),
        num_turns: u32_of(line, "num_turns"),
        cost_usd_estimate: line["total_cost_usd"].as_f64(),
        permission_denials: line["permission_denials"]
            .as_array()
            .map_or(0, |d| u32::try_from(d.len()).unwrap_or(u32::MAX)),
        text,
        limit,
    })
}

/// One-line summary of a tool call (spec §7.6 table); paths relative to `worktree`.
pub fn tool_summary(name: &str, input: &Value, worktree: &str) -> String {
    let s = |key| str_of(input, key).unwrap_or_default();
    let path = |key| relative(s(key), worktree);
    let summary = match name {
        "Bash" => {
            let first = s("command").lines().next().unwrap_or_default();
            let first: String = first.chars().take(MAX_BASH_SUMMARY).collect();
            format!("$ {first}")
        }
        "Read" => format!("Read {}", path("file_path")),
        "Edit" | "MultiEdit" => format!("Edit {}", path("file_path")),
        "NotebookEdit" => format!("Edit {}", path("notebook_path")),
        "Write" => format!("Write {}", path("file_path")),
        "Grep" => {
            let dir = str_of(input, "path").map_or_else(|| ".".into(), |p| relative(p, worktree));
            format!("Grep \"{}\" {dir}", s("pattern"))
        }
        "Glob" => format!("Glob {}", s("pattern")),
        "WebFetch" => s("url").to_owned(),
        "WebSearch" => s("query").to_owned(),
        "Task" | "Agent" => format!("Subagent: {}", s("description")),
        "TodoWrite" => {
            let todos = input["todos"].as_array().map_or(&[][..], Vec::as_slice);
            let done = todos.iter().filter(|t| t["status"] == "completed").count();
            format!("Todos: {done}/{}", todos.len())
        }
        _ => match name
            .strip_prefix("mcp__")
            .and_then(|rest| rest.split_once("__"))
        {
            Some((server, tool)) => format!("MCP {server}/{tool}"),
            None => name.to_owned(),
        },
    };
    one_line(&summary)
}

/// Tool input as JSON text within `max` bytes.
fn render_input(input: &Value, max: usize) -> String {
    cap(&input.to_string(), max)
}

/// `decision_reason` (shape [DA VERIFICARE → M5]): a string as is; an object by its
/// `reason`/`message`/`description`, else compact JSON; one line, at most [`MAX_REASON`].
fn render_reason(reason: &Value) -> Option<String> {
    let text = match reason {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        Value::Object(o) => ["reason", "message", "description"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| reason.to_string()),
        other => other.to_string(),
    };
    let text = one_line(text.trim());
    (!text.is_empty()).then(|| cap(&text, MAX_REASON))
}

/// `tool_result.content`: a string, or blocks joined by newlines (images → `[image]`).
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match str_of(b, "type") {
                Some("text") => str_of(b, "text").map(str::to_owned),
                Some("image") => Some("[image]".into()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A `ToolCall` of this turn, kept to apply later updates.
#[derive(Debug)]
struct Tool {
    idx: u32,
    rev: u32,
    ts: Millis,
    parent: Option<String>,
    name: String,
    summary: String,
    input: String,
    status: ToolStatus,
    output: Option<ToolOutput>,
    /// Asked for approval: `input` keeps up to [`MAX_APPROVAL_INPUT`].
    asked: bool,
}

/// The open `Stderr` entry, extended while lines keep coming.
#[derive(Debug)]
struct OpenStderr {
    idx: u32,
    rev: u32,
    ts: Millis,
    text: String,
    last: Millis,
}

/// State for one turn (one process). Entries get consecutive `idx` from `next_idx`; every
/// update of an existing entry bumps its `rev`.
#[derive(Debug)]
pub struct Normalizer {
    process_id: Id,
    next_idx: u32,
    worktree: String,
    /// By `tool_use_id`.
    tools: HashMap<String, Tool>,
    /// `approval_id` → `tool_use_id` while awaiting the user.
    approvals: HashMap<String, String>,
    api_retry: Option<Entry>,
    stderr: Option<OpenStderr>,
    typing: String,
    typing_at: Option<Millis>,
    resume_failed: bool,
}

impl Normalizer {
    /// `next_idx` = `Db::next_entry_idx`; `worktree` (canonical) makes summaries relative and
    /// is compared with the `cwd` of `system/init`.
    pub fn new(process_id: Id, next_idx: u32, worktree: String) -> Normalizer {
        Normalizer {
            process_id,
            next_idx,
            worktree,
            tools: HashMap::new(),
            approvals: HashMap::new(),
            api_retry: None,
            stderr: None,
            typing: String::new(),
            typing_at: None,
            resume_failed: false,
        }
    }

    fn alloc_idx(&mut self) -> u32 {
        self.next_idx += 1;
        self.next_idx - 1
    }

    fn entry(
        &self,
        idx: u32,
        rev: u32,
        ts: Millis,
        parent: Option<String>,
        body: EntryBody,
    ) -> Entry {
        Entry {
            idx,
            rev,
            process_id: self.process_id.clone(),
            ts,
            parent_tool_use_id: parent,
            body,
        }
    }

    fn create(&mut self, ts: Millis, parent: Option<String>, body: EntryBody) -> Entry {
        let idx = self.alloc_idx();
        self.entry(idx, 0, ts, parent, body)
    }

    fn push(&mut self, ts: Millis, parent: Option<String>, body: EntryBody) -> EntryOp {
        EntryOp::Upsert(self.create(ts, parent, body))
    }

    fn tool_op(&self, tool_use_id: &str) -> EntryOp {
        let t = &self.tools[tool_use_id];
        let body = EntryBody::ToolCall {
            tool_use_id: tool_use_id.to_owned(),
            name: t.name.clone(),
            summary: t.summary.clone(),
            input: t.input.clone(),
            status: t.status.clone(),
            output: t.output.clone(),
        };
        EntryOp::Upsert(self.entry(t.idx, t.rev, t.ts, t.parent.clone(), body))
    }

    fn clear_typing(&mut self) -> EntryOp {
        self.typing.clear();
        self.typing_at = None;
        EntryOp::Typing(None)
    }

    fn resume_failed_notice(&mut self, ts: Millis) -> Vec<EntryOp> {
        if std::mem::replace(&mut self.resume_failed, true) {
            return Vec::new();
        }
        self.on_notice(
            Level::Warn,
            RESUME_FAILED_NOTICE,
            Some(NoticeAction::NewSession),
            ts,
        )
    }

    /// `UserMessage` for the prompt sent on stdin.
    pub fn on_user_message(&mut self, text: &str, ts: Millis) -> Vec<EntryOp> {
        let text = cap(text, MAX_TEXT);
        vec![self.push(ts, None, EntryBody::UserMessage { text })]
    }

    /// A routed stdout line (`Inbound::Message` or `Inbound::StreamEvent`), per the §7.6
    /// table; unknown types yield nothing.
    pub fn on_line(&mut self, line: &Value, ts: Millis) -> Vec<EntryOp> {
        let kind = str_of(line, "type").unwrap_or_default();
        let subtype = str_of(line, "subtype").unwrap_or_default();
        match (kind, subtype) {
            ("stream_event", _) => self.stream_event(&line["event"], ts),
            ("assistant", _) => self.assistant(line, ts),
            ("user", _) => self.tool_results(line),
            ("system", "init") => vec![self.session_init(line, ts)],
            ("system", "api_retry") => vec![self.api_retry(line, ts)],
            ("system", "compact_boundary") => {
                self.on_notice(Level::Info, COMPACTED_NOTICE, None, ts)
            }
            ("result", _) => self.result(line, ts),
            _ => Vec::new(),
        }
    }

    fn stream_event(&mut self, event: &Value, ts: Millis) -> Vec<EntryOp> {
        match str_of(event, "type") {
            Some("content_block_start") => self.typing.clear(),
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let text = match str_of(delta, "type") {
                    Some("text_delta") => str_of(delta, "text"),
                    Some("thinking_delta") => str_of(delta, "thinking"),
                    _ => None,
                };
                let Some(text) = text else {
                    return Vec::new();
                };
                if self.typing.len() + text.len() <= MAX_TEXT {
                    self.typing.push_str(text);
                }
                if self
                    .typing_at
                    .is_none_or(|at| ts - at >= TYPING_INTERVAL_MS)
                {
                    self.typing_at = Some(ts);
                    return vec![EntryOp::Typing(Some(self.typing.clone()))];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn assistant(&mut self, line: &Value, ts: Millis) -> Vec<EntryOp> {
        let parent = str_of(line, "parent_tool_use_id").map(str::to_owned);
        let mut ops = Vec::new();
        let blocks = line["message"]["content"].as_array().into_iter().flatten();
        for block in blocks {
            let text = |key| str_of(block, key).filter(|t| !t.is_empty());
            let op = match str_of(block, "type") {
                Some("text") => text("text").map(|t| {
                    let text = cap(t, MAX_TEXT);
                    self.push(ts, parent.clone(), EntryBody::AssistantText { text })
                }),
                Some("thinking") => text("thinking").map(|t| {
                    let text = cap(t, MAX_TEXT);
                    self.push(ts, parent.clone(), EntryBody::Thinking { text })
                }),
                Some("tool_use") => self.tool_use(block, parent.clone(), ts),
                _ => None,
            };
            ops.extend(op);
        }
        ops.push(self.clear_typing());
        ops
    }

    /// New `ToolCall{Running}`, or the merge into the one created by `can_use_tool`.
    fn tool_use(&mut self, block: &Value, parent: Option<String>, ts: Millis) -> Option<EntryOp> {
        let id = str_of(block, "id")?;
        let name = str_of(block, "name").unwrap_or_default().to_owned();
        let input = &block["input"];
        let summary = tool_summary(&name, input, &self.worktree);
        if let Some(t) = self.tools.get_mut(id) {
            let input = render_input(
                input,
                if t.asked {
                    MAX_APPROVAL_INPUT
                } else {
                    MAX_INPUT
                },
            );
            if (&t.name, &t.summary, &t.input, &t.parent) == (&name, &summary, &input, &parent) {
                return None;
            }
            (t.name, t.summary, t.input, t.parent) = (name, summary, input, parent);
            t.rev += 1;
        } else {
            let tool = Tool {
                idx: self.alloc_idx(),
                rev: 0,
                ts,
                parent,
                name,
                summary,
                input: render_input(input, MAX_INPUT),
                status: ToolStatus::Running,
                output: None,
                asked: false,
            };
            self.tools.insert(id.to_owned(), tool);
        }
        Some(self.tool_op(id))
    }

    /// `user` lines: each `tool_result` completes its `ToolCall`; string content is ignored.
    fn tool_results(&mut self, line: &Value) -> Vec<EntryOp> {
        let blocks = line["message"]["content"].as_array().into_iter().flatten();
        let mut ops = Vec::new();
        for block in blocks.filter(|b| b["type"] == "tool_result") {
            let Some(id) = str_of(block, "tool_use_id") else {
                continue;
            };
            let Some(t) = self.tools.get_mut(id) else {
                continue;
            };
            let is_error = block["is_error"].as_bool().unwrap_or(false);
            let (text, truncated_bytes) =
                head_tail(&tool_result_text(&block["content"]), MAX_OUTPUT);
            // A denied call keeps `Denied`: its error result is the deny message.
            if !matches!(t.status, ToolStatus::Denied { .. }) {
                t.status = if is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Succeeded
                };
            }
            t.output = Some(ToolOutput {
                text,
                truncated_bytes,
                is_error,
            });
            t.rev += 1;
            ops.push(self.tool_op(id));
        }
        ops
    }

    fn session_init(&mut self, line: &Value, ts: Millis) -> EntryOp {
        let api_key_source = str_of(line, "apiKeySource").map(str::to_owned);
        let mut warnings = Vec::new();
        if let Some(source) = &api_key_source
            && !NO_API_KEY_SOURCES
                .iter()
                .any(|none| source.eq_ignore_ascii_case(none))
        {
            warnings.push(format!(
                "Il CLI usa una chiave API ({source}): l'uso viene fatturato via API, non con \
                 l'abbonamento."
            ));
        }
        if let Some(cwd) = str_of(line, "cwd")
            && cwd.trim_end_matches('/') != self.worktree.trim_end_matches('/')
        {
            warnings.push(format!(
                "Il CLI lavora in {cwd} invece che nel worktree {}.",
                self.worktree
            ));
        }
        let body = EntryBody::SessionInit {
            model: str_of(line, "model").map(str::to_owned),
            permission_mode: str_of(line, "permissionMode").map(str::to_owned),
            api_key_source,
            mcp_servers: line["mcp_servers"]
                .as_array()
                .map_or(0, |s| u32::try_from(s.len()).unwrap_or(u32::MAX)),
            warnings,
        };
        self.push(ts, None, body)
    }

    fn api_retry(&mut self, line: &Value, ts: Millis) -> EntryOp {
        let error = match &line["error"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let error = match line["error_status"].as_u64() {
            Some(status) => format!("{status} {error}").trim_end().to_owned(),
            None => error,
        };
        let body = EntryBody::ApiRetry {
            attempt: u32_of(line, "attempt").unwrap_or(0),
            max_retries: u32_of(line, "max_retries").unwrap_or(0),
            delay_ms: line["retry_delay_ms"]
                .as_u64()
                .or_else(|| line["delay_ms"].as_u64())
                .unwrap_or(0),
            error: cap(&error, MAX_TEXT),
        };
        let entry = match self.api_retry.take() {
            Some(prev) => Entry {
                rev: prev.rev + 1,
                body,
                ..prev
            },
            None => self.create(ts, None, body),
        };
        self.api_retry = Some(entry.clone());
        EntryOp::Upsert(entry)
    }

    fn result(&mut self, line: &Value, ts: Millis) -> Vec<EntryOp> {
        let Some(r) = parse_result(line) else {
            return Vec::new();
        };
        let resume_failed = r.is_error && r.text.as_deref().is_some_and(is_resume_failure);
        let body = EntryBody::TurnEnd {
            subtype: r.subtype,
            is_error: r.is_error,
            duration_ms: r.duration_ms,
            num_turns: r.num_turns,
            cost_usd_estimate: r.cost_usd_estimate,
            permission_denials: r.permission_denials,
            text: r.text,
            limit: r.limit,
        };
        let mut ops = vec![self.push(ts, None, body)];
        if resume_failed {
            ops.extend(self.resume_failed_notice(ts));
        }
        ops.push(self.clear_typing());
        ops
    }

    /// One stderr line (ANSI stripped, merged with the previous one if < 2 s apart).
    pub fn on_stderr(&mut self, line: &str, ts: Millis) -> Vec<EntryOp> {
        let line = strip_ansi(line);
        let mut ops = Vec::new();
        let open = self.stderr.as_mut().filter(|s| {
            ts - s.last < STDERR_MERGE_MS && s.text.len() + 1 + line.len() <= MAX_STDERR_ENTRY
        });
        if let Some(s) = open {
            s.text.push('\n');
            s.text.push_str(&line);
            s.rev += 1;
            s.last = ts;
        } else if !line.trim().is_empty() {
            let idx = self.alloc_idx();
            let text = cap(&line, MAX_STDERR_ENTRY);
            self.stderr = Some(OpenStderr {
                idx,
                rev: 0,
                ts,
                text,
                last: ts,
            });
        } else {
            return ops;
        }
        if let Some(s) = &self.stderr {
            let body = EntryBody::Stderr {
                text: s.text.clone(),
            };
            ops.push(EntryOp::Upsert(self.entry(s.idx, s.rev, s.ts, None, body)));
        }
        if is_resume_failure(&line) {
            ops.extend(self.resume_failed_notice(ts));
        }
        ops
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
        let id = str_of(request, "tool_use_id")
            .unwrap_or(approval_id)
            .to_owned();
        let input = &request["input"];
        let status = ToolStatus::AwaitingApproval {
            approval_id: approval_id.to_owned(),
            can_remember,
            reason: render_reason(&request["decision_reason"]),
        };
        let rendered = render_input(input, MAX_APPROVAL_INPUT);
        if let Some(t) = self.tools.get_mut(&id) {
            (t.status, t.input, t.asked) = (status, rendered, true);
            t.rev += 1;
        } else {
            let name = str_of(request, "tool_name").unwrap_or_default().to_owned();
            let tool = Tool {
                idx: self.alloc_idx(),
                rev: 0,
                ts,
                parent: None,
                summary: tool_summary(&name, input, &self.worktree),
                name,
                input: rendered,
                status,
                output: None,
                asked: true,
            };
            self.tools.insert(id.clone(), tool);
        }
        self.approvals.insert(approval_id.to_owned(), id.clone());
        vec![self.tool_op(&id)]
    }

    /// Moves the call still awaiting `approval_id` to `status`.
    fn settle_approval(&mut self, approval_id: &str, status: ToolStatus) -> Vec<EntryOp> {
        let Some(id) = self.approvals.remove(approval_id) else {
            return Vec::new();
        };
        let Some(t) = self.tools.get_mut(&id) else {
            return Vec::new();
        };
        if !matches!(&t.status, ToolStatus::AwaitingApproval { approval_id: a, .. } if a == approval_id)
        {
            return Vec::new();
        }
        t.status = status;
        t.rev += 1;
        vec![self.tool_op(&id)]
    }

    /// Allow → `Running`; deny → `Denied{message}`.
    pub fn on_approval_resolved(
        &mut self,
        approval_id: &str,
        decision: &ApprovalDecision,
    ) -> Vec<EntryOp> {
        let status = match decision {
            ApprovalDecision::Allow { .. } => ToolStatus::Running,
            ApprovalDecision::Deny { message, .. } => ToolStatus::Denied {
                message: cap(message, MAX_TEXT),
            },
        };
        self.settle_approval(approval_id, status)
    }

    /// `control_cancel_request`: the `ToolCall` leaves `AwaitingApproval` (→ `Cancelled`).
    pub fn on_approval_cancelled(&mut self, approval_id: &str) -> Vec<EntryOp> {
        self.settle_approval(approval_id, ToolStatus::Cancelled)
    }

    pub fn on_notice(
        &mut self,
        level: Level,
        text: &str,
        action: Option<NoticeAction>,
        ts: Millis,
    ) -> Vec<EntryOp> {
        let text = cap(text, MAX_TEXT);
        vec![self.push(
            ts,
            None,
            EntryBody::Notice {
                level,
                text,
                action,
            },
        )]
    }

    /// End of the process: every `ToolCall` still `Running`/`AwaitingApproval` → `Cancelled`,
    /// plus `Typing(None)`. Updated entries keep their original `ts`.
    pub fn finish(&mut self, _ts: Millis) -> Vec<EntryOp> {
        let mut open: Vec<(u32, String)> = self
            .tools
            .iter()
            .filter(|(_, t)| {
                matches!(
                    t.status,
                    ToolStatus::Running | ToolStatus::AwaitingApproval { .. }
                )
            })
            .map(|(id, t)| (t.idx, id.clone()))
            .collect();
        open.sort_unstable();
        let mut ops = Vec::with_capacity(open.len() + 1);
        for (_, id) in open {
            if let Some(t) = self.tools.get_mut(&id) {
                t.status = ToolStatus::Cancelled;
                t.rev += 1;
            }
            ops.push(self.tool_op(&id));
        }
        self.approvals.clear();
        ops.push(self.clear_typing());
        ops
    }
}
