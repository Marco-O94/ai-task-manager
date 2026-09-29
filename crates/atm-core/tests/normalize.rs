//! M2-CLAUDE: golden tests of the normalizer, one per row of the spec §7.6 tables, plus
//! whole fixture turns routed as the runner does (`tests/fixtures/stream/*.jsonl`).

use atm_core::normalize::{self, EntryOp, Normalizer};
use atm_core::wire::{self, Inbound};
use atm_types::{ApprovalDecision, Entry, EntryBody, Level, LimitKind, NoticeAction, ToolStatus};
use serde::Serialize;
use serde_json::{Value, json};

const WT: &str = "/Users/me/.ai-task-manager/worktrees/demo/atm-1-hello";

/// `EntryOp` as a snapshot-friendly value.
#[derive(Serialize)]
enum Op {
    Upsert(Box<Entry>),
    Typing(Option<String>),
}

fn view(ops: Vec<EntryOp>) -> Vec<Op> {
    ops.into_iter()
        .map(|op| match op {
            EntryOp::Upsert(e) => Op::Upsert(Box::new(e)),
            EntryOp::Typing(t) => Op::Typing(t),
        })
        .collect()
}

fn normalizer() -> Normalizer {
    Normalizer::new("proc-1".into(), 10, WT.into())
}

fn upserts(ops: &[EntryOp]) -> Vec<&Entry> {
    ops.iter()
        .filter_map(|op| match op {
            EntryOp::Upsert(e) => Some(e),
            EntryOp::Typing(_) => None,
        })
        .collect()
}

fn tool_status(ops: &[EntryOp]) -> Vec<(u32, u32, ToolStatus)> {
    upserts(ops)
        .into_iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolCall { status, .. } => Some((e.idx, e.rev, status.clone())),
            _ => None,
        })
        .collect()
}

fn assistant(content: Value, parent: Option<&str>) -> Value {
    json!({"type":"assistant","message":{"id":"msg_1","role":"assistant","content":content},
           "parent_tool_use_id":parent,"session_id":"s"})
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    assistant(
        json!([{"type":"tool_use","id":id,"name":name,"input":input}]),
        None,
    )
}

fn tool_result(id: &str, content: Value, is_error: bool) -> Value {
    json!({"type":"user","message":{"role":"user","content":[
        {"type":"tool_result","tool_use_id":id,"content":content,"is_error":is_error}]},
        "parent_tool_use_id":null})
}

fn delta(kind: &str, text: &str) -> Value {
    let field = if kind == "thinking_delta" {
        "thinking"
    } else {
        "text"
    };
    json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,
           "delta":{"type":kind, field:text}}})
}

fn block_start() -> Value {
    json!({"type":"stream_event","event":{"type":"content_block_start","index":0,
           "content_block":{"type":"text","text":""}}})
}

fn can_use_tool(tool_use_id: &str, name: &str, input: Value, reason: Value) -> Value {
    json!({"subtype":"can_use_tool","tool_name":name,"input":input,"tool_use_id":tool_use_id,
           "decision_reason":reason})
}

// ---- rows of the §7.6 table -----------------------------------------------------------------

#[test]
fn user_message() {
    let mut n = normalizer();
    insta::assert_json_snapshot!(view(
        n.on_user_message("# Crea hello\n\nScrivi hello.txt", 1000)
    ));
}

#[test]
fn session_init() {
    let mut n = normalizer();
    let clean = json!({"type":"system","subtype":"init","cwd":WT,"session_id":"s1",
        "model":"claude-opus-5-5","permissionMode":"acceptEdits","apiKeySource":"none",
        "mcp_servers":[],"tools":["Bash"]});
    let warned = json!({"type":"system","subtype":"init","cwd":"/private/tmp/elsewhere",
        "session_id":"s2","model":"claude-opus-5-5","permissionMode":"default",
        "apiKeySource":"ANTHROPIC_API_KEY",
        "mcp_servers":[{"name":"github","status":"connected"},{"name":"db","status":"failed"}]});
    let mut ops = n.on_line(&clean, 1000);
    ops.extend(n.on_line(&warned, 2000));
    insta::assert_json_snapshot!(view(ops));
    assert_eq!(normalize::init_session_id(&clean), Some("s1"));
    assert_eq!(
        normalize::init_session_id(&json!({"type":"system","subtype":"status"})),
        None
    );
}

/// Without `apiKeySource` the billing source is unknown: a warning, as for an API key; `none`
/// (M5, spec §13.4) is the only silent value, in any case.
#[test]
fn session_init_warns_without_an_api_key_source() {
    let init = |source: Option<&str>| {
        let mut line = json!({"type":"system","subtype":"init","cwd":WT,"session_id":"s1",
            "model":"sonnet","permissionMode":"default","mcp_servers":[]});
        if let Some(source) = source {
            line["apiKeySource"] = source.into();
        }
        let ops = normalizer().on_line(&line, 1000);
        match &upserts(&ops)[0].body {
            EntryBody::SessionInit { warnings, .. } => warnings.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(init(Some("none")), Vec::<String>::new());
    assert_eq!(init(Some("NONE")), Vec::<String>::new());
    let missing = init(None);
    assert_eq!(missing.len(), 1);
    assert!(missing[0].contains("apiKeySource"), "{missing:?}");
    let key = init(Some("ANTHROPIC_API_KEY"));
    assert_eq!(key.len(), 1);
    assert!(key[0].contains("chiave API (ANTHROPIC_API_KEY)"), "{key:?}");
}

/// What makes the runner stop a turn (spec §7.6): a `system/init` whose `apiKeySource` is
/// present and not in `NO_API_KEY_SOURCES` (any case). A missing value only warns (above);
/// other lines never count.
#[test]
fn api_key_billing_of_system_init() {
    let init = |source: Value| json!({"type":"system","subtype":"init","session_id":"s1","apiKeySource":source});
    for source in [
        "ANTHROPIC_API_KEY",
        "apiKeyHelper",
        "/login managed key",
        "",
    ] {
        assert_eq!(
            normalize::api_key_billing(&init(source.into())),
            Some(source),
            "{source}"
        );
    }
    for none in normalize::NO_API_KEY_SOURCES {
        assert_eq!(normalize::api_key_billing(&init((*none).into())), None);
        let upper = none.to_uppercase();
        assert_eq!(normalize::api_key_billing(&init(upper.into())), None);
    }
    assert_eq!(normalize::api_key_billing(&init(Value::Null)), None);
    let status = json!({"type":"system","subtype":"status","apiKeySource":"ANTHROPIC_API_KEY"});
    assert_eq!(normalize::api_key_billing(&status), None);
}

#[test]
fn stream_event_typing_preview() {
    let mut n = normalizer();
    let mut ops = Vec::new();
    for (line, ts) in [
        (block_start(), 1000),
        (delta("text_delta", "Hel"), 1000),
        (delta("text_delta", "lo"), 1050), // < 100 ms: buffered only
        (delta("text_delta", " world"), 1120), // ≥ 100 ms: whole buffer
        (
            json!({"type":"stream_event","event":{"type":"message_delta"}}),
            1300,
        ), // ignored
        (block_start(), 1400),             // resets the buffer
        (delta("thinking_delta", "Pensando"), 1450),
        (delta("input_json_delta", "{\"a\""), 1600), // not text: ignored
        (
            assistant(json!([{"type":"text","text":"Hello world"}]), None),
            1700,
        ),
    ] {
        ops.extend(n.on_line(&line, ts));
    }
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn assistant_blocks_and_subagent_parent() {
    let mut n = normalizer();
    let line = assistant(
        json!([
            {"type":"thinking","thinking":"Plan: write the file.","signature":"c2ln"},
            {"type":"redacted_thinking","data":"xyz"},
            {"type":"text","text":"Creating it now."},
            {"type":"text","text":""},
            {"type":"tool_use","id":"toolu_1","name":"Write",
             "input":{"file_path":format!("{WT}/src/hello.txt"),"content":"hello\n"}},
        ]),
        Some("toolu_parent"),
    );
    insta::assert_json_snapshot!(view(n.on_line(&line, 1000)));
}

#[test]
fn tool_results() {
    let mut n = normalizer();
    let mut ops = Vec::new();
    for (id, name, input) in [
        ("toolu_ok", "Bash", json!({"command":"ls"})),
        ("toolu_err", "Bash", json!({"command":"false"})),
        (
            "toolu_blocks",
            "Read",
            json!({"file_path":format!("{WT}/logo.png")}),
        ),
    ] {
        ops.extend(n.on_line(&tool_use(id, name, input), 1000));
    }
    ops.extend(n.on_line(&tool_result("toolu_ok", json!("a.txt\nb.txt"), false), 1100));
    ops.extend(n.on_line(&tool_result("toolu_err", json!("exit code 1"), true), 1200));
    ops.extend(n.on_line(
        &tool_result(
            "toolu_blocks",
            json!([{"type":"text","text":"Image read"},
                   {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBO"}},
                   {"type":"text","text":"done"}]),
            false,
        ),
        1300,
    ));
    // Unknown tool_use_id: nothing to update.
    ops.extend(n.on_line(&tool_result("toolu_unknown", json!("?"), false), 1400));
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn user_string_content_is_ignored() {
    let mut n = normalizer();
    let line = json!({"type":"user","message":{"role":"user","content":"a replayed prompt"}});
    assert!(n.on_line(&line, 1000).is_empty());
    let text_blocks = json!({"type":"user","message":{"role":"user",
        "content":[{"type":"text","text":"not a tool result"}]}});
    assert!(n.on_line(&text_blocks, 1000).is_empty());
}

#[test]
fn tool_output_keeps_head_and_tail_within_8_kib() {
    let mut n = normalizer();
    n.on_line(
        &tool_use("toolu_big", "Bash", json!({"command":"cat big"})),
        1000,
    );
    let big: String = (0..5000).map(|i| format!("riga {i:05}\n")).collect(); // 55 000 bytes
    let ops = n.on_line(&tool_result("toolu_big", json!(big), false), 1100);
    let [EntryOp::Upsert(entry)] = &ops[..] else {
        panic!("{ops:?}")
    };
    let EntryBody::ToolCall {
        output: Some(out),
        status,
        ..
    } = &entry.body
    else {
        panic!("{entry:?}")
    };
    assert_eq!(*status, ToolStatus::Succeeded);
    assert!(out.text.len() <= normalize::MAX_OUTPUT);
    assert!(out.text.starts_with("riga 00000\n"));
    assert!(out.text.ends_with("riga 04999\n"));
    assert!(out.text.contains("\n…\n"));
    assert_eq!(
        out.truncated_bytes as usize,
        big.len() - (out.text.len() - "\n…\n".len())
    );
    // Multi-byte text is cut on char boundaries.
    n.on_line(
        &tool_use("toolu_utf8", "Bash", json!({"command":"cat"})),
        1000,
    );
    let ops = n.on_line(
        &tool_result("toolu_utf8", json!("è".repeat(9000)), false),
        1100,
    );
    let EntryBody::ToolCall {
        output: Some(out), ..
    } = &upserts(&ops)[0].body
    else {
        panic!()
    };
    assert!(out.text.len() <= normalize::MAX_OUTPUT);
}

#[test]
fn api_retry_is_one_entry_updated_in_place() {
    let mut n = normalizer();
    let retry = |attempt| {
        json!({"type":"system","subtype":"api_retry","attempt":attempt,"max_retries":10,
               "retry_delay_ms":500 * attempt,"error_status":529,"error":"overloaded_error"})
    };
    let mut ops = n.on_line(&retry(1), 1000);
    ops.extend(n.on_line(&retry(2), 1600));
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn compact_boundary_is_an_info_notice() {
    let mut n = normalizer();
    let line = json!({"type":"system","subtype":"compact_boundary",
                      "compact_metadata":{"trigger":"auto","pre_tokens":150000}});
    insta::assert_json_snapshot!(view(n.on_line(&line, 1000)));
}

#[test]
fn result_success_and_error() {
    let mut n = normalizer();
    let ok = json!({"type":"result","subtype":"success","is_error":false,"duration_ms":5123,
        "num_turns":3,"result":"Done.","total_cost_usd":0.0421,"permission_denials":[]});
    let failed = json!({"type":"result","subtype":"success","is_error":true,"duration_ms":10,
        "num_turns":1,"result":"Claude AI usage limit reached|1760000000",
        "permission_denials":[{"tool_name":"Bash"},{"tool_name":"Edit"}]});
    let errors = json!({"type":"result","subtype":"error_during_execution","is_error":true,
        "errors":["first problem","second problem"]});
    let mut ops = n.on_line(&ok, 1000);
    ops.extend(n.on_line(&failed, 2000));
    ops.extend(n.on_line(&errors, 3000));
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn result_limit_classification() {
    let result = |subtype: &str, is_error: bool, text: &str| {
        normalize::parse_result(
            &json!({"type":"result","subtype":subtype,"is_error":is_error,
                                         "result":text}),
        )
        .unwrap()
        .limit
    };
    assert_eq!(
        result("success", true, "Not logged in · Please run /login"),
        Some(LimitKind::AuthFailure)
    );
    assert_eq!(
        result("success", true, "Invalid API key · Please run /login"),
        Some(LimitKind::AuthFailure)
    );
    assert_eq!(
        result("success", true, "OAuth token: Login expired"),
        Some(LimitKind::AuthFailure)
    );
    assert_eq!(
        result(
            "error_billing",
            true,
            "billing_error: credit balance too low"
        ),
        Some(LimitKind::Billing)
    );
    assert_eq!(
        result("success", true, "API Error: Rate limit exceeded"),
        Some(LimitKind::RateLimit)
    );
    assert_eq!(
        result("success", true, "Claude AI usage limit reached|1760000000"),
        Some(LimitKind::UsageLimit)
    );
    assert_eq!(
        result("success", true, "5-hour limit reached ∙ resets 5pm"),
        Some(LimitKind::UsageLimit)
    );
    assert_eq!(result("error_max_turns", true, "Reached max turns"), None);
    // Only a failed result is classified.
    assert_eq!(
        result("success", false, "I raised the rate limit in config.rs"),
        None
    );
    assert_eq!(normalize::parse_result(&json!({"type":"assistant"})), None);
    let r = normalize::parse_result(
        &json!({"type":"result","subtype":"success","is_error":false,
        "duration_ms":7,"num_turns":2,"total_cost_usd":0.5,"permission_denials":[{}]}),
    )
    .unwrap();
    assert_eq!(
        (
            r.duration_ms,
            r.num_turns,
            r.cost_usd_estimate,
            r.permission_denials,
            r.text
        ),
        (Some(7), Some(2), Some(0.5), 1, None)
    );
}

#[test]
fn approval_after_tool_use_then_allowed() {
    let mut n = normalizer();
    let input = json!({"command":"npm test","description":"Run tests"});
    let mut ops = n.on_line(&tool_use("toolu_1", "Bash", input.clone()), 1000);
    let request = can_use_tool(
        "toolu_1",
        "Bash",
        input,
        json!("Bash needs approval in this mode"),
    );
    ops.extend(n.on_approval_requested("approval-1", &request, true, 1100));
    ops.extend(n.on_approval_resolved("approval-1", &ApprovalDecision::Allow { remember: true }));
    // A second resolution of the same approval is a no-op.
    assert!(
        n.on_approval_resolved("approval-1", &ApprovalDecision::Allow { remember: false })
            .is_empty()
    );
    ops.extend(n.on_line(&tool_result("toolu_1", json!("3 passing"), false), 1200));
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn approval_before_tool_use_creates_then_merges() {
    let mut n = normalizer();
    let input = json!({"file_path":format!("{WT}/src/lib.rs"),"old_string":"a","new_string":"b"});
    let reason = json!({"type":"rule","rule":{"toolName":"Edit"},"reason":"matches an ask rule"});
    let request = can_use_tool("toolu_2", "Edit", input.clone(), reason);
    let mut ops = n.on_approval_requested("approval-2", &request, false, 1000);
    ops.extend(n.on_line(&tool_use("toolu_2", "Edit", input), 1100));
    ops.extend(n.on_approval_resolved(
        "approval-2",
        &ApprovalDecision::Deny {
            message: "non toccare lib.rs".into(),
            interrupt: false,
        },
    ));
    // The CLI then reports the rejection as an error result: the call stays Denied.
    ops.extend(n.on_line(
        &tool_result("toolu_2", json!(wire::DENY_PREFIX), true),
        1200,
    ));
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn approval_cancelled() {
    let mut n = normalizer();
    let request = can_use_tool(
        "toolu_3",
        "WebFetch",
        json!({"url":"https://example.com"}),
        json!({"type":"mode"}),
    );
    let mut ops = n.on_approval_requested("approval-3", &request, false, 1000);
    ops.extend(n.on_approval_cancelled("approval-3"));
    assert!(n.on_approval_cancelled("approval-3").is_empty());
    assert!(n.on_approval_cancelled("unknown").is_empty());
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn approval_keeps_input_up_to_256_kib() {
    let big = "x".repeat(100 << 10);
    let input = json!({"file_path":format!("{WT}/big.txt"),"content":big});
    let input_len = |ops: &[EntryOp]| match &upserts(ops)[0].body {
        EntryBody::ToolCall { input, .. } => input.len(),
        other => panic!("{other:?}"),
    };

    // Without approval: 4 KiB.
    let mut n = normalizer();
    let ops = n.on_line(&tool_use("toolu_a", "Write", input.clone()), 1000);
    assert!(input_len(&ops) <= normalize::MAX_INPUT);

    // Asked for approval: the whole 100 KiB input, also after the tool_use merge.
    let request = can_use_tool("toolu_a", "Write", input.clone(), Value::Null);
    let ops = n.on_approval_requested("approval-a", &request, false, 1100);
    assert_eq!(input_len(&ops), input.to_string().len());
    let ops = n.on_line(&tool_use("toolu_a", "Write", input.clone()), 1200);
    assert!(
        ops.iter().all(|op| !matches!(op, EntryOp::Upsert(_))),
        "unchanged: no upsert"
    );

    let mut n = normalizer();
    let ops = n.on_approval_requested(
        "approval-b",
        &can_use_tool("toolu_b", "Write", input.clone(), Value::Null),
        false,
        1000,
    );
    assert_eq!(input_len(&ops), input.to_string().len());
    let ops = n.on_line(&tool_use("toolu_b", "Write", input), 1100);
    assert!(upserts(&ops).is_empty());

    // Beyond 256 KiB the approval input is cut too.
    let huge = json!({"content":"y".repeat(300 << 10)});
    let ops = n.on_approval_requested(
        "approval-c",
        &can_use_tool("toolu_c", "Write", huge, Value::Null),
        false,
        1000,
    );
    assert!(input_len(&ops) <= normalize::MAX_APPROVAL_INPUT);
}

#[test]
fn stderr_merges_lines_closer_than_2_s() {
    let mut n = normalizer();
    let mut ops = Vec::new();
    for (line, ts) in [
        ("\x1b[31merror:\x1b[0m something failed\r", 1000),
        ("    at step one", 1500),
        ("", 3000), // merged (1.5 s after the previous line)
        ("\x1b]0;title\x07    at step two", 3100),
        ("later, unrelated", 6000), // ≥ 2 s: new entry
        ("   ", 20_000),            // blank line alone: no entry
    ] {
        ops.extend(n.on_stderr(line, ts));
    }
    insta::assert_json_snapshot!(view(ops));
}

#[test]
fn stderr_entry_is_capped_at_64_kib() {
    let mut n = normalizer();
    let line = "e".repeat(40 << 10);
    let first = n.on_stderr(&line, 1000);
    let second = n.on_stderr(&line, 1100); // would exceed 64 KiB: new entry
    let idx = |ops: &[EntryOp]| upserts(ops)[0].idx;
    assert_ne!(idx(&first), idx(&second));
    let long = n.on_stderr(&"z".repeat(100 << 10), 10_000);
    let EntryBody::Stderr { text } = &upserts(&long)[0].body else {
        panic!()
    };
    assert!(text.len() <= normalize::MAX_STDERR_ENTRY);
}

#[test]
fn finish_cancels_open_tool_calls() {
    let mut n = normalizer();
    n.on_line(
        &tool_use("toolu_done", "Bash", json!({"command":"ls"})),
        1000,
    );
    n.on_line(&tool_result("toolu_done", json!("ok"), false), 1050);
    n.on_line(
        &tool_use("toolu_running", "Bash", json!({"command":"sleep 100"})),
        1100,
    );
    let request = can_use_tool(
        "toolu_waiting",
        "Bash",
        json!({"command":"rm x"}),
        Value::Null,
    );
    n.on_approval_requested("approval-w", &request, false, 1200);
    n.on_line(&delta("text_delta", "typing…"), 1300);
    let ops = n.finish(2000);
    insta::assert_json_snapshot!(view(ops));
    // Resolving after the end changes nothing.
    assert!(
        n.on_approval_resolved("approval-w", &ApprovalDecision::Allow { remember: false })
            .is_empty()
    );
}

#[test]
fn notice_entry() {
    let mut n = normalizer();
    insta::assert_json_snapshot!(view(n.on_notice(
        Level::Error,
        "Esecuzione interrotta dal riavvio dell'app",
        None,
        1000
    )));
}

#[test]
fn other_system_and_unknown_lines_yield_nothing() {
    let mut n = normalizer();
    for line in [
        json!({"type":"system","subtype":"status","status":"compacting"}),
        json!({"type":"system","subtype":"hook_response","hook_name":"Stop"}),
        json!({"type":"system","subtype":"hook_started"}),
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}),
        json!({"type":"tool_progress","tool_use_id":"x"}),
        json!({"type":"something_new","payload":[1,2,3]}),
        json!({"type":"stream_event","event":{"type":"message_start"}}),
        json!({"no_type":true}),
        json!([1, 2]),
        Value::Null,
    ] {
        assert!(n.on_line(&line, 1000).is_empty(), "{line}");
    }
}

/// The runner, not the normalizer, adds the `NewSession` Notice (M1 contract of
/// `RESUME_FAILED_PATTERN`): the normalizer only records stderr and the `TurnEnd`.
#[test]
fn resume_failure_is_detected_and_left_to_the_runner() {
    let stderr = "No conversation found with session ID: 3f1c2b9e";
    assert!(normalize::is_resume_failure(stderr));
    assert!(!normalize::is_resume_failure("Error: rate limited"));

    let mut n = normalizer();
    let mut ops = n.on_stderr(stderr, 1000);
    ops.extend(n.on_line(
        &json!({"type":"result","subtype":"error_during_execution","is_error":true,
                "result":stderr}),
        1100,
    ));
    let kinds: Vec<_> = upserts(&ops)
        .iter()
        .map(|e| match e.body {
            EntryBody::Stderr { .. } => "Stderr",
            EntryBody::TurnEnd { .. } => "TurnEnd",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["Stderr", "TurnEnd"]);

    let ops = n.on_notice(
        Level::Warn,
        "Sessione non trovata",
        Some(NoticeAction::NewSession),
        1200,
    );
    assert!(matches!(
        &upserts(&ops)[0].body,
        EntryBody::Notice {
            action: Some(NoticeAction::NewSession),
            ..
        }
    ));
}

#[test]
fn texts_are_capped_at_256_kib() {
    let mut n = normalizer();
    let long = "é".repeat(200 << 10); // 400 KiB
    let ops = n.on_user_message(&long, 1000);
    let EntryBody::UserMessage { text } = &upserts(&ops)[0].body else {
        panic!()
    };
    assert!(text.len() <= normalize::MAX_TEXT && text.ends_with('…'));
    let ops = n.on_line(&assistant(json!([{"type":"text","text":long}]), None), 1000);
    let EntryBody::AssistantText { text } = &upserts(&ops)[0].body else {
        panic!()
    };
    assert!(text.len() <= normalize::MAX_TEXT);
}

#[test]
fn indexes_are_consecutive_and_revisions_grow() {
    let mut n = Normalizer::new("proc-9".into(), 42, WT.into());
    let mut ops = n.on_user_message("ciao", 1);
    ops.extend(n.on_line(&tool_use("t1", "Bash", json!({"command":"ls"})), 2));
    ops.extend(n.on_line(&tool_result("t1", json!("ok"), false), 3));
    ops.extend(n.on_notice(Level::Info, "x", None, 4));
    let keys: Vec<_> = upserts(&ops).iter().map(|e| (e.idx, e.rev)).collect();
    assert_eq!(keys, [(42, 0), (43, 0), (43, 1), (44, 0)]);
    assert!(upserts(&ops).iter().all(|e| e.process_id == "proc-9"));
    assert_eq!(tool_status(&ops)[1].2, ToolStatus::Succeeded);
}

// ---- ToolCall.summary table ---------------------------------------------------------------

#[test]
fn tool_summaries() {
    let long_cmd = format!("echo {}\nsecond line", "a".repeat(300));
    let cases = [
        ("Bash", json!({"command":"cargo test --locked\n# more"})),
        ("Bash", json!({"command":long_cmd})),
        ("Read", json!({"file_path":format!("{WT}/src/main.rs")})),
        ("Read", json!({"file_path":"/etc/hosts"})),
        ("Read", json!({"file_path":format!("{WT}-other/x.rs")})),
        ("Edit", json!({"file_path":format!("{WT}/a.rs")})),
        ("MultiEdit", json!({"file_path":format!("{WT}/b.rs")})),
        (
            "NotebookEdit",
            json!({"notebook_path":format!("{WT}/n.ipynb")}),
        ),
        ("Write", json!({"file_path":format!("{WT}/hello.txt")})),
        (
            "Grep",
            json!({"pattern":"fn main","path":format!("{WT}/src")}),
        ),
        ("Grep", json!({"pattern":"TODO"})),
        ("Glob", json!({"pattern":"**/*.rs"})),
        (
            "WebFetch",
            json!({"url":"https://example.com/docs","prompt":"x"}),
        ),
        ("WebSearch", json!({"query":"rust tokio process group"})),
        (
            "Task",
            json!({"description":"Review the diff","prompt":"…"}),
        ),
        ("Agent", json!({"description":"Explore\nthe repo"})),
        (
            "TodoWrite",
            json!({"todos":[{"status":"completed"},{"status":"in_progress"},{"status":"completed"},{"status":"pending"}]}),
        ),
        ("mcp__github__create_issue", json!({})),
        ("mcp__my_server__do__thing", json!({})),
        ("ExitPlanMode", json!({})),
        ("Read", json!({"file_path":WT})),
    ];
    let summaries: Vec<(String, String)> = cases
        .iter()
        .map(|(name, input)| (name.to_string(), normalize::tool_summary(name, input, WT)))
        .collect();
    insta::assert_json_snapshot!(summaries);
    let bash = &summaries[1].1;
    assert_eq!(bash.chars().count(), 2 + normalize::MAX_BASH_SUMMARY);
    assert!(summaries.iter().all(|(_, s)| !s.contains('\n')));

    // A full Bash summary of four-byte chars still fits.
    let emoji = "\u{1F600}".repeat(300);
    let bash = normalize::tool_summary("Bash", &json!({"command":emoji}), WT);
    assert_eq!(bash.chars().count(), 2 + normalize::MAX_BASH_SUMMARY);
}

#[test]
fn oversized_summaries_names_and_init_fields_are_capped() {
    let huge = "a".repeat(1 << 20);
    for (name, input) in [
        ("Grep", json!({"pattern":huge})),
        ("Glob", json!({"pattern":huge})),
        ("WebFetch", json!({"url":huge})),
        ("WebSearch", json!({"query":huge})),
        ("Task", json!({"description":huge})),
        ("Read", json!({"file_path":huge})),
        (huge.as_str(), json!({})),
    ] {
        let summary = normalize::tool_summary(name, &input, WT);
        assert!(summary.len() <= normalize::MAX_SUMMARY, "{}", &name[..4]);
        assert!(summary.ends_with('…'));
    }

    let mut n = normalizer();
    let ops = n.on_line(&tool_use("t1", &huge, json!({"pattern":huge})), 1000);
    let EntryBody::ToolCall { name, summary, .. } = &upserts(&ops)[0].body else {
        panic!()
    };
    assert!(name.len() <= normalize::MAX_SUMMARY && summary.len() <= normalize::MAX_SUMMARY);
    let ops = n.on_approval_requested(
        "a1",
        &can_use_tool("t2", &huge, json!({}), Value::Null),
        false,
        1000,
    );
    let EntryBody::ToolCall { name, .. } = &upserts(&ops)[0].body else {
        panic!()
    };
    assert!(name.len() <= normalize::MAX_SUMMARY);

    let huge_text = "b".repeat(normalize::MAX_TEXT * 2);
    let init = json!({"type":"system","subtype":"init","cwd":huge_text,"session_id":"s",
        "model":huge_text,"permissionMode":huge_text,"apiKeySource":huge_text});
    let ops = n.on_line(&init, 1000);
    let EntryBody::SessionInit {
        model,
        permission_mode,
        api_key_source,
        warnings,
        ..
    } = &upserts(&ops)[0].body
    else {
        panic!()
    };
    let fields = [model, permission_mode, api_key_source].map(|f| f.as_ref().unwrap());
    assert!(fields.iter().all(|f| f.len() <= normalize::MAX_TEXT));
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().all(|w| w.len() <= normalize::MAX_TEXT));
    let result = json!({"type":"result","subtype":huge_text,"is_error":false});
    assert!(normalize::parse_result(&result).unwrap().subtype.len() <= normalize::MAX_TEXT);
}

// ---- whole fixture turns, routed as the runner does -------------------------------------------

/// Feeds a fixture through `wire::parse` like the runner: messages and stream events to
/// `on_line`, `can_use_tool` to `on_approval_requested` (then `decide`), cancels to
/// `on_approval_cancelled`; control noise is dropped; `finish` at the end.
fn replay(fixture: &str, decide: impl Fn(&str) -> Option<ApprovalDecision>) -> Vec<EntryOp> {
    let path = format!(
        "{}/tests/fixtures/stream/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut n = Normalizer::new("proc-1".into(), 0, WT.into());
    let mut ts = 1_000;
    let mut ops = n.on_user_message("# Crea hello\n\nScrivi hello.txt", ts);
    for line in text.lines() {
        ts += 40;
        match wire::parse(line.as_bytes()) {
            Inbound::Message(v) | Inbound::StreamEvent(v) => ops.extend(n.on_line(&v, ts)),
            Inbound::CanUseTool(req) => {
                let approval_id = format!("approval-{}", req.request_id);
                let remember = wire::can_remember(&req.permission_suggestions);
                ops.extend(n.on_approval_requested(&approval_id, &req.request, remember, ts));
                if let Some(decision) = decide(&req.request_id) {
                    ops.extend(n.on_approval_resolved(&approval_id, &decision));
                }
            }
            Inbound::ControlCancel { request_id } => {
                ops.extend(n.on_approval_cancelled(&format!("approval-{request_id}")));
            }
            Inbound::ControlResponse { .. }
            | Inbound::ControlRequest { .. }
            | Inbound::KeepAlive
            | Inbound::NotJson => {}
        }
    }
    ops.extend(n.finish(ts));
    ops
}

#[test]
fn fixture_simple_turn() {
    insta::assert_json_snapshot!(view(replay("simple.jsonl", |_| None)));
}

#[test]
fn fixture_approval_turn() {
    let ops = replay("approval.jsonl", |request_id| match request_id {
        "cli-req-1" => Some(ApprovalDecision::Allow { remember: true }),
        "cli-req-2" => Some(ApprovalDecision::Deny {
            message: "no".into(),
            interrupt: false,
        }),
        _ => None,
    });
    insta::assert_json_snapshot!(view(ops));
}

/// M6, from M5's `git push` (spec §13.4): a call denied by a settings rule is `Denied` with the
/// CLI's text, whichever of the three signals arrives: `system/permission_denied`, the
/// `tool_result` (its `non_execution_kind` or its text alone), `result.permission_denials`.
#[test]
fn rule_denials_are_denied_not_failed() {
    let denial = "Permission to use Bash with command git push has been denied.";
    let push = || tool_use("toolu_push", "Bash", json!({"command":"git push"}));
    let result = |ids: &[&str]| {
        let denials: Vec<Value> = ids
            .iter()
            .map(|id| json!({"tool_name":"Bash","tool_use_id":id,"tool_input":{}}))
            .collect();
        json!({"type":"result","subtype":"success","is_error":false,"result":"Blocked.",
               "permission_denials":denials})
    };
    let denied = |ops: &[EntryOp]| {
        tool_status(ops)
            .last()
            .map(|(_, _, status)| status.clone())
            .unwrap()
    };
    let expected = ToolStatus::Denied {
        message: denial.into(),
    };

    // The real sequence: system/permission_denied, then the tool_result with its meta.
    let mut n = normalizer();
    let mut ops = n.on_line(&push(), 1000);
    let line = json!({"type":"system","subtype":"permission_denied","tool_name":"Bash",
        "tool_use_id":"toolu_push","decision_reason_type":"rule","message":denial});
    ops.extend(n.on_line(&line, 1010));
    assert_eq!(denied(&ops), expected);
    let mut result_line = tool_result("toolu_push", json!(denial), true);
    result_line["tool_result_meta"] =
        json!([{"id":"toolu_push","non_execution_kind":"permission-rule"}]);
    ops.extend(n.on_line(&result_line, 1020));
    assert_eq!(denied(&ops), expected);
    let end = n.on_line(&result(&["toolu_push"]), 1030);
    assert!(tool_status(&end).is_empty(), "already Denied: no update");
    insta::assert_json_snapshot!(view(ops));

    // Only the text of the tool_result.
    let mut n = normalizer();
    let mut ops = n.on_line(&push(), 1000);
    ops.extend(n.on_line(&tool_result("toolu_push", json!(denial), true), 1010));
    assert_eq!(denied(&ops), expected);
    assert!(normalize::is_rule_denial(denial));
    assert!(!normalize::is_rule_denial("Permission to use Bash: yes"));

    // Only `permission_denials`: a failed call becomes Denied with its output, an unknown or
    // succeeded one is left alone.
    let mut n = normalizer();
    let mut ops = n.on_line(&push(), 1000);
    ops.extend(n.on_line(&tool_result("toolu_push", json!("blocked"), true), 1010));
    ops.extend(n.on_line(&tool_use("toolu_ok", "Bash", json!({"command":"ls"})), 1020));
    ops.extend(n.on_line(&tool_result("toolu_ok", json!("a.txt"), false), 1030));
    let end = n.on_line(&result(&["toolu_push", "toolu_ok", "toolu_gone"]), 1040);
    let statuses = tool_status(&end);
    assert_eq!(statuses.len(), 1, "{statuses:?}");
    assert_eq!(
        statuses[0].2,
        ToolStatus::Denied {
            message: "blocked".into()
        }
    );
    // A user's denial stays as the user wrote it.
    let mut n = normalizer();
    n.on_line(&push(), 1000);
    n.on_approval_requested(
        "ap-1",
        &json!({"tool_use_id":"toolu_push","tool_name":"Bash",
        "input":{"command":"git push"}}),
        false,
        1001,
    );
    n.on_approval_resolved(
        "ap-1",
        &ApprovalDecision::Deny {
            message: "no".into(),
            interrupt: false,
        },
    );
    let ops = n.on_line(&tool_result("toolu_push", json!(denial), true), 1010);
    assert_eq!(
        denied(&ops),
        ToolStatus::Denied {
            message: "no".into()
        }
    );
}

/// M6, from M5's Stop (spec §13.4): after `on_stop_requested` the error `result` answering the
/// interrupt is a `TurnEnd` with `stopped` and without the CLI's `[ede_diagnostic]`, and the
/// approved call the CLI rejected on the interrupt is `Cancelled` without that canned text.
/// Without a stop nothing changes.
#[test]
fn a_stopped_turn_ends_interrupted_not_failed() {
    let interrupted = json!({"type":"result","subtype":"error_during_execution","is_error":true,
        "num_turns":4,"duration_ms":4291,"total_cost_usd":0.2,"permission_denials":[],
        "errors":["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"],
        "terminal_reason":"aborted_tools"});
    let mut rejected = tool_result(
        "toolu_wait",
        json!("The user doesn't want to proceed with this tool use. STOP what you are doing."),
        true,
    );
    rejected["tool_result_meta"] =
        json!([{"id":"toolu_wait","non_execution_kind":"user-rejected"}]);
    let wait = tool_use("toolu_wait", "Bash", json!({"command":"sleep 41"}));

    let mut n = normalizer();
    let mut ops = n.on_line(&wait, 1000);
    n.on_stop_requested(atm_types::StopReason::UserStop);
    n.on_stop_requested(atm_types::StopReason::AppShutdown); // the first reason wins
    ops.extend(n.on_line(&rejected, 1010));
    ops.extend(n.on_line(&interrupted, 1020));
    insta::assert_json_snapshot!(view(ops));

    let mut n = normalizer();
    n.on_line(&wait, 1000);
    let ops = n.on_line(&rejected, 1010);
    assert_eq!(tool_status(&ops)[0].2, ToolStatus::Failed);
    let end = n.on_line(&interrupted, 1020);
    let body = &upserts(&end)[0].body;
    assert!(
        matches!(body, EntryBody::TurnEnd { stopped: None, text: Some(t), .. }
            if t.starts_with("[ede_diagnostic]")),
        "{body:?}"
    );

    // A stop that lost the race against a successful result keeps it as it is.
    let mut n = normalizer();
    n.on_stop_requested(atm_types::StopReason::UserStop);
    let ok = json!({"type":"result","subtype":"success","is_error":false,"result":"Done."});
    let body = upserts(&n.on_line(&ok, 1000))[0].body.clone();
    assert!(
        matches!(body, EntryBody::TurnEnd { stopped: None, text: Some(ref t), .. } if t == "Done."),
        "{body:?}"
    );

    // Finding M6 #18: a real error already queued when Stop was pressed keeps its text and is
    // not labelled as the user's stop; the answer to the interrupt still is, whatever its
    // `terminal_reason` (`aborted_streaming`, or only `[ede_diagnostic]` text).
    for real in [
        json!({"type":"result","subtype":"success","is_error":true,
            "result":"API Error: 529 Overloaded"}),
        json!({"type":"result","subtype":"error_max_turns","is_error":true,
            "errors":["Reached maximum number of turns (3)"],"terminal_reason":"max_turns"}),
    ] {
        let mut n = normalizer();
        n.on_stop_requested(atm_types::StopReason::UserStop);
        let body = upserts(&n.on_line(&real, 1000))[0].body.clone();
        assert!(
            matches!(body, EntryBody::TurnEnd { stopped: None, text: Some(ref t), .. }
                if !t.is_empty() && !t.starts_with("[ede_diagnostic]")),
            "{body:?}"
        );
    }
    for answer in [
        json!({"type":"result","subtype":"error_during_execution","is_error":true,
            "errors":["something"],"terminal_reason":"aborted_streaming"}),
        json!({"type":"result","subtype":"error_during_execution","is_error":true}),
    ] {
        let mut n = normalizer();
        n.on_stop_requested(atm_types::StopReason::UserStop);
        let body = upserts(&n.on_line(&answer, 1000))[0].body.clone();
        assert!(
            matches!(
                body,
                EntryBody::TurnEnd {
                    stopped: Some(atm_types::StopReason::UserStop),
                    text: None,
                    ..
                }
            ),
            "{body:?}"
        );
    }
}
