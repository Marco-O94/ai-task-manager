//! Serde contract: every type round-trips, every enum's JSON is pinned by an insta snapshot.

use std::fmt::Debug;
use std::str::FromStr;

use atm_types::debug::*;
use atm_types::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(v: &T) {
    let json = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(&back, v, "{json}");
}

fn round_trip_all<T: Serialize + DeserializeOwned + PartialEq + Debug>(vs: &[T]) {
    vs.iter().for_each(round_trip);
}

fn id(n: u8) -> Id {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
}

fn task() -> Task {
    Task {
        id: id(1),
        project_id: id(2),
        title: "Aggiungi una riga".into(),
        description: "al README".into(),
        status: TaskStatus::InReview,
        position: 1536.5,
        created_at: 1_790_000_000_000,
        updated_at: 1_790_000_000_500,
    }
}

fn project() -> Project {
    Project {
        id: id(2),
        name: "demo".into(),
        repo_path: "/Users/me/demo".into(),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: Some("opus".into()),
        config_policy: ConfigPolicy::Isolated,
        trusted: false,
        allow_bypass: false,
        created_at: 1,
        updated_at: 2,
        trust_error: None,
    }
}

fn attempt() -> AttemptView {
    AttemptView {
        id: id(3),
        task_id: id(1),
        state: AttemptState::Active,
        branch: "atm/00000000-aggiungi-una-riga".into(),
        target_branch: "main".into(),
        base_commit: "abc123".into(),
        worktree_path: "/Users/me/.ai-task-manager/worktrees/x".into(),
        worktree_state: WorktreeState::Present,
        permission_mode: PermissionMode::Default,
        model: None,
        effort: Some(Effort::XHigh),
        session_started: true,
        merge_commit: None,
        running: true,
        pending_approvals: 1,
        created_at: 3,
        closed_at: None,
    }
}

fn process() -> ProcessInfo {
    ProcessInfo {
        id: id(4),
        seq: 1,
        prompt: "# titolo\n\ndescrizione".into(),
        status: ProcessStatus::Failed,
        stop_reason: Some(StopReason::UsageLimit),
        result_subtype: Some("success".into()),
        is_error: Some(true),
        cost_usd_estimate: Some(0.125),
        duration_ms: Some(12_000),
        num_turns: Some(3),
        head_after: Some("def456".into()),
        started_at: 10,
        finished_at: Some(20),
    }
}

fn logged_in() -> AuthState {
    AuthState::LoggedIn {
        auth_method: Some("claude.ai".into()),
        api_provider: Some("firstParty".into()),
        email: Some("user@example.com".into()),
        org_name: None,
        subscription_type: Some("max".into()),
    }
}

fn env_status() -> EnvStatus {
    EnvStatus {
        claude: ClaudeInfo {
            path: Some("/Users/me/.local/bin/claude".into()),
            version: Some("2.1.283".into()),
            supported: true,
            min_version: CLAUDE_MIN_VERSION.into(),
            tested_version: CLAUDE_TESTED_VERSION.into(),
        },
        auth: logged_in(),
        git_version: Some("2.54.0".into()),
        api_key_in_env: false,
        cloud_provider_env: false,
        base_url_env: false,
        paused: Some("usage limit".into()),
        running: 1,
        max_running: 2,
        problems: vec!["x".into()],
        checked_at: 42,
    }
}

fn tool_statuses() -> Vec<ToolStatus> {
    vec![
        ToolStatus::Running,
        ToolStatus::AwaitingApproval {
            approval_id: id(9),
            can_remember: true,
            reason: Some("Bash non è tra i tool consentiti".into()),
        },
        ToolStatus::Denied {
            message: "no".into(),
        },
        ToolStatus::Succeeded,
        ToolStatus::Failed,
        ToolStatus::Cancelled,
    ]
}

fn entry_bodies() -> Vec<EntryBody> {
    vec![
        EntryBody::UserMessage {
            text: "ciao".into(),
        },
        EntryBody::SessionInit {
            model: Some("claude-opus".into()),
            permission_mode: Some("acceptEdits".into()),
            api_key_source: Some("none".into()),
            mcp_servers: 0,
            warnings: vec![],
        },
        EntryBody::AssistantText { text: "ok".into() },
        EntryBody::Thinking { text: "hmm".into() },
        EntryBody::ToolCall {
            tool_use_id: "toolu_1".into(),
            name: "Write".into(),
            summary: "Write hello.txt".into(),
            input: r#"{"file_path":"hello.txt"}"#.into(),
            status: ToolStatus::Succeeded,
            output: Some(ToolOutput {
                text: "done".into(),
                truncated_bytes: 0,
                is_error: false,
            }),
        },
        EntryBody::ApiRetry {
            attempt: 1,
            max_retries: 10,
            delay_ms: 500,
            error: "overloaded".into(),
        },
        EntryBody::TurnEnd {
            subtype: "success".into(),
            is_error: false,
            duration_ms: Some(1000),
            num_turns: Some(2),
            cost_usd_estimate: Some(0.5),
            permission_denials: 0,
            text: Some("fatto".into()),
            limit: Some(LimitKind::RateLimit),
            stopped: None,
        },
        EntryBody::TurnEnd {
            subtype: "error_during_execution".into(),
            is_error: true,
            duration_ms: Some(400),
            num_turns: Some(1),
            cost_usd_estimate: None,
            permission_denials: 0,
            text: None,
            limit: None,
            stopped: Some(StopReason::UserStop),
        },
        EntryBody::Notice {
            level: Level::Warn,
            text: "Contesto compattato".into(),
            action: None,
        },
        EntryBody::Notice {
            level: Level::Error,
            text: "Sessione non trovata".into(),
            action: Some(NoticeAction::NewSession),
        },
        EntryBody::Stderr {
            text: "warning".into(),
        },
    ]
}

fn entry(idx: u32, body: EntryBody) -> Entry {
    Entry {
        idx,
        rev: 2,
        process_id: id(4),
        ts: 100 + i64::from(idx),
        parent_tool_use_id: (idx % 2 == 1).then(|| "toolu_parent".into()),
        body,
    }
}

fn entries() -> Vec<Entry> {
    entry_bodies()
        .into_iter()
        .enumerate()
        .map(|(i, b)| entry(i as u32, b))
        .collect()
}

fn transcript_msgs() -> Vec<TranscriptMsg> {
    vec![
        TranscriptMsg::Snapshot {
            entries: vec![entry(0, EntryBody::UserMessage { text: "a".into() })],
            has_more: true,
            typing: Some("sto scri".into()),
        },
        TranscriptMsg::Upsert {
            entries: vec![entry(1, EntryBody::AssistantText { text: "b".into() })],
        },
        TranscriptMsg::Typing { text: None },
    ]
}

fn approval_decisions() -> Vec<ApprovalDecision> {
    vec![
        ApprovalDecision::Allow { remember: true },
        ApprovalDecision::Deny {
            message: "usa un altro approccio".into(),
            interrupt: false,
        },
    ]
}

fn merge_outcomes() -> Vec<MergeOutcome> {
    vec![
        MergeOutcome::Merged {
            commit: "c0ffee".into(),
            strategy: MergeStrategy::FfCheckedOut,
            cleanup_warning: Some("worktree non rimosso".into()),
        },
        MergeOutcome::NothingToMerge,
        MergeOutcome::Conflicts {
            files: vec!["src/a b.rs".into()],
        },
    ]
}

fn auth_states() -> Vec<AuthState> {
    vec![
        logged_in(),
        AuthState::LoggedOut,
        AuthState::Unknown {
            reason: "timeout".into(),
        },
    ]
}

fn file_diff() -> FileDiff {
    FileDiff {
        path: "src/new.rs".into(),
        old_path: Some("src/old.rs".into()),
        status: FileStatus::Renamed,
        additions: 2,
        deletions: 1,
        binary: false,
        too_large: false,
        omitted: false,
        lines: vec![
            DiffLine {
                kind: LineKind::Hunk,
                old_no: None,
                new_no: None,
                text: "@@ -1,2 +1,3 @@".into(),
            },
            DiffLine {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(1),
                text: "fn main() {}".into(),
            },
        ],
    }
}

const ERROR_CODES: [ErrorCode; 17] = [
    ErrorCode::NotFound,
    ErrorCode::Invalid,
    ErrorCode::Conflict,
    ErrorCode::Busy,
    ErrorCode::ConcurrencyLimit,
    ErrorCode::UsageLimited,
    ErrorCode::ClaudeNotFound,
    ErrorCode::NotLoggedIn,
    ErrorCode::WorktreeMissing,
    ErrorCode::BranchMismatch,
    ErrorCode::TargetCheckoutDirty,
    ErrorCode::GitIdentityMissing,
    ErrorCode::Git,
    ErrorCode::Io,
    ErrorCode::Db,
    ErrorCode::NotImplemented,
    ErrorCode::Internal,
];
const LEVELS: [Level; 3] = [Level::Info, Level::Warn, Level::Error];
const NOTICE_ACTIONS: [NoticeAction; 1] = [NoticeAction::NewSession];
const LIMIT_KINDS: [LimitKind; 4] = [
    LimitKind::UsageLimit,
    LimitKind::RateLimit,
    LimitKind::AuthFailure,
    LimitKind::Billing,
];
const LOGIN_METHODS: [LoginMethod; 3] = [
    LoginMethod::ClaudeAi,
    LoginMethod::Console,
    LoginMethod::Sso,
];
const OPEN_TARGETS: [OpenTarget; 3] =
    [OpenTarget::Finder, OpenTarget::Terminal, OpenTarget::Editor];
const FILE_STATUSES: [FileStatus; 6] = [
    FileStatus::Added,
    FileStatus::Modified,
    FileStatus::Deleted,
    FileStatus::Renamed,
    FileStatus::Copied,
    FileStatus::TypeChanged,
];
const LINE_KINDS: [LineKind; 5] = [
    LineKind::Hunk,
    LineKind::Context,
    LineKind::Add,
    LineKind::Del,
    LineKind::Meta,
];
const MERGE_STRATEGIES: [MergeStrategy; 2] =
    [MergeStrategy::UpdateRef, MergeStrategy::FfCheckedOut];

/// The serde string, `as_str`, `Display` and `FromStr` of a DB enum all agree.
fn check_db_enum<T>(all: &[T])
where
    T: Serialize + DeserializeOwned + FromStr<Err = AppError> + ToString + PartialEq + Debug + Copy,
{
    for &v in all {
        let s = v.to_string();
        assert_eq!(
            serde_json::to_value(v).unwrap(),
            serde_json::Value::String(s.clone())
        );
        assert_eq!(T::from_str(&s).unwrap(), v);
    }
    assert_eq!(T::from_str("bogus").unwrap_err().code, ErrorCode::Invalid);
    round_trip_all(all);
}

#[test]
fn db_enums_match_db_and_cli_strings() {
    check_db_enum(TaskStatus::ALL);
    check_db_enum(AttemptState::ALL);
    check_db_enum(WorktreeState::ALL);
    check_db_enum(ProcessStatus::ALL);
    check_db_enum(PermissionMode::ALL);
    check_db_enum(ConfigPolicy::ALL);
    check_db_enum(Effort::ALL);
    check_db_enum(StopReason::ALL);
    assert_eq!(PermissionMode::AcceptEdits.as_str(), "acceptEdits");
    assert_eq!(Effort::XHigh.as_str(), "xhigh");
}

#[test]
fn enum_snapshots() {
    insta::assert_json_snapshot!("task_status", TaskStatus::ALL);
    insta::assert_json_snapshot!("attempt_state", AttemptState::ALL);
    insta::assert_json_snapshot!("worktree_state", WorktreeState::ALL);
    insta::assert_json_snapshot!("process_status", ProcessStatus::ALL);
    insta::assert_json_snapshot!("permission_mode", PermissionMode::ALL);
    insta::assert_json_snapshot!("config_policy", ConfigPolicy::ALL);
    insta::assert_json_snapshot!("effort", Effort::ALL);
    insta::assert_json_snapshot!("stop_reason", StopReason::ALL);
    insta::assert_json_snapshot!("error_code", ERROR_CODES);
    insta::assert_json_snapshot!("level", LEVELS);
    insta::assert_json_snapshot!("notice_action", NOTICE_ACTIONS);
    insta::assert_json_snapshot!("limit_kind", LIMIT_KINDS);
    insta::assert_json_snapshot!("login_method", LOGIN_METHODS);
    insta::assert_json_snapshot!("open_target", OPEN_TARGETS);
    insta::assert_json_snapshot!("file_status", FILE_STATUSES);
    insta::assert_json_snapshot!("line_kind", LINE_KINDS);
    insta::assert_json_snapshot!("merge_strategy", MERGE_STRATEGIES);
}

#[test]
fn tagged_enum_snapshots() {
    insta::assert_json_snapshot!("auth_state", auth_states());
    insta::assert_json_snapshot!("entry_body", entry_bodies());
    insta::assert_json_snapshot!("tool_status", tool_statuses());
    insta::assert_json_snapshot!("transcript_msg", transcript_msgs());
    insta::assert_json_snapshot!("approval_decision", approval_decisions());
    insta::assert_json_snapshot!("merge_outcome", merge_outcomes());
}

#[test]
fn enums_round_trip() {
    round_trip_all(&ERROR_CODES);
    round_trip_all(&LEVELS);
    round_trip_all(&NOTICE_ACTIONS);
    round_trip_all(&LIMIT_KINDS);
    round_trip_all(&LOGIN_METHODS);
    round_trip_all(&OPEN_TARGETS);
    round_trip_all(&FILE_STATUSES);
    round_trip_all(&LINE_KINDS);
    round_trip_all(&MERGE_STRATEGIES);
    round_trip_all(&auth_states());
    round_trip_all(&entry_bodies());
    round_trip_all(&tool_statuses());
    round_trip_all(&transcript_msgs());
    round_trip_all(&approval_decisions());
    round_trip_all(&merge_outcomes());
}

#[test]
fn entry_kind_is_the_serde_tag() {
    for body in entry_bodies() {
        assert_eq!(serde_json::to_value(&body).unwrap()["type"], body.kind());
    }
}

#[test]
fn model_types_round_trip() {
    round_trip(&project());
    round_trip(&task());
    round_trip(&TaskCard {
        task: task(),
        attempt_id: Some(id(3)),
        attempt_state: Some(AttemptState::Active),
        branch: Some("atm/x".into()),
        running: true,
        pending_approvals: 2,
        last_status: Some(ProcessStatus::Killed),
        last_stop_reason: Some(StopReason::UserStop),
        worktree_state: Some(WorktreeState::Missing),
    });
    round_trip(&attempt());
    round_trip(&process());
    round_trip(&TaskDetail {
        task: task(),
        attempt: Some(attempt()),
        processes: vec![process()],
        closed_attempts: vec![AttemptView {
            state: AttemptState::Merged,
            merge_commit: Some("c0ffee".into()),
            closed_at: Some(99),
            ..attempt()
        }],
    });
    round_trip(&BranchList {
        current: Some("main".into()),
        branches: vec!["main".into(), "dev".into()],
    });
    round_trip(&Settings::default());
    round_trip(&env_status());
}

#[test]
fn transcript_types_round_trip() {
    round_trip_all(&entries());
    round_trip(&EntryPage {
        entries: entries(),
        has_more: false,
    });
    round_trip(&ToolOutput {
        text: "x".into(),
        truncated_bytes: 10,
        is_error: true,
    });
}

#[test]
fn review_types_round_trip() {
    round_trip(&file_diff());
    round_trip(&DiffResult {
        base: "abc".into(),
        snapshot_tree: "def".into(),
        files: vec![file_diff()],
        additions: 2,
        deletions: 1,
        truncated: true,
    });
    round_trip(&BranchStatus {
        target_branch: "main".into(),
        ahead: 2,
        behind: 1,
        dirty: true,
        head_ok: false,
        conflicts: vec!["a.txt".into()],
        target_checked_out_at: Some("/Users/me/demo".into()),
        merge_blocked: Some("HEAD non corretto".into()),
    });
}

#[test]
fn api_types_round_trip() {
    round_trip(&AppError::not_implemented("get_env"));
    round_trip(&Changed {
        project_id: Some(id(2)),
        task_id: None,
    });
    round_trip(&Empty {});
    round_trip(&IdReq { id: id(1) });
    round_trip(&ProjectIdReq { project_id: id(2) });
    round_trip(&AttemptIdReq { attempt_id: id(3) });
    round_trip(&GetEnvReq { force: true });
    round_trip(&OpenLoginTerminalReq {
        method: LoginMethod::Sso,
    });
    round_trip(&AddProjectReq {
        path: "/tmp/r".into(),
    });
    round_trip(&AddProjectRes {
        project: project(),
        warnings: vec!["w".into()],
    });
    round_trip(&UpdateProjectReq {
        id: id(2),
        name: "n".into(),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::BypassPermissions,
        default_model: None,
    });
    round_trip(&SetProjectSecurityReq {
        id: id(2),
        config_policy: ConfigPolicy::Trusted,
        allow_bypass: true,
    });
    round_trip(&CreateTaskReq {
        project_id: id(2),
        title: "t".into(),
        description: String::new(),
        status: Some(TaskStatus::Cancelled),
    });
    round_trip(&UpdateTaskReq {
        id: id(1),
        title: "t".into(),
        description: "d".into(),
    });
    round_trip(&MoveTaskReq {
        id: id(1),
        status: TaskStatus::Done,
        before_id: Some(id(5)),
    });
    round_trip(&StartAttemptReq {
        task_id: id(1),
        target_branch: "main".into(),
        permission_mode: PermissionMode::AcceptEdits,
        model: Some("sonnet".into()),
        effort: Some(Effort::Low),
    });
    round_trip(&SendFollowUpReq {
        attempt_id: id(3),
        prompt: "continua".into(),
        permission_mode: None,
        fresh_session: true,
    });
    round_trip(&RespondApprovalReq {
        attempt_id: id(3),
        approval_id: id(9),
        decision: ApprovalDecision::Allow { remember: false },
    });
    round_trip(&UnsubscribeTranscriptReq {
        subscription_id: id(7),
    });
    round_trip(&GetEntriesReq {
        attempt_id: id(3),
        before_idx: 100,
        limit: 100,
    });
    round_trip(&MergeAttemptReq {
        attempt_id: id(3),
        message: "titolo\n\nATM-Attempt: x".into(),
    });
    round_trip(&OpenAttemptReq {
        attempt_id: id(3),
        target: OpenTarget::Editor,
    });
    round_trip(&OpenUrlReq {
        url: "https://docs.anthropic.com".into(),
    });
    round_trip(&PingReq { fail: true });
    round_trip(&ProbeMsg {
        i: 7,
        data: "a".repeat(10),
    });
    round_trip(&ReportReq {
        report: serde_json::json!({"ping_ok": true, "csp_violations": 0}),
    });
}

#[test]
fn wire_shapes() {
    // `{req: {}}` for argument-less commands, `null` for `()` replies.
    assert_eq!(serde_json::to_string(&Empty {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&()).unwrap(), "null");
    assert_eq!(
        serde_json::to_value(AppError::not_implemented("get_env")).unwrap(),
        serde_json::json!({"code": "NotImplemented", "message": "get_env: not implemented yet"})
    );
    assert_eq!(
        AppError::invalid("x").to_string(),
        "Invalid: x",
        "Display is `<Code>: <message>`"
    );
}

#[test]
fn command_names_are_unique_snake_case() {
    let mut names = COMMAND_NAMES.to_vec();
    assert_eq!(names.len(), 32);
    assert!(
        names
            .iter()
            .all(|n| n.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    );
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), COMMAND_NAMES.len());
}

#[test]
fn fixed_texts() {
    assert_eq!(
        CONTINUE_PROMPT,
        "The previous run was interrupted when the app closed. Continue the task."
    );
    assert_eq!(
        merge_message(" Titolo ", "riga 1\nriga 2\n", "a1"),
        "Titolo\n\nriga 1\nriga 2\n\nATM-Attempt: a1"
    );
    assert_eq!(merge_message("T", "  ", "a1"), "T\n\nATM-Attempt: a1");
    let long = merge_message("T", &"è".repeat(3000), "a1");
    assert_eq!(long.matches('è').count(), MERGE_MESSAGE_MAX_DESCRIPTION);
}

/// `TurnEnd.stopped` (M6) is optional on the wire: omitted when `None`, and a payload saved
/// before M6 (no key) still reads back.
#[test]
fn turn_end_stopped_is_optional() {
    let old = serde_json::json!({"type": "TurnEnd", "subtype": "success", "is_error": false,
        "duration_ms": null, "num_turns": null, "cost_usd_estimate": null,
        "permission_denials": 0, "text": null, "limit": null});
    let body: EntryBody = serde_json::from_value(old.clone()).unwrap();
    assert!(matches!(body, EntryBody::TurnEnd { stopped: None, .. }));
    assert_eq!(serde_json::to_value(&body).unwrap(), old);
    let stopped = EntryBody::TurnEnd {
        subtype: "error_during_execution".into(),
        is_error: true,
        duration_ms: None,
        num_turns: None,
        cost_usd_estimate: None,
        permission_denials: 0,
        text: None,
        limit: None,
        stopped: Some(StopReason::AppShutdown),
    };
    assert_eq!(
        serde_json::to_value(&stopped).unwrap()["stopped"],
        "app_shutdown"
    );
    round_trip(&stopped);
}
