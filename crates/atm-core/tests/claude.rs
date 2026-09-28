//! M2-CLAUDE: argv, environment, discovery and version gate, `auth status`, login script,
//! wire protocol and approvals, and whole turns against `fake-claude` (spec §7.1–§7.5, §7.8,
//! §7.10, §12.1). Never runs the real CLI.

mod common;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use atm_core::claude::{self, ChildEnv, TurnArgs};
use atm_core::wire::{self, CanUseTool, Inbound, Line, Pending};
use atm_types::{ApprovalDecision, AuthState, Effort, ErrorCode, LoginMethod, PermissionMode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, BufReader};
use tokio::sync::mpsc;

const SESSION: &str = "3f1c2b9e-0d4a-4c1e-9a55-1b2c3d4e5f60";
const WORKTREE: &str = "/Users/me/.ai-task-manager/worktrees/demo/atm-1-hello";

fn os_vars(vars: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    vars.iter().map(|(k, v)| (k.into(), v.into())).collect()
}

fn test_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())
}

/// The test process environment plus `extra` (later entries win), as the app would pass it.
fn base_env(extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    let mut base: Vec<_> = std::env::vars_os().collect();
    base.extend(os_vars(extra));
    base
}

// ---- argv (spec §7.3) ---------------------------------------------------------------------

fn turn_args() -> TurnArgs {
    TurnArgs {
        claude: PathBuf::from("/Users/me/.local/bin/claude"),
        permission_mode: PermissionMode::AcceptEdits,
        allow_bypass: false,
        session_id: SESSION.into(),
        resume: false,
        isolated: true,
        allow_rules: Vec::new(),
        model: None,
        effort: None,
        append_prompt: claude::append_prompt(Path::new(WORKTREE), "atm/1-hello", "main"),
    }
}

/// `build_argv` checked against the spec's invariants, with the deny list folded into a
/// placeholder (only `DENY_RULES` in claude.rs may spell those rules out, spec §10.3).
fn argv_snapshot(args: &TurnArgs) -> Vec<String> {
    let argv = claude::build_argv(args);
    assert_eq!(argv[0], args.claude.to_string_lossy());
    let modes: Vec<_> = argv
        .iter()
        .filter(|a| a.starts_with("--permission-mode="))
        .collect();
    assert_eq!(
        modes,
        [&format!("--permission-mode={}", args.permission_mode)]
    );
    for forbidden in [
        "--bare",
        "--dangerously-skip-permissions",
        "--system-prompt",
        "--no-session-persistence",
        "--replay-user-messages",
        "--allowedTools",
    ] {
        assert!(
            !argv.iter().any(|a| a.split('=').next() == Some(forbidden)),
            "{forbidden} in {argv:?}"
        );
    }
    let settings = format!("--settings={}", claude::settings_json(&args.allow_rules));
    assert!(argv.contains(&settings));
    let deny = serde_json::to_string(claude::DENY_RULES).unwrap();
    argv.into_iter()
        .map(|a| a.replace(&deny, "\"<DENY_RULES>\""))
        .collect()
}

#[test]
fn argv_first_turn() {
    insta::assert_json_snapshot!(argv_snapshot(&turn_args()));
}

#[test]
fn argv_resume() {
    let args = TurnArgs {
        resume: true,
        allow_rules: vec!["Bash(npm test)".into(), "Bash(cargo check:*)".into()],
        ..turn_args()
    };
    insta::assert_json_snapshot!(argv_snapshot(&args));
}

#[test]
fn argv_trusted() {
    let args = TurnArgs {
        isolated: false,
        permission_mode: PermissionMode::Default,
        ..turn_args()
    };
    let argv = argv_snapshot(&args);
    assert!(!argv.iter().any(|a| a.starts_with("--setting-sources")));
    assert!(!argv.contains(&"--strict-mcp-config".to_owned()));
    insta::assert_json_snapshot!(argv);
}

#[test]
fn argv_bypass_enabled() {
    let args = TurnArgs {
        permission_mode: PermissionMode::BypassPermissions,
        allow_bypass: true,
        ..turn_args()
    };
    insta::assert_json_snapshot!(argv_snapshot(&args));
}

#[test]
fn argv_bypass_without_opt_in_runs_as_default() {
    let args = TurnArgs {
        permission_mode: PermissionMode::BypassPermissions,
        allow_bypass: false,
        ..turn_args()
    };
    let argv = claude::build_argv(&args);
    let modes: Vec<_> = argv
        .iter()
        .filter(|a| a.starts_with("--permission-mode="))
        .collect();
    assert_eq!(modes, ["--permission-mode=default"]);
    assert!(!argv.iter().any(|a| a.contains("dangerously")), "{argv:?}");
}

#[test]
fn argv_model_and_effort() {
    let args = TurnArgs {
        model: Some("opus".into()),
        effort: Some(Effort::XHigh),
        ..turn_args()
    };
    insta::assert_json_snapshot!(argv_snapshot(&args));
}

#[test]
fn settings_json_has_deny_rules_then_allow_rules() {
    let rules = vec!["Bash(npm test)".to_owned()];
    let text = claude::settings_json(&rules);
    assert!(text.starts_with(r#"{"permissions":{"deny":["#), "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["permissions"]["deny"], json!(claude::DENY_RULES));
    assert_eq!(v["permissions"]["allow"], json!(rules));
    assert!(claude::settings_json(&[]).ends_with(r#""allow":[]}}"#));
}

#[test]
fn append_prompt_names_worktree_branch_and_target() {
    let text = claude::append_prompt(Path::new("/tmp/a b/wt"), "atm/7-fix", "develop");
    assert!(
        text.contains("git worktree `/tmp/a b/wt` on branch `atm/7-fix` (created from `develop`)")
    );
    assert!(text.contains("Do not push"));
    assert!(text.ends_with("read it first and follow its conventions."));
}

// ---- child environment (spec §7.2) ----------------------------------------------------------

#[test]
fn child_env_scrubs_and_sets() {
    let base = os_vars(&[
        ("ANTHROPIC_API_KEY", "sk-test"),
        ("ANTHROPIC_AUTH_TOKEN", "token"),
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("GIT_DIR", "/elsewhere/.git"),
        ("GIT_WORK_TREE", "/elsewhere"),
        ("GIT_INDEX_FILE", "/elsewhere/index"),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.hooksPath"),
        ("GIT_CONFIG_VALUE_0", "/evil"),
        ("GIT_AUTHOR_NAME", "Kept"),
        ("HOME", "/Users/me"),
        ("PATH", "/old/path"),
    ]);
    let env = ChildEnv::new(base.clone(), "/new/bin:/usr/bin".as_ref(), false);
    for gone in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        assert_eq!(env.get(gone), None, "{gone}");
    }
    let get = |env: &ChildEnv, k| env.get(k).map(|v| v.to_str().unwrap().to_owned());
    assert_eq!(get(&env, "PATH").as_deref(), Some("/new/bin:/usr/bin"));
    assert_eq!(get(&env, "LANG").as_deref(), Some("en_US.UTF-8"));
    assert_eq!(get(&env, "GIT_EDITOR").as_deref(), Some("true"));
    assert_eq!(get(&env, "GIT_SEQUENCE_EDITOR").as_deref(), Some("true"));
    assert_eq!(get(&env, "GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
    assert_eq!(get(&env, "GIT_AUTHOR_NAME").as_deref(), Some("Kept"));
    assert_eq!(get(&env, "HOME").as_deref(), Some("/Users/me"));
    assert_eq!(get(&env, "PWD"), None);

    let turn = env.for_attempt(Path::new("/wt/a b"), "attempt-1");
    assert_eq!(get(&turn, "PWD").as_deref(), Some("/wt/a b"));
    assert_eq!(get(&turn, "ATM_ATTEMPT_ID").as_deref(), Some("attempt-1"));

    let passthrough = ChildEnv::new(base, "/bin".as_ref(), true);
    assert_eq!(
        get(&passthrough, "ANTHROPIC_API_KEY").as_deref(),
        Some("sk-test")
    );
    assert_eq!(
        get(&passthrough, "ANTHROPIC_AUTH_TOKEN").as_deref(),
        Some("token")
    );
    assert_eq!(passthrough.get("CLAUDECODE"), None);

    let italian = ChildEnv::new(os_vars(&[("LANG", "it_IT.UTF-8")]), "/bin".as_ref(), false);
    assert_eq!(get(&italian, "LANG").as_deref(), Some("it_IT.UTF-8"));

    // Debug names the variables but never prints a value (spec §10.2).
    let debug = format!("{passthrough:?}");
    assert!(debug.contains("ANTHROPIC_API_KEY"), "{debug}");
    assert!(
        !debug.contains("sk-test") && !debug.contains("/Users/me"),
        "{debug}"
    );
}

#[test]
fn api_key_and_cloud_provider_detection() {
    assert!(claude::api_key_in_env(&os_vars(&[(
        "ANTHROPIC_API_KEY",
        "sk"
    )])));
    assert!(claude::api_key_in_env(&os_vars(&[(
        "ANTHROPIC_AUTH_TOKEN",
        "t"
    )])));
    assert!(!claude::api_key_in_env(&os_vars(&[(
        "ANTHROPIC_API_KEY",
        ""
    )])));
    assert!(!claude::api_key_in_env(&os_vars(&[("HOME", "/x")])));

    assert!(claude::cloud_provider_env(&os_vars(&[(
        "CLAUDE_CODE_USE_BEDROCK",
        "1"
    )])));
    assert!(claude::cloud_provider_env(&os_vars(&[(
        "CLAUDE_CODE_USE_VERTEX",
        "true"
    )])));
    assert!(!claude::cloud_provider_env(&os_vars(&[(
        "CLAUDE_CODE_USE_BEDROCK",
        "0"
    )])));
    assert!(!claude::cloud_provider_env(&os_vars(&[(
        "CLAUDE_CODE_USE_VERTEX",
        "false"
    )])));
    assert!(!claude::cloud_provider_env(&os_vars(&[("PATH", "/bin")])));
}

// ---- login-shell PATH (spec §7.2) -----------------------------------------------------------

#[test]
fn marked_path_is_extracted_from_noise() {
    let cases = [
        ("__ATM__/a:/b__ATM__", Some("/a:/b")),
        (
            "Last login: Mon\n\x1b]0;title\x07welcome\n__ATM__/opt/x bin:/usr/bin__ATM__bye\n",
            Some("/opt/x bin:/usr/bin"),
        ),
        (
            "__ATM__/first__ATM__ and __ATM__/second__ATM__",
            Some("/first"),
        ),
        ("no markers at all", None),
        ("__ATM__/unterminated", None),
        ("__ATM____ATM__", None),
    ];
    for (output, expected) in cases {
        assert_eq!(
            claude::extract_marked_path(output).as_deref(),
            expected,
            "{output:?}"
        );
    }
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn login_shell_path_between_markers() {
    let dir = common::tempdir();
    // Stands in for `$SHELL`: rc noise on stdout, then runs the `-ilc` command with its PATH.
    let shell = write_script(
        dir.path(),
        "fake-shell",
        "#!/bin/sh\necho 'Last login: yesterday'\n[ \"$1\" = -ilc ] || exit 3\n\
         PATH='/fake bin:/usr/bin:/bin'\neval \"$2\"\necho 'rc: goodbye'\n",
    );
    let path = claude::shell_path(&shell, Duration::from_secs(5)).await;
    assert_eq!(path.as_deref(), Some("/fake bin:/usr/bin:/bin"));

    let slow = write_script(dir.path(), "slow-shell", "#!/bin/sh\nexec sleep 30\n");
    let started = std::time::Instant::now();
    assert_eq!(
        claude::shell_path(&slow, Duration::from_millis(300)).await,
        None
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    let silent = write_script(dir.path(), "silent-shell", "#!/bin/sh\necho no markers\n");
    assert_eq!(
        claude::shell_path(&silent, Duration::from_secs(5)).await,
        None
    );
}

// ---- discovery and version gate (spec §7.1) ---------------------------------------------------

#[test]
fn candidates_follow_the_spec_order() {
    let dir = common::tempdir();
    let root = dir.path().canonicalize().unwrap();
    let home = root.join("home");
    let tmp = root.join("tmp");
    let on_path = root.join("bin");
    let shims = root.join("cmux-cli-shims");
    for d in [&home, &tmp.join("x"), &on_path, &shims] {
        std::fs::create_dir_all(d).unwrap();
    }
    for d in [&tmp.join("x"), &on_path, &shims] {
        write_script(d, "claude", "#!/bin/sh\n");
    }
    let path =
        std::env::join_paths([tmp.join("x"), on_path.clone(), shims, root.join("none")]).unwrap();
    let env = ChildEnv::new(
        os_vars(&[
            ("HOME", home.to_str().unwrap()),
            ("TMPDIR", tmp.to_str().unwrap()),
            ("ATM_CLAUDE_PATH", "/dev/atm/claude"),
        ]),
        &path,
        false,
    );
    let found = claude::candidates(Some(Path::new("/override/claude")), &env);
    let mut expected = vec![
        PathBuf::from("/override/claude"),
        PathBuf::from("/dev/atm/claude"),
    ];
    expected.extend(claude::fixed_candidates(&home));
    expected.push(on_path.join("claude"));
    assert_eq!(found, expected);
    assert_eq!(
        claude::fixed_candidates(Path::new("/Users/me")),
        [
            "/Users/me/.local/bin/claude",
            "/Users/me/.claude/local/claude",
            "/opt/homebrew/bin/claude",
            "/usr/local/bin/claude",
        ]
        .map(PathBuf::from)
    );
}

#[test]
fn candidates_are_always_absolute() {
    let dir = common::tempdir();
    let bin = dir.path().canonicalize().unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_script(&bin, "claude", "#!/bin/sh\n");
    // The same directory spelled relative to the test's cwd: an existing file, still skipped.
    let cwd = std::env::current_dir().unwrap();
    let up = "../".repeat(cwd.components().count());
    let relative_bin = PathBuf::from(format!("{up}{}", bin.strip_prefix("/").unwrap().display()));
    assert!(relative_bin.join("claude").is_file());
    let path = std::env::join_paths([relative_bin, PathBuf::new(), bin.clone()]).unwrap();
    let env = ChildEnv::new(
        os_vars(&[("ATM_CLAUDE_PATH", "dev/fake-claude"), ("HOME", "home")]),
        &path,
        false,
    );
    assert_eq!(
        claude::candidates(Some(Path::new("rel/claude")), &env),
        [
            cwd.join("rel/claude"),
            cwd.join("dev/fake-claude"),
            bin.join("claude")
        ]
    );
}

#[tokio::test]
async fn discover_and_probe_fake_claude() {
    let dir = common::tempdir();
    let fake = common::fake_claude();
    let home = dir.path().to_str().unwrap();
    // HOME points at an empty dir, so no fixed candidate exists; the fake is always first.
    let env = ChildEnv::new(os_vars(&[("HOME", home)]), &test_path(), false);
    let found = claude::discover(Some(&fake), &env).await.unwrap();
    assert_eq!(found.path, fake);
    assert_eq!(found.version, atm_types::CLAUDE_TESTED_VERSION);

    // An override that is not Claude Code is skipped in favour of ATM_CLAUDE_PATH.
    let env = ChildEnv::new(
        os_vars(&[("HOME", home), ("ATM_CLAUDE_PATH", fake.to_str().unwrap())]),
        &test_path(),
        false,
    );
    let found = claude::discover(Some(Path::new("/bin/echo")), &env)
        .await
        .unwrap();
    assert_eq!(found.path, fake);

    let err = claude::probe_version(Path::new("/bin/echo"), &env)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ClaudeNotFound);
    let missing = dir.path().join("missing/claude");
    let err = claude::probe_version(&missing, &env).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ClaudeNotFound);
}

#[test]
fn version_gate() {
    assert_eq!(
        claude::parse_version("2.1.283 (Claude Code)\n").as_deref(),
        Some("2.1.283")
    );
    assert_eq!(
        claude::parse_version("note: update available\n2.2.0 (Claude Code)").as_deref(),
        Some("2.2.0")
    );
    assert_eq!(claude::parse_version("2.1.283"), None);
    assert_eq!(claude::parse_version("git version 2.54.0"), None);
    assert_eq!(claude::parse_version("(Claude Code)"), None);

    for (version, supported) in [
        ("2.1.222", false),
        ("2.0.999", false),
        ("1.9.0", false),
        ("2.1.223", true),
        ("2.1.283", true),
        ("2.1.300", true),
        ("2.10.0", true),
        ("3.0.0-beta.1", true),
        ("garbage", false),
        ("", false),
    ] {
        assert_eq!(claude::version_supported(version), supported, "{version}");
    }

    let old = claude::Discovered {
        path: "/usr/local/bin/claude".into(),
        version: "2.1.100".into(),
    };
    let info = claude::claude_info(Some(&old));
    assert_eq!(info.path.as_deref(), Some("/usr/local/bin/claude"));
    assert_eq!(info.version.as_deref(), Some("2.1.100"));
    assert!(!info.supported);
    assert_eq!(info.min_version, atm_types::CLAUDE_MIN_VERSION);
    assert_eq!(info.tested_version, atm_types::CLAUDE_TESTED_VERSION);
    let none = claude::claude_info(None);
    assert_eq!(
        (none.path, none.version, none.supported),
        (None, None, false)
    );
}

// ---- auth (spec §7.10) ------------------------------------------------------------------------

#[test]
fn auth_status_parse() {
    let logged_in = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
        "email":"me@example.com","orgId":"o1","orgName":"Org","subscriptionType":"max"}"#;
    assert_eq!(
        claude::parse_auth_status(Some(0), logged_in),
        AuthState::LoggedIn {
            auth_method: Some("claude.ai".into()),
            api_provider: Some("firstParty".into()),
            email: Some("me@example.com".into()),
            org_name: Some("Org".into()),
            subscription_type: Some("max".into()),
        }
    );
    assert_eq!(
        claude::parse_auth_status(Some(0), r#"{"loggedIn":true}"#),
        AuthState::LoggedIn {
            auth_method: None,
            api_provider: None,
            email: None,
            org_name: None,
            subscription_type: None,
        }
    );
    assert_eq!(
        claude::parse_auth_status(Some(0), r#"{"loggedIn":false}"#),
        AuthState::LoggedOut
    );
    assert_eq!(claude::parse_auth_status(Some(1), ""), AuthState::LoggedOut);
    assert_eq!(
        claude::parse_auth_status(Some(1), logged_in),
        AuthState::LoggedOut
    );
    for (code, stdout) in [
        (Some(2), logged_in),
        (None, logged_in),
        (Some(0), "Logged in as me@example.com"),
        (Some(0), r#"{"authMethod":"claude.ai"}"#),
    ] {
        match claude::parse_auth_status(code, stdout) {
            AuthState::Unknown { reason } => assert!(!reason.contains("example.com"), "{reason}"),
            other => panic!("{code:?} {stdout:?} → {other:?}"),
        }
    }
}

#[tokio::test]
async fn auth_status_against_fake_claude() {
    let fake = common::fake_claude();
    let env = |auth| ChildEnv::new(base_env(&[("FAKE_CLAUDE_AUTH", auth)]), &test_path(), false);
    assert!(matches!(
        claude::auth_status(&fake, &env("in")).await,
        AuthState::LoggedIn { subscription_type: Some(s), .. } if s == "max"
    ));
    assert_eq!(
        claude::auth_status(&fake, &env("out")).await,
        AuthState::LoggedOut
    );
    assert!(matches!(
        claude::auth_status(&fake, &env("broken")).await,
        AuthState::Unknown { .. }
    ));
    let missing = Path::new("/nonexistent/claude");
    assert!(matches!(
        claude::auth_status(missing, &env("in")).await,
        AuthState::Unknown { .. }
    ));
}

#[test]
fn login_script_content() {
    let claude = Path::new("/Users/me/.local/bin/claude");
    let tail = "echo\necho \"Accesso completato? Puoi chiudere questa finestra e tornare ad AI \
                Task Manager.\"\n";
    assert_eq!(
        claude::login_script(claude, LoginMethod::ClaudeAi),
        format!("#!/bin/sh\n'/Users/me/.local/bin/claude' auth login\n{tail}")
    );
    assert_eq!(
        claude::login_script(claude, LoginMethod::Console),
        format!("#!/bin/sh\n'/Users/me/.local/bin/claude' auth login --console\n{tail}")
    );
    assert_eq!(
        claude::login_script(claude, LoginMethod::Sso),
        format!("#!/bin/sh\n'/Users/me/.local/bin/claude' auth login --sso\n{tail}")
    );
    assert!(
        claude::login_script(
            Path::new("/Users/o'brien/$(rm -rf ~)/claude"),
            LoginMethod::Sso
        )
        .contains(r"'/Users/o'\''brien/$(rm -rf ~)/claude' auth login --sso")
    );
}

#[test]
fn login_script_file_runs_the_quoted_path() {
    let dir = common::tempdir();
    let odd = dir.path().join("it's $(here)");
    std::fs::create_dir_all(&odd).unwrap();
    let claude = odd.join("claude");
    std::os::unix::fs::symlink(common::fake_claude(), &claude).unwrap();
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    // A stale file (e.g. with other permissions) is replaced.
    std::fs::write(cache.join(claude::LOGIN_SCRIPT_NAME), "stale").unwrap();

    let script = claude::write_login_script(&cache, &claude, LoginMethod::Console).unwrap();
    assert_eq!(script, cache.join("claude-login.command"));
    let meta = std::fs::metadata(&script).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        claude::login_script(&claude, LoginMethod::Console)
    );
    let out = std::process::Command::new(&script).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("login simulato"));
}

// ---- wire protocol (spec §7.4, §7.5) --------------------------------------------------------

#[test]
fn parse_classifies_every_route() {
    let parse = |v: Value| wire::parse(v.to_string().as_bytes());
    assert_eq!(
        parse(
            json!({"type":"control_response","response":{"subtype":"success",
            "request_id":"atm_1_abcd0123","response":{"commands":[]}}})
        ),
        Inbound::ControlResponse {
            request_id: "atm_1_abcd0123".into(),
            result: Ok(json!({"commands":[]})),
        }
    );
    assert_eq!(
        parse(
            json!({"type":"control_response","response":{"subtype":"error",
            "request_id":"atm_2_abcd0123","error":"boom"}})
        ),
        Inbound::ControlResponse {
            request_id: "atm_2_abcd0123".into(),
            result: Err("boom".into()),
        }
    );
    let request = json!({"subtype":"can_use_tool","tool_name":"Bash",
        "input":{"command":"ls"},"tool_use_id":"toolu_1","decision_reason":"asks"});
    assert_eq!(
        parse(json!({"type":"control_request","request_id":"r1","request":request})),
        Inbound::CanUseTool(CanUseTool {
            request_id: "r1".into(),
            tool_name: "Bash".into(),
            input: json!({"command":"ls"}),
            tool_use_id: "toolu_1".into(),
            permission_suggestions: Value::Null,
            request: request.clone(),
        })
    );
    assert_eq!(
        parse(json!({"type":"control_request","request_id":"r2",
            "request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}})),
        Inbound::ControlRequest {
            request_id: "r2".into(),
            subtype: "can_use_tool".into(),
        }
    );
    assert_eq!(
        parse(json!({"type":"control_request","request_id":"r3",
            "request":{"subtype":"hook_callback","callback_id":"h"}})),
        Inbound::ControlRequest {
            request_id: "r3".into(),
            subtype: "hook_callback".into(),
        }
    );
    assert_eq!(
        parse(json!({"type":"control_cancel_request","request_id":"r1"})),
        Inbound::ControlCancel {
            request_id: "r1".into()
        }
    );
    assert_eq!(parse(json!({"type":"keep_alive"})), Inbound::KeepAlive);
    let event = json!({"type":"stream_event","event":{"type":"message_stop"}});
    assert_eq!(parse(event.clone()), Inbound::StreamEvent(event));
    let assistant = json!({"type":"assistant","message":{"content":[]}});
    assert_eq!(parse(assistant.clone()), Inbound::Message(assistant));
    let no_id = json!({"type":"control_request","request":{"subtype":"interrupt"}});
    assert_eq!(parse(no_id.clone()), Inbound::Message(no_id));
    assert_eq!(wire::parse(b"Warning: something"), Inbound::NotJson);
    assert_eq!(wire::parse(b"{not json"), Inbound::NotJson);
    assert_eq!(wire::parse(b""), Inbound::NotJson);
    assert_eq!(wire::parse(b"[1,2]"), Inbound::NotJson);
}

#[test]
fn outbound_frames() {
    let id = wire::request_id(7);
    let hex = id.strip_prefix("atm_7_").unwrap();
    assert_eq!(hex.len(), 8);
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_ne!(wire::request_id(7), wire::request_id(7));

    assert_eq!(
        wire::initialize_request("atm_1_00000001"),
        json!({"type":"control_request","request_id":"atm_1_00000001",
               "request":{"subtype":"initialize","hooks":null}})
    );
    assert_eq!(
        wire::interrupt_request("atm_2_00000001"),
        json!({"type":"control_request","request_id":"atm_2_00000001",
               "request":{"subtype":"interrupt"}})
    );
    let user = wire::user_message("# Titolo\n\nDescrizione");
    assert_eq!(
        user,
        json!({"type":"user","message":{"role":"user","content":"# Titolo\n\nDescrizione"},
               "parent_tool_use_id":null})
    );
    assert!(user.get("session_id").is_none());
    assert_eq!(
        wire::control_error("r3", "Unsupported control request subtype: hook_callback"),
        json!({"type":"control_response","response":{"subtype":"error","request_id":"r3",
               "error":"Unsupported control request subtype: hook_callback"}})
    );
    assert_eq!(
        wire::control_success("r4", json!({"ok":true})),
        json!({"type":"control_response","response":{"subtype":"success","request_id":"r4",
               "response":{"ok":true}}})
    );
}

#[tokio::test]
async fn read_line_capped_skips_a_20_mib_line_and_continues() {
    let mut data = b"{\"a\":1}\n".to_vec();
    data.extend(std::iter::repeat_n(b'x', 20 << 20));
    data.extend(b"\n{\"b\":2}\n\nlast");
    let mut reader = BufReader::new(&data[..]);
    let mut buf = Vec::new();
    let mut next = async || {
        let line = wire::read_line_capped(&mut reader, &mut buf, wire::MAX_STDOUT_LINE)
            .await
            .unwrap();
        (line, String::from_utf8(buf.clone()).unwrap())
    };
    assert_eq!(next().await, (Line::Complete, r#"{"a":1}"#.into()));
    assert_eq!(next().await, (Line::TooLong(20 << 20), String::new()));
    assert_eq!(next().await, (Line::Complete, r#"{"b":2}"#.into()));
    assert_eq!(next().await, (Line::Complete, String::new()));
    assert_eq!(next().await, (Line::Complete, "last".into()));
    assert_eq!(next().await, (Line::Eof, String::new()));
}

#[tokio::test]
async fn read_line_capped_boundaries_across_small_chunks() {
    let data = b"12345\n123456\n1234567\nab";
    let mut reader = BufReader::with_capacity(4, &data[..]);
    let mut buf = Vec::new();
    let mut lines = Vec::new();
    loop {
        match wire::read_line_capped(&mut reader, &mut buf, 6)
            .await
            .unwrap()
        {
            Line::Eof => break,
            line => lines.push((line, String::from_utf8(buf.clone()).unwrap())),
        }
    }
    assert_eq!(
        lines,
        [
            (Line::Complete, "12345".to_owned()),
            (Line::Complete, "123456".to_owned()),
            (Line::TooLong(7), String::new()),
            (Line::Complete, "ab".to_owned()),
        ]
    );
}

#[tokio::test]
async fn write_frames_writes_lines_and_log_until_closed() {
    let dir = common::tempdir();
    let log_path = dir.path().join("stdin.jsonl");
    let log = tokio::fs::File::create(&log_path).await.unwrap();
    let (writer, mut reader) = tokio::io::duplex(1 << 16);
    let (tx, rx) = mpsc::channel(wire::STDIN_CHANNEL);
    let task = tokio::spawn(wire::write_frames(writer, rx, Some(log)));
    tx.send(wire::initialize_request("atm_1_0000000a"))
        .await
        .unwrap();
    tx.send(wire::user_message("ciao")).await.unwrap();
    drop(tx);
    task.await.unwrap().unwrap();
    let mut written = String::new();
    reader.read_to_string(&mut written).await.unwrap();
    let expected = format!(
        "{}\n{}\n",
        wire::initialize_request("atm_1_0000000a"),
        wire::user_message("ciao")
    );
    assert_eq!(written, expected);
    assert_eq!(std::fs::read_to_string(&log_path).unwrap(), expected);
}

// ---- approvals (spec §7.8) ----------------------------------------------------------------

fn pending(tool: &str, suggestions: Value) -> Pending {
    Pending {
        approval_id: "approval-1".into(),
        request_id: "cli-req-9".into(),
        tool_use_id: "toolu_9".into(),
        tool_name: tool.into(),
        input: json!({"command":"npm test","description":"Run tests"}),
        suggestions,
    }
}

fn add_rules(rules: Value, behavior: &str) -> Value {
    json!({"type":"addRules","rules":rules,"behavior":behavior,"destination":"localSettings"})
}

#[test]
fn can_remember_only_specific_allow_rules() {
    let npm = json!([{"toolName":"Bash","ruleContent":"npm test"}]);
    assert!(wire::can_remember(&json!([add_rules(
        npm.clone(),
        "allow"
    )])));
    assert!(wire::can_remember(&json!([
        add_rules(npm.clone(), "allow"),
        add_rules(
            json!([{"toolName":"Bash","ruleContent":"npm run lint"}]),
            "allow"
        ),
    ])));
    for not_rememberable in [
        Value::Null,
        json!([]),
        json!([add_rules(npm.clone(), "deny")]),
        json!([add_rules(json!([{"toolName":"Bash"}]), "allow")]),
        json!([add_rules(
            json!([{"toolName":"Bash","ruleContent":""}]),
            "allow"
        )]),
        json!([add_rules(json!([]), "allow")]),
        json!([add_rules(npm.clone(), "allow"), {"type":"setMode","mode":"acceptEdits"}]),
        json!([add_rules(npm.clone(), "allow"),
               {"type":"addDirectories","directories":["/tmp"],"destination":"session"}]),
    ] {
        assert!(!wire::can_remember(&not_rememberable), "{not_rememberable}");
    }
}

#[test]
fn approval_response_allow_always_echoes_the_input() {
    let p = pending(
        "Bash",
        json!([add_rules(
            json!([{"toolName":"Bash","ruleContent":"npm test"}]),
            "allow"
        )]),
    );
    assert_eq!(
        wire::approval_response(&p, &ApprovalDecision::Allow { remember: false }),
        json!({"type":"control_response","response":{"subtype":"success",
            "request_id":"cli-req-9",
            "response":{"behavior":"allow","updatedInput":{"command":"npm test","description":"Run tests"}}}})
    );
}

#[test]
fn approval_response_remember_rewrites_destination_to_session() {
    let p = pending(
        "Bash",
        json!([
            add_rules(json!([{"toolName":"Bash","ruleContent":"npm test"}]), "allow"),
            {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm run *"}],
             "behavior":"allow","destination":"projectSettings"},
        ]),
    );
    let frame = wire::approval_response(&p, &ApprovalDecision::Allow { remember: true });
    assert_eq!(
        frame["response"]["response"],
        json!({"behavior":"allow","updatedInput":p.input,
            "updatedPermissions":[
                {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm test"}],
                 "behavior":"allow","destination":"session"},
                {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm run *"}],
                 "behavior":"allow","destination":"session"}]})
    );
    assert_eq!(frame["response"]["request_id"], "cli-req-9");
    assert_eq!(
        wire::remembered_rules(&p),
        ["Bash(npm test)", "Bash(npm run *)"]
    );
}

#[test]
fn approval_response_never_remembers_whole_tool_rules() {
    let p = pending(
        "Bash",
        json!([add_rules(
            json!([{"toolName":"Bash","ruleContent":"npm test"},{"toolName":"Bash"}]),
            "allow"
        )]),
    );
    assert!(!wire::can_remember(&p.suggestions));
    let frame = wire::approval_response(&p, &ApprovalDecision::Allow { remember: true });
    assert_eq!(
        frame["response"]["response"],
        json!({"behavior":"allow","updatedInput":p.input})
    );
    assert!(wire::remembered_rules(&p).is_empty());
}

#[test]
fn approval_response_deny_and_ask_user_question() {
    let p = pending("Bash", Value::Null);
    let frame = wire::approval_response(
        &p,
        &ApprovalDecision::Deny {
            message: "usa cargo, non npm".into(),
            interrupt: true,
        },
    );
    assert_eq!(
        frame["response"]["response"],
        json!({"behavior":"deny","interrupt":true,
               "message":format!("{}usa cargo, non npm", wire::DENY_PREFIX)})
    );
    assert!(wire::DENY_PREFIX.ends_with("the user said: "));

    let ask = pending(
        "AskUserQuestion",
        json!([add_rules(
            json!([{"toolName":"AskUserQuestion","ruleContent":"x"}]),
            "allow"
        )]),
    );
    assert!(wire::remembered_rules(&ask).is_empty());
    for decision in [
        ApprovalDecision::Allow { remember: true },
        ApprovalDecision::Deny {
            message: "x".into(),
            interrupt: true,
        },
    ] {
        assert_eq!(
            wire::approval_response(&ask, &decision)["response"]["response"],
            json!({"behavior":"deny","message":wire::ASK_USER_QUESTION_DENY,"interrupt":false})
        );
    }
}

// ---- whole turns against fake-claude (spec §7.3, §7.4, §12.1) -------------------------------

struct Turn {
    lines: Vec<Inbound>,
    too_long: Vec<usize>,
    status: ExitStatus,
    stderr: String,
}

impl Turn {
    fn result(&self) -> &Value {
        self.lines
            .iter()
            .find_map(|l| match l {
                Inbound::Message(v) if v["type"] == "result" => Some(v),
                _ => None,
            })
            .expect("a result line")
    }

    fn texts(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Inbound::Message(v) if v["type"] == "assistant" => {
                    v["message"]["content"][0]["text"].as_str()
                }
                _ => None,
            })
            .collect()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    worktree: PathBuf,
    record: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = common::tempdir();
        let worktree = dir.path().join("wt dir").join("atm-1-hello");
        std::fs::create_dir_all(&worktree).unwrap();
        let worktree = worktree.canonicalize().unwrap();
        let record = dir.path().join("record.jsonl");
        Fixture {
            _dir: dir,
            worktree,
            record,
        }
    }

    /// The app's environment with the variables the child must never see, as a turn gets it.
    fn env(&self, extra: &[(&str, &str)]) -> ChildEnv {
        let mut vars = vec![
            ("ANTHROPIC_API_KEY", "sk-must-not-leak"),
            ("ANTHROPIC_AUTH_TOKEN", "token-must-not-leak"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("GIT_DIR", "/nowhere/.git"),
            ("PWD", "/somewhere/else"),
            ("FAKE_CLAUDE_RECORD", self.record.to_str().unwrap()),
        ];
        vars.extend_from_slice(extra);
        ChildEnv::new(base_env(&vars), &test_path(), false).for_attempt(&self.worktree, "attempt-1")
    }

    fn argv(&self) -> Vec<String> {
        claude::build_argv(&TurnArgs {
            claude: common::fake_claude(),
            append_prompt: claude::append_prompt(&self.worktree, "atm/1-hello", "main"),
            ..turn_args()
        })
    }

    fn records(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.record)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn spawn(&self, extra: &[(&str, &str)]) -> claude::Spawned {
        claude::spawn(&self.argv(), &self.worktree, &self.env(extra)).unwrap()
    }
}

/// One turn as the runner drives it: initialize, then the prompt, stdin closed after
/// `result`, stdout read to EOF. `on_request` answers the fake's control requests.
async fn drive(
    spawned: claude::Spawned,
    prompt: &str,
    mut on_request: impl FnMut(&Inbound) -> Option<Value>,
) -> Turn {
    let claude::Spawned {
        mut child,
        stdin,
        stdout,
        mut stderr,
        ..
    } = spawned;
    let stderr = tokio::spawn(async move {
        let mut text = String::new();
        stderr.read_to_string(&mut text).await.map(|_| text)
    });
    let (tx, rx) = mpsc::channel(wire::STDIN_CHANNEL);
    let writer = tokio::spawn(wire::write_frames(stdin, rx, None));
    let init_id = wire::request_id(1);
    tx.send(wire::initialize_request(&init_id)).await.unwrap();
    let mut tx = Some(tx);
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    let (mut lines, mut too_long) = (Vec::new(), Vec::new());
    let read = async {
        loop {
            match wire::read_line_capped(&mut reader, &mut buf, wire::MAX_STDOUT_LINE).await {
                Ok(Line::Eof) | Err(_) => break,
                Ok(Line::TooLong(n)) => too_long.push(n),
                Ok(Line::Complete) => {
                    let inbound = wire::parse(&buf);
                    let reply = match &inbound {
                        Inbound::ControlResponse { request_id, .. } if *request_id == init_id => {
                            Some(wire::user_message(prompt))
                        }
                        Inbound::Message(v) if v["type"] == "result" => {
                            tx = None; // close stdin (spec §7.4 step 3)
                            None
                        }
                        other => on_request(other),
                    };
                    if let (Some(reply), Some(tx)) = (reply, &tx) {
                        tx.send(reply).await.unwrap();
                    }
                    lines.push(inbound);
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), read)
        .await
        .expect("turn finished in time");
    drop(tx);
    writer.await.unwrap().ok();
    let status = child.wait().await.unwrap();
    let stderr = stderr.await.unwrap().unwrap();
    Turn {
        lines,
        too_long,
        status,
        stderr,
    }
}

#[tokio::test]
async fn spawn_scrubs_the_environment_and_sets_pwd() {
    let fx = Fixture::new();
    let turn = drive(fx.spawn(&[]), "# Crea hello\n\nScrivi hello.txt", |_| None).await;
    assert!(turn.status.success(), "{}", turn.stderr);
    assert_eq!(turn.result()["subtype"], "success");
    assert_eq!(
        std::fs::read_to_string(fx.worktree.join("hello.txt")).unwrap(),
        "hello\n"
    );

    let records = fx.records();
    assert_eq!(records.len(), 1);
    let call = &records[0];
    let wt = fx.worktree.to_str().unwrap();
    assert_eq!(call["kind"], "call");
    assert_eq!(call["pwd"], wt);
    assert_eq!(call["cwd"], wt);
    assert_eq!(call["argv"], json!(fx.argv()[1..]));
    assert_eq!(
        call["env"],
        json!({"ANTHROPIC_API_KEY":false,"ANTHROPIC_AUTH_TOKEN":false,"CLAUDECODE":false,
               "CLAUDE_CODE_ENTRYPOINT":false,"GIT_DIR":false})
    );
    // The fake echoes the session and permission mode it was given.
    let init = turn
        .lines
        .iter()
        .find_map(|l| match l {
            Inbound::Message(v) if v["subtype"] == "init" => Some(v),
            _ => None,
        })
        .unwrap();
    assert_eq!(init["session_id"], SESSION);
    assert_eq!(init["permissionMode"], "acceptEdits");
    assert_eq!(init["cwd"], wt);
}

#[tokio::test]
async fn big_scenario_skips_the_20_mib_line_and_the_stream_continues() {
    let fx = Fixture::new();
    let turn = drive(fx.spawn(&[]), "Leggi tutto [fake:big]", |_| None).await;
    assert!(turn.status.success(), "{}", turn.stderr);
    assert_eq!(turn.too_long.len(), 1);
    assert!(turn.too_long[0] > 20 << 20);
    assert!(turn.texts().contains(&"Dopo la riga gigante."));
    assert_eq!(turn.result()["subtype"], "success");
}

#[tokio::test]
async fn approval_scenario_round_trip() {
    let fx = Fixture::new();
    let mut pendings = Vec::new();
    let turn = drive(
        fx.spawn(&[("FAKE_CLAUDE_SCENARIO", "approval")]),
        "Esegui i test",
        |line| match line {
            Inbound::CanUseTool(req) => {
                let p = Pending::new("approval-1".into(), req);
                assert!(wire::can_remember(&p.suggestions));
                let reply =
                    wire::approval_response(&p, &ApprovalDecision::Allow { remember: true });
                pendings.push(p);
                Some(reply)
            }
            _ => None,
        },
    )
    .await;
    assert!(turn.status.success(), "{}", turn.stderr);
    assert_eq!(pendings.len(), 1);
    assert_eq!(wire::remembered_rules(&pendings[0]), ["Bash(echo hello)"]);
    assert_eq!(turn.result()["subtype"], "success");
    assert!(fx.worktree.join("hello.txt").exists());
    let answered = &fx.records()[1];
    assert_eq!(answered["kind"], "control_response");
    assert_eq!(
        answered["response"]["response"]["updatedPermissions"][0]["destination"],
        "session"
    );
}

#[tokio::test]
async fn control_scenario_records_the_error_reply() {
    let fx = Fixture::new();
    let turn = drive(fx.spawn(&[]), "[fake:control]", |line| match line {
        Inbound::ControlRequest {
            request_id,
            subtype,
        } => Some(wire::control_error(
            request_id,
            &format!("Unsupported control request subtype: {subtype}"),
        )),
        _ => None,
    })
    .await;
    assert!(turn.status.success(), "{}", turn.stderr);
    let answered = &fx.records()[1];
    assert_eq!(answered["response"]["subtype"], "error");
    assert_eq!(
        answered["response"]["error"],
        "Unsupported control request subtype: hook_callback"
    );
}

#[tokio::test]
async fn limit_scenarios_end_with_classified_errors() {
    for (scenario, kind) in [
        ("usage_limit", atm_types::LimitKind::UsageLimit),
        ("auth_fail", atm_types::LimitKind::AuthFailure),
    ] {
        let fx = Fixture::new();
        let turn = drive(fx.spawn(&[]), &format!("[fake:{scenario}]"), |_| None).await;
        let result = atm_core::normalize::parse_result(turn.result()).unwrap();
        assert!(result.is_error);
        assert_eq!(result.limit, Some(kind), "{scenario}");
    }
}

#[tokio::test]
async fn crash_and_resume_fail_exit_without_result() {
    let fx = Fixture::new();
    let turn = drive(fx.spawn(&[]), "[fake:crash]", |_| None).await;
    assert_eq!(turn.status.code(), Some(1));
    assert!(turn.stderr.contains("crash simulato"));

    let turn = drive(fx.spawn(&[]), "[fake:resume_fail]", |_| None).await;
    assert_eq!(turn.status.code(), Some(1));
    assert!(atm_core::normalize::is_resume_failure(&turn.stderr));
}

#[tokio::test]
async fn killpg_reaches_the_whole_group() {
    let fx = Fixture::new();
    let spawned = fx.spawn(&[("FAKE_CLAUDE_SCENARIO", "hang_ignore")]);
    let claude::Spawned {
        mut child,
        stdin,
        stdout,
        stderr: _stderr,
        pgid,
    } = spawned;
    assert_eq!(Some(pgid as u32), child.id());
    let (tx, rx) = mpsc::channel(wire::STDIN_CHANNEL);
    tokio::spawn(wire::write_frames(stdin, rx, None));
    tx.send(wire::user_message("resta appeso")).await.unwrap();
    // Wait for `system/init`: by then the grandchild `sleep` exists.
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        let line = wire::read_line_capped(&mut reader, &mut buf, wire::MAX_STDOUT_LINE)
            .await
            .unwrap();
        assert_eq!(line, Line::Complete);
        if atm_core::normalize::init_session_id(&serde_json::from_slice(&buf).unwrap()).is_some() {
            break;
        }
    }
    let command = claude::process_command(pgid).await.unwrap();
    assert!(command.contains("fake-claude"), "{command}");
    assert!(claude::pid_alive(pgid));
    let grandchild = fx
        .records()
        .iter()
        .find_map(|r| (r["kind"] == "grandchild").then(|| r["pid"].as_i64()))
        .flatten()
        .expect("the fake recorded its grandchild");
    let grandchild = i32::try_from(grandchild).unwrap();
    assert!(claude::pid_alive(grandchild));
    // SAFETY: getpgid only reads the process table.
    assert_eq!(unsafe { libc::getpgid(grandchild) }, pgid);

    // Interrupt, EOF and SIGTERM are all ignored.
    tx.send(wire::interrupt_request(&wire::request_id(2)))
        .await
        .unwrap();
    drop(tx);
    claude::killpg(pgid, libc::SIGTERM).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(child.try_wait().unwrap().is_none());

    claude::killpg(pgid, libc::SIGKILL).unwrap();
    let status = child.wait().await.unwrap();
    assert!(status.code().is_none());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // The reparented grandchild can linger as a zombie until launchd reaps it: poll it too.
    while (claude::group_alive(pgid) || claude::pid_alive(grandchild))
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!claude::group_alive(pgid), "the grandchild sleep survived");
    assert!(!claude::pid_alive(pgid));
    assert!(!claude::pid_alive(grandchild));
    claude::killpg(pgid, libc::SIGTERM).expect("ESRCH is Ok");
    assert_eq!(claude::process_command(pgid).await, None);
}

#[test]
fn killpg_refuses_our_own_group() {
    // Signal 0 only probes: a regression here cannot hit the test harness.
    // SAFETY: getpgrp has no preconditions and cannot fail.
    let own = unsafe { libc::getpgrp() };
    for pgid in [-1, 0, 1, own] {
        let err = claude::killpg(pgid, 0).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{pgid}");
    }
    assert!(!claude::pid_alive(0));
    assert!(!claude::group_alive(0));
}

#[test]
fn spawn_error_is_io() {
    let dir = common::tempdir();
    let env = ChildEnv::new(os_vars(&[]), "/bin".as_ref(), false);
    let argv = vec![dir.path().join("missing").to_string_lossy().into_owned()];
    let err = claude::spawn(&argv, dir.path(), &env).unwrap_err();
    assert_eq!(err.code, ErrorCode::Io);
    assert_eq!(
        claude::spawn(&[], dir.path(), &env).unwrap_err().code,
        ErrorCode::Io
    );
    // A relative argv[0] would run a file of the worktree: refused even when it exists.
    write_script(dir.path(), "claude", "#!/bin/sh\n");
    let err = claude::spawn(&["./claude".into()], dir.path(), &env).unwrap_err();
    assert_eq!(err.code, ErrorCode::Io);
}
