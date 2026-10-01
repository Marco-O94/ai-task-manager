//! M5 (spec §11.2): the sanitized traffic of the REAL Claude Code CLI 2.1.283, captured by
//! `tests/real_cli.rs` into `tests/fixtures/real/`, replayed without any CLI. Each turn goes
//! through `wire::parse` and the normalizer as the runner routes it, with the approval decisions
//! read back from the turn's `stdin.jsonl`; golden outlines plus the protocol facts M5 observed.

use std::collections::BTreeMap;

use atm_core::normalize::{self, EntryOp, Normalizer};
use atm_core::wire::{self, Inbound, Pending};
use atm_types::{ApprovalDecision, Entry, EntryBody, LimitKind, StopReason, ToolStatus};
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/real/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn lines(name: &str) -> Vec<Value> {
    fixture(name)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The host's answers to the CLI's `can_use_tool`, by `request_id`, from `stdin.jsonl`.
fn decisions(stdin: &str) -> BTreeMap<String, ApprovalDecision> {
    let mut out = BTreeMap::new();
    for frame in lines(stdin) {
        if frame["type"] != "control_response" {
            continue;
        }
        let r = &frame["response"]["response"];
        let decision = match r["behavior"].as_str() {
            Some("allow") => ApprovalDecision::Allow {
                remember: r.get("updatedPermissions").is_some(),
            },
            Some("deny") => ApprovalDecision::Deny {
                message: r["message"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches(wire::DENY_PREFIX)
                    .to_owned(),
                interrupt: r["interrupt"] == true,
            },
            _ => continue,
        };
        let id = frame["response"]["request_id"].as_str().unwrap().to_owned();
        out.insert(id, decision);
    }
    out
}

/// A replayed turn: the final store (what the UI shows) and the number of typing previews.
struct Replay {
    store: BTreeMap<u32, Entry>,
    typing: usize,
}

/// Routes one captured turn like the runner (spec §7.4): messages and stream events to
/// `on_line`, `can_use_tool` to `on_approval_requested` then the decision the host sent,
/// cancels to `on_approval_cancelled`, control noise dropped; `finish` at the end. The worktree
/// is the `cwd` of the turn's `system/init`. A turn the host interrupted (the user's Stop) gets
/// `on_stop_requested` at the answer to the interrupt, which precedes everything it caused.
fn replay(label: &str) -> Replay {
    let stdout = lines(&format!("{label}.stdout.jsonl"));
    let stdin = format!("{label}.stdin.jsonl");
    let decided = decisions(&stdin);
    let worktree = stdout
        .iter()
        .find(|v| v["type"] == "system" && v["subtype"] == "init")
        .and_then(|v| v["cwd"].as_str())
        .unwrap_or("/tmp/atm-real/wt")
        .to_owned();
    let prompt = lines(&stdin)
        .into_iter()
        .find(|v| v["type"] == "user")
        .and_then(|v| v["message"]["content"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let interrupt = lines(&stdin)
        .into_iter()
        .find(|v| v["request"]["subtype"] == "interrupt")
        .and_then(|v| v["request_id"].as_str().map(str::to_owned));
    let mut n = Normalizer::new("proc-1".into(), 0, worktree);
    let mut ts = 1_000;
    let mut ops = n.on_user_message(&prompt, ts);
    for v in &stdout {
        ts += 40;
        match wire::parse(v.to_string().as_bytes()) {
            Inbound::Message(v) | Inbound::StreamEvent(v) => ops.extend(n.on_line(&v, ts)),
            Inbound::CanUseTool(req) => {
                let id = format!("approval-{}", req.request_id);
                let remember = wire::can_remember(&req.permission_suggestions);
                ops.extend(n.on_approval_requested(&id, &req.request, remember, ts));
                if let Some(decision) = decided.get(&req.request_id) {
                    ops.extend(n.on_approval_resolved(&id, decision));
                }
            }
            Inbound::ControlCancel { request_id } => {
                ops.extend(n.on_approval_cancelled(&format!("approval-{request_id}")));
            }
            Inbound::ControlResponse { request_id, .. }
                if interrupt.as_ref() == Some(&request_id) =>
            {
                n.on_stop_requested(StopReason::UserStop);
            }
            Inbound::ControlResponse { .. }
            | Inbound::ControlRequest { .. }
            | Inbound::McpMessage { .. }
            | Inbound::KeepAlive
            | Inbound::NotJson => {}
        }
    }
    ops.extend(n.finish(ts));
    let mut store = BTreeMap::new();
    let mut typing = 0;
    for op in ops {
        match op {
            EntryOp::Upsert(e) => {
                if store.get(&e.idx).is_none_or(|old: &Entry| e.rev > old.rev) {
                    store.insert(e.idx, e);
                }
            }
            EntryOp::Typing(Some(_)) => typing += 1,
            EntryOp::Typing(None) => {}
        }
    }
    Replay { store, typing }
}

fn short(s: &str, max: usize) -> String {
    let one = s.replace('\n', "⏎");
    match one.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &one[..i]),
        None => one,
    }
}

/// One line per final entry, ids and timings left out.
fn outline(r: &Replay) -> String {
    let mut out = String::new();
    for e in r.store.values() {
        let line = match &e.body {
            EntryBody::UserMessage { text } => format!("UserMessage {}", short(text, 60)),
            EntryBody::SessionInit {
                model,
                permission_mode,
                api_key_source,
                mcp_servers,
                warnings,
            } => format!(
                "SessionInit model={model:?} mode={permission_mode:?} \
                 apiKeySource={api_key_source:?} mcp_servers={mcp_servers} warnings={warnings:?}"
            ),
            EntryBody::AssistantText { text } => format!("AssistantText {}", short(text, 80)),
            EntryBody::Thinking { text } => format!("Thinking {}", short(text, 40)),
            EntryBody::ToolCall {
                name,
                summary,
                status,
                output,
                ..
            } => {
                let status = match status {
                    ToolStatus::AwaitingApproval { reason, .. } => {
                        format!("AwaitingApproval(reason={reason:?})")
                    }
                    other => format!("{other:?}"),
                };
                let summary = if name == "Bash" {
                    short(summary, 60)
                } else {
                    summary.clone()
                };
                let output = output
                    .as_ref()
                    .map(|o| format!(" → {}", short(&o.text, 60)))
                    .unwrap_or_default();
                format!("ToolCall {name} [{summary}] {status}{output}")
            }
            EntryBody::TurnEnd {
                subtype,
                is_error,
                permission_denials,
                limit,
                text,
                num_turns,
                stopped,
                ..
            } => format!(
                "TurnEnd {subtype} is_error={is_error} num_turns={num_turns:?} \
                 denials={permission_denials} limit={limit:?} text={:?}{}",
                text.as_deref().map(|t| short(t, 80)),
                stopped.map(|s| format!(" stopped={s}")).unwrap_or_default()
            ),
            other => format!("{} {other:?}", other.kind()),
        };
        out.push_str(&format!("{:>2} {line}\n", e.idx));
    }
    out.push_str(&format!("typing previews: {}\n", r.typing));
    out
}

const TURNS: &[&str] = &[
    "checklist-1-readme",
    "checklist-2-approvals",
    "checklist-3-stop",
    "checklist-4-interrupted",
    "checklist-5-continue",
    "push-1-git-push",
    "isolated-1-keyword",
    "probes-no-initialize",
];

#[test]
fn real_turns_golden() {
    for label in TURNS {
        insta::assert_snapshot!(format!("real_{label}"), outline(&replay(label)));
    }
}

/// Every turn of a subscription login says `apiKeySource: "none"`, runs in its worktree and,
/// with `--strict-mcp-config` and no `--mcp-config`, has no MCP server: no SessionInit warning.
#[test]
fn every_real_init_is_a_subscription_without_mcp_servers() {
    for label in TURNS {
        let init = lines(&format!("{label}.stdout.jsonl"))
            .into_iter()
            .find(|v| v["type"] == "system" && v["subtype"] == "init")
            .unwrap_or_else(|| panic!("{label}: no system/init"));
        assert_eq!(init["apiKeySource"], "none", "{label}");
        assert!(normalize::NO_API_KEY_SOURCES.contains(&"none"));
        assert_eq!(init["mcp_servers"], json!([]), "{label}");
        let r = replay(label);
        let warnings: Vec<&Vec<String>> = r
            .store
            .values()
            .filter_map(|e| match &e.body {
                EntryBody::SessionInit { warnings, .. } => Some(warnings),
                _ => None,
            })
            .collect();
        assert_eq!(warnings, [&Vec::<String>::new()], "{label}");
    }
}

/// The real `can_use_tool` (spec §7.8): `tool_use_id` present, `addRules` + `addDirectories` +
/// `setMode` suggestions, `decision_reason` a plain string when present (else `blocked_path`).
/// "Consenti sempre" forwards only the `addRules` rule and remembers `Bash(touch approved.txt)`.
#[test]
fn real_can_use_tool_is_rememberable_and_forwards_only_the_rule() {
    let requests: Vec<wire::CanUseTool> = lines("checklist-2-approvals.stdout.jsonl")
        .into_iter()
        .chain(lines("checklist-3-stop.stdout.jsonl"))
        .filter_map(|v| match wire::parse(v.to_string().as_bytes()) {
            Inbound::CanUseTool(req) => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 3, "touch, touch again, the Python wait");
    let touch = &requests[0];
    assert_eq!(touch.input["command"], "touch approved.txt");
    let kinds: Vec<&str> = touch
        .permission_suggestions
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["addRules", "addDirectories", "setMode"]);
    assert!(touch.request.get("decision_reason").is_none());
    assert!(
        touch.request["blocked_path"]
            .as_str()
            .is_some_and(|p| p.ends_with("/approved.txt"))
    );
    assert!(wire::can_remember(&touch.permission_suggestions));
    let pending = Pending::new("approval-1".into(), touch);
    assert_eq!(
        wire::remembered_rules(&pending),
        ["Bash(touch approved.txt)"]
    );
    let frame = wire::approval_response(&pending, &ApprovalDecision::Allow { remember: true });
    assert_eq!(
        frame["response"]["response"]["updatedPermissions"],
        json!([{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch approved.txt"}],
                "behavior":"allow","destination":"session"}])
    );
    let wait = &requests[2];
    assert_eq!(
        wait.request["decision_reason"],
        "This command requires approval"
    );
    assert_eq!(wait.request["decision_reason_type"], "other");
    // What the app sent in turn 2: "Consenti sempre" with only the rule.
    let sent = decisions("checklist-2-approvals.stdin.jsonl");
    assert_eq!(
        sent.values().collect::<Vec<_>>(),
        [&ApprovalDecision::Allow { remember: true }]
    );
}

/// Stop mid-turn (spec §7.9): the interrupt is answered `{"still_queued":[]}`, then the CLI adds
/// a user line and ends with `result` `error_during_execution` (is_error, no limit class).
#[test]
fn real_interrupt_ends_with_error_during_execution() {
    let out = lines("checklist-3-stop.stdout.jsonl");
    let interrupt_id = lines("checklist-3-stop.stdin.jsonl")
        .into_iter()
        .find(|v| v["request"]["subtype"] == "interrupt")
        .and_then(|v| v["request_id"].as_str().map(str::to_owned))
        .unwrap();
    let answer = out
        .iter()
        .find(|v| v["response"]["request_id"] == interrupt_id.as_str())
        .unwrap();
    assert_eq!(answer["response"]["response"], json!({"still_queued": []}));
    let result =
        normalize::parse_result(out.iter().find(|v| v["type"] == "result").unwrap()).unwrap();
    assert_eq!(result.subtype, "error_during_execution");
    assert!(result.is_error);
    assert_eq!(result.limit, None);
    assert!(out.iter().any(|v| {
        v["type"] == "user"
            && v["message"]["content"][0]["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("[Request interrupted by user"))
    }));
    // M6: what the app shows of it: "Interrotto dall'utente" without the `[ede_diagnostic]`,
    // and the approved wait the CLI rejected on the interrupt is cancelled, not failed.
    let r = replay("checklist-3-stop");
    let end = r.store.values().rev().find_map(|e| match &e.body {
        EntryBody::TurnEnd { stopped, text, .. } => Some((*stopped, text.clone())),
        _ => None,
    });
    assert_eq!(end, Some((Some(StopReason::UserStop), None)));
    let wait = r.store.values().find_map(|e| match &e.body {
        EntryBody::ToolCall {
            summary, status, ..
        } if summary.contains("time.sleep(41)") => Some(status.clone()),
        _ => None,
    });
    assert_eq!(wait, Some(ToolStatus::Cancelled));
}

/// `git push` never asks: the `--settings` deny rule rejects it (spec §7.8), the CLI says so with
/// `system/permission_denied`, a `tool_result` marked `permission-rule` and the entry of
/// `permission_denials`; the call is `Denied` (M6; `Failed` before) with the CLI's text.
#[test]
fn real_git_push_is_denied_by_the_rule() {
    let out = lines("push-1-git-push.stdout.jsonl");
    assert!(
        !out.iter()
            .any(|v| v["request"]["subtype"] == "can_use_tool"),
        "git push must not ask"
    );
    let result = out.iter().find(|v| v["type"] == "result").unwrap();
    assert_eq!(result["permission_denials"][0]["tool_name"], "Bash");
    assert_eq!(
        result["permission_denials"][0]["tool_input"]["command"],
        "git push"
    );
    let r = replay("push-1-git-push");
    let push = r
        .store
        .values()
        .find_map(|e| match &e.body {
            EntryBody::ToolCall {
                summary,
                status,
                output,
                ..
            } if summary == "$ git push" => Some((status.clone(), output.clone().unwrap().text)),
            _ => None,
        })
        .unwrap();
    let denial = "Permission to use Bash with command git push has been denied.";
    assert_eq!(
        push.0,
        ToolStatus::Denied {
            message: denial.into()
        }
    );
    assert_eq!(push.1, denial);
    assert!(out.iter().any(|v| v["subtype"] == "permission_denied"));
    assert!(out.iter().any(|v| {
        v["tool_result_meta"][0]["non_execution_kind"] == normalize::RULE_DENIED_KIND
    }));
}

/// `--resume` of an unknown session (spec §7.9): exit before `initialize` is answered, the text
/// on stderr and in the `errors` of an `error_during_execution` result.
#[test]
fn real_resume_failure_matches_the_pattern() {
    let stderr = fixture("probes-resume-not-found.stderr.log");
    assert!(normalize::is_resume_failure(&stderr), "{stderr}");
    let out = lines("probes-resume-not-found.stdout.jsonl");
    assert_eq!(
        out.len(),
        1,
        "only the result: initialize is never answered"
    );
    let result = normalize::parse_result(&out[0]).unwrap();
    assert_eq!(result.subtype, "error_during_execution");
    assert!(
        result
            .text
            .as_deref()
            .is_some_and(normalize::is_resume_failure)
    );
    assert_eq!(result.limit, None);
}

/// The answer to `initialize`: after the user's SessionStart hooks start, with the account
/// (redacted in any log) and the current mode; `system/init` only comes with a user message.
#[test]
fn real_initialize_answer() {
    let out = lines("probes-initialize.stdout.jsonl");
    assert!(out.iter().all(|v| v["subtype"] != "init"));
    let answer = out
        .iter()
        .find(|v| v["type"] == "control_response")
        .unwrap();
    let Inbound::ControlResponse { result, .. } = wire::parse(answer.to_string().as_bytes()) else {
        panic!("not a control response: {answer}");
    };
    let response = result.unwrap();
    assert_eq!(response["current_permission_mode"], "acceptEdits");
    assert_eq!(response["account"]["email"], wire::REDACTED);
    assert_eq!(response["account"]["organization"], wire::REDACTED);
    assert_eq!(response["account"]["subscriptionType"], "Claude Max");
}

/// Traffic the normalizer leaves to the raw log (spec §7.6 "altri system/*"): hook lifecycle,
/// `system/status`, `rate_limit_event` (shape below) produce no entry.
#[test]
fn real_noise_lines_make_no_entry() {
    let mut rate_limit = None;
    for label in TURNS {
        for v in lines(&format!("{label}.stdout.jsonl")) {
            let kind = v["type"].as_str().unwrap_or_default();
            let sub = v["subtype"].as_str().unwrap_or_default();
            let noise = kind == "rate_limit_event"
                || (kind == "system" && (sub.starts_with("hook_") || sub == "status"));
            if !noise {
                continue;
            }
            let mut n = Normalizer::new("p".into(), 0, "/tmp/atm-real/wt".into());
            assert!(n.on_line(&v, 1).is_empty(), "{v}");
            if kind == "rate_limit_event" {
                rate_limit = Some(v);
            }
        }
    }
    let info = &rate_limit.expect("a rate_limit_event")["rate_limit_info"];
    assert_eq!(info["status"], "allowed");
    assert_eq!(info["rateLimitType"], "five_hour");
    assert!(info["unifiedWindows"]["seven_day"]["utilization"].is_number());
}

/// Isolated (spec §10.2, E11): CLAUDE.md is not in the context under `--setting-sources=user`;
/// the append prompt makes the agent read it (here with `cat`) and learn the keyword.
#[test]
fn real_isolated_claude_md_is_read_on_request() {
    let r = replay("isolated-1-keyword");
    let said: Vec<&str> = r
        .store
        .values()
        .filter_map(|e| match &e.body {
            EntryBody::AssistantText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        said.iter().any(|t| t.starts_with("NOT_IN_CONTEXT")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|t| t.contains("TANGERINE-7031")),
        "{said:?}"
    );
    assert!(r.store.values().any(|e| matches!(&e.body,
        EntryBody::ToolCall { summary, .. } if summary.contains("CLAUDE.md"))));
}

/// The stopped and the dropped turns end with their tools closed and no limit class.
#[test]
fn real_interrupted_turns_close_their_tools() {
    for label in ["checklist-3-stop", "checklist-4-interrupted"] {
        let r = replay(label);
        for e in r.store.values() {
            if let EntryBody::ToolCall { status, .. } = &e.body {
                assert!(
                    !matches!(
                        status,
                        ToolStatus::Running | ToolStatus::AwaitingApproval { .. }
                    ),
                    "{label}: {status:?}"
                );
            }
            if let EntryBody::TurnEnd { limit, .. } = &e.body {
                assert_ne!(*limit, Some(LimitKind::UsageLimit), "{label}");
            }
        }
    }
}
