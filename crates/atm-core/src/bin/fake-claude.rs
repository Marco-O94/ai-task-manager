//! Test double of the Claude CLI (spec §12.1); never bundled, never calls any API.
//!
//! - `--version` / `-v`: `<CLAUDE_TESTED_VERSION> (Claude Code)`.
//! - `auth status [--json|--text]`: `FAKE_CLAUDE_AUTH=in` (default) → exit 0, `out` → exit 1,
//!   anything else → exit 2. The content of the file `$FAKE_CLAUDE_AUTH_FILE` (`in`/`out`), when
//!   it exists, takes precedence: a running app can be logged out and in (M4 E2E).
//!   `auth login …` prints one line and exits 0.
//! - `-p …` (stream-json on stdin/stdout): appends one JSON line per call to
//!   `$FAKE_CLAUDE_RECORD`: `{"kind":"call","argv":[…without argv0],"cwd":…,"pwd":…,
//!   "pid":…,"env":{"<VAR>":present,…}}`, then `{"kind":"control_response","response":…}` for
//!   every answer the host gives to a request of the fake (`can_use_tool`, `hook_callback`),
//!   `{"kind":"turn","pid":…,"scenario":…,"prompt":…}` for every user message it plays (its
//!   whole text), `{"kind":"subagent","n":…,"behavior":…,"message":…}` for every answer to a
//!   sub-agent spawn of `subagents`, and
//!   `{"kind":"grandchild","pid":…}` for the `sleep` of `hang_ignore`, `{"kind":"background",
//!   "pid":…}` for the `sleep` of `background`.
//!   Answers `initialize` (except `noinit`); each user message plays the scenario named by
//!   `[fake:NAME]` in its text, else `resolve_merge` for the app's "Risolvi con l'agente"
//!   prompt, else `$FAKE_CLAUDE_SCENARIO`, else `simple`; exits 0 at EOF.
//!   An interrupt is answered with success (`{"still_queued":[]}`), the user line `[Request
//!   interrupted by user]` and a `result` `error_during_execution` (except `hang_ignore`).
//!
//! Frame shapes follow the real CLI 2.1.283 as captured in M5 (`tests/fixtures/real/`): the
//! `initialize` answer names the account (email, organization), `system/status` precedes each
//! request and a `rate_limit_event` follows the first assistant message of a turn, `can_use_tool`
//! carries `display_name`, `description`, a string `decision_reason` and three suggestions
//! (`addRules`, `addDirectories`, `setMode`), and the `sleep` of `hang_ignore` leads a process
//! group of its own, like the commands of the real Bash tool.
//!
//! Scenarios: simple, approval, slow, hang, hang_ignore, crash, noinit, big, flood, control,
//! usage_limit, auth_fail (also writes `out` to `$FAKE_CLAUDE_AUTH_FILE`), resolve_merge
//! (`$FAKE_CLAUDE_TARGET`, else the target named by the app's conflict prompt), resume_fail,
//! append (like simple, but appends the message's first line, without the tag and the leading
//! `#`, to `hello.txt`: two tasks appending to the same file conflict), background (leaves a
//! `sleep 300` running in a process group of its own, like a `run_in_background` command of the
//! real Bash tool, then succeeds and exits at EOF as usual), subagents (spawns
//! `FAKE_CLAUDE_SUBAGENTS` sub-agents one after the other, default 3: for each an `Agent`
//! `tool_use` and its `can_use_tool`, as the real CLI asks under an `ask` rule; an allowed one
//! returns a result, a denied one the host's message; then succeeds).
//! Counts: `FAKE_CLAUDE_SLOW_EVENTS` (default 20), `FAKE_CLAUDE_FLOOD_EVENTS` (default 10000).
//! `FAKE_CLAUDE_FLOOD_PAUSE_MS` (default 0): pause after every 100 texts of `flood`, so that
//! several floods started one after the other overlap (the E2E's perf phase, M6).
//!
//! `FAKE_CLAUDE_PROJECT_CONFIG=1` (M6, the malicious-repo test): at startup, like the real CLI
//! (M5, spec §13.4), the fake loads the repo's Claude configuration from its cwd. Unless
//! `--setting-sources` leaves out `project` (resp. `local`; no flag = all sources), the
//! `SessionStart` command hooks and the `apiKeyHelper` of `.claude/settings.json` (resp.
//! `.claude/settings.local.json`) run through `sh -c`; unless `--strict-mcp-config`, every
//! `.mcp.json` server with a `command` is started (waited for at most 5 s, then killed). Each
//! run is recorded as `{"kind":"project_config","what":"hook"|"apiKeyHelper"|"mcp",…}`, and
//! `system/init` lists the servers and says `apiKeySource: "apiKeyHelper"` after a helper ran.
//! `apiKeySource` is otherwise `none`, or `ANTHROPIC_API_KEY` when that variable reached the
//! fake (not empty), or the value of `FAKE_CLAUDE_API_KEY_SOURCE` when set, which wins over
//! both: the app must stop a turn that would bill through an API key (spec §7.6).
//! `FAKE_CLAUDE_DELTA_MS` (default 0): pause between the text deltas of a streamed text, so a
//! UI can be seen rendering it progressively.

use std::io::{BufRead as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, exit};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Variables whose presence is recorded (never their values): the credentials, a parent
/// Claude Code session's variables and its host's (every name of `claude::CLAUDE_NESTING_VARS`
/// and `HOST_SESSION_VARS`, one per prefix: `tests/claude.rs` checks it; cmux's `NODE_OPTIONS`
/// and its marker) and the git ones the app must remove (spec §7.2), plus
/// `CLAUDE_CONFIG_DIR`, the user's configuration that must pass.
const RECORDED_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "GIT_DIR",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_SSE_PORT",
    "ENABLE_IDE_INTEGRATION",
    "CMUX_SOCKET_PATH",
    "CMUX_CUA_AUTH_TOKEN_FILE",
    "CMUX_ORIGINAL_NODE_OPTIONS_PRESENT",
    "NODE_OPTIONS",
    "CLAUDE_CONFIG_DIR",
];
/// Overrides `system/init.apiKeySource` (any value, `none` included).
const API_KEY_SOURCE_ENV: &str = "FAKE_CLAUDE_API_KEY_SOURCE";
/// `1` = load the repo's configuration at startup ([`Session::load_project_config`]).
const PROJECT_CONFIG_ENV: &str = "FAKE_CLAUDE_PROJECT_CONFIG";
/// How long a project MCP server may run before the fake kills it.
const MCP_SERVER_WAIT: Duration = Duration::from_secs(5);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--version" | "-v", ..] => println!("{} (Claude Code)", atm_types::CLAUDE_TESTED_VERSION),
        ["auth", "status", rest @ ..] => auth_status(rest.contains(&"--text")),
        ["auth", "login", ..] => println!("fake-claude: login simulato, nessun account toccato"),
        _ if words.iter().any(|&a| a == "-p" || a == "--print") => Session::start(&args).run(),
        _ => {
            eprintln!("fake-claude: unsupported arguments {args:?}");
            exit(2);
        }
    }
}

/// Where a running app's tests keep the login state (`in`/`out`), see [`auth_status`].
const AUTH_FILE_ENV: &str = "FAKE_CLAUDE_AUTH_FILE";
/// First words of the app's "Risolvi con l'agente" follow-up (spec §8.7).
const CONFLICT_PROMPT: &str = "This branch conflicts with `";

fn auth_status(text: bool) {
    let from_file = std::env::var_os(AUTH_FILE_ENV)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|s| s.trim().to_owned());
    let state = from_file.or_else(|| std::env::var("FAKE_CLAUDE_AUTH").ok());
    let logged_in = match state.as_deref() {
        Some("in") | None => true,
        Some("out") => false,
        Some(other) => {
            eprintln!("fake-claude: FAKE_CLAUDE_AUTH={other}");
            exit(2);
        }
    };
    if text {
        let status = if logged_in {
            "Logged in"
        } else {
            "Not logged in"
        };
        println!("{status}");
    } else if logged_in {
        let status = json!({
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "fake@example.com",
            "orgId": "fake-org-id",
            "orgName": "Fake Org",
            "subscriptionType": "max",
        });
        println!("{status}");
    } else {
        println!("{}", json!({"loggedIn": false, "authMethod": "none"}));
    }
    exit(if logged_in { 0 } else { 1 });
}

/// Why a scenario stopped early.
enum Stop {
    /// Interrupt with this request id (not answered yet).
    Interrupted(String),
    Eof,
}

type Step = Result<(), Stop>;

struct Session {
    rx: Receiver<Value>,
    session_id: String,
    /// The `rate_limit_event` of this turn went out (after its first assistant message).
    rate_limit_sent: bool,
    cwd: PathBuf,
    permission_mode: String,
    record: Option<PathBuf>,
    counter: u64,
    /// Text of the user message being played.
    prompt: String,
    /// Project MCP servers started at startup (`system/init.mcp_servers`).
    mcp_servers: Vec<String>,
    /// `system/init.apiKeySource`.
    api_key_source: String,
}

impl Session {
    fn start(args: &[String]) -> Session {
        let flag = |name: &str| {
            args.iter()
                .find_map(|a| a.strip_prefix(name))
                .map(str::to_owned)
        };
        let mut session = Session {
            rx: spawn_stdin_reader(),
            session_id: flag("--session-id=")
                .or_else(|| flag("--resume="))
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            rate_limit_sent: false,
            cwd: std::env::current_dir().unwrap_or_default(),
            permission_mode: flag("--permission-mode=").unwrap_or_else(|| "default".into()),
            record: std::env::var_os("FAKE_CLAUDE_RECORD").map(PathBuf::from),
            counter: 0,
            prompt: String::new(),
            mcp_servers: Vec::new(),
            api_key_source: if std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|k| !k.is_empty())
            {
                "ANTHROPIC_API_KEY".into()
            } else {
                "none".into()
            },
        };
        let env: serde_json::Map<String, Value> = RECORDED_VARS
            .iter()
            .map(|&k| (k.to_owned(), std::env::var_os(k).is_some().into()))
            .collect();
        session.record(json!({
            "kind": "call",
            "argv": args,
            "cwd": session.cwd,
            "pwd": std::env::var_os("PWD").map(|p| p.to_string_lossy().into_owned()),
            // Lets a test find its own agents among other fake-claude processes.
            "pid": std::process::id(),
            "env": env,
        }));
        if std::env::var(PROJECT_CONFIG_ENV).as_deref() == Ok("1") {
            session.load_project_config(args);
        }
        if let Ok(source) = std::env::var(API_KEY_SOURCE_ENV) {
            session.api_key_source = source;
        }
        session
    }

    /// What the real CLI runs of the repo's configuration at startup (module docs).
    fn load_project_config(&mut self, args: &[String]) {
        let sources: Vec<&str> = args
            .iter()
            .find_map(|a| a.strip_prefix("--setting-sources="))
            .map_or_else(
                || vec!["user", "project", "local"],
                |list| list.split(',').map(str::trim).collect(),
            );
        for (source, file) in [
            ("project", "settings.json"),
            ("local", "settings.local.json"),
        ] {
            if !sources.contains(&source) {
                continue;
            }
            let path = self.cwd.join(".claude").join(file);
            let Some(settings) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            let hooks = settings["hooks"]["SessionStart"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|matcher| matcher["hooks"].as_array().into_iter().flatten());
            for command in hooks.filter_map(|h| h["command"].as_str()) {
                self.run_config("hook", command);
            }
            if let Some(helper) = settings["apiKeyHelper"].as_str() {
                self.run_config("apiKeyHelper", helper);
                self.api_key_source = "apiKeyHelper".into();
            }
        }
        if args.iter().any(|a| a == "--strict-mcp-config") {
            return;
        }
        let Some(mcp) = std::fs::read_to_string(self.cwd.join(".mcp.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            return;
        };
        let servers = mcp["mcpServers"].as_object().cloned().unwrap_or_default();
        for (name, server) in servers {
            let Some(command) = server["command"].as_str() else {
                continue;
            };
            let server_args: Vec<&str> = server["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let spawned = Command::new(command)
                .args(&server_args)
                .current_dir(&self.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut child) = spawned {
                let deadline = Instant::now() + MCP_SERVER_WAIT;
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
            }
            self.record(json!({"kind": "project_config", "what": "mcp", "server": name}));
            self.mcp_servers.push(name);
        }
    }

    /// `sh -c <command>` in the cwd, waited for, then recorded.
    fn run_config(&self, what: &str, command: &str) {
        let _ = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        self.record(json!({"kind": "project_config", "what": what, "command": command}));
    }

    fn record(&self, line: Value) {
        let Some(path) = &self.record else { return };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open FAKE_CLAUDE_RECORD");
        // One write per line: concurrent fakes may share the file.
        file.write_all(format!("{line}\n").as_bytes())
            .expect("write FAKE_CLAUDE_RECORD");
    }

    fn run(mut self) -> ! {
        if std::env::var("FAKE_CLAUDE_SCENARIO").as_deref() == Ok("noinit") {
            exit(1);
        }
        loop {
            let Ok(msg) = self.rx.recv() else { exit(0) };
            match msg["type"].as_str() {
                Some("control_request") => self.answer(&msg),
                Some("user") => {
                    self.prompt = user_text(&msg);
                    let scenario = pick_scenario(&self.prompt);
                    self.record(json!({
                        "kind": "turn",
                        "pid": std::process::id(),
                        "scenario": scenario,
                        "prompt": self.prompt,
                    }));
                    self.rate_limit_sent = false;
                    match self.play(&scenario) {
                        Ok(()) => {}
                        Err(Stop::Interrupted(request_id)) => self.interrupted(&request_id),
                        Err(Stop::Eof) => exit(0),
                    }
                }
                _ => {}
            }
        }
    }

    /// A host request outside a scenario wait (interrupt while idle included).
    fn answer(&mut self, msg: &Value) {
        let request_id = msg["request_id"].as_str().unwrap_or_default();
        let reply = match msg["request"]["subtype"].as_str().unwrap_or_default() {
            "initialize" => self.initialize_answer(request_id),
            "interrupt" => control_success(request_id, json!({"still_queued": []})),
            other => json!({"type": "control_response", "response": {
                "subtype": "error",
                "request_id": request_id,
                "error": format!("Unsupported control request subtype: {other}"),
            }}),
        };
        self.send(reply);
    }

    /// The next host response to a request of the fake, `None` at `deadline`. Answers other
    /// requests; an interrupt or EOF ends the scenario.
    fn next_response(&mut self, deadline: Option<Instant>) -> Result<Option<Value>, Stop> {
        loop {
            let msg = match deadline {
                None => self.rx.recv().map_err(|_| Stop::Eof)?,
                Some(at) => match self
                    .rx
                    .recv_timeout(at.saturating_duration_since(Instant::now()))
                {
                    Ok(msg) => msg,
                    Err(RecvTimeoutError::Timeout) => return Ok(None),
                    Err(RecvTimeoutError::Disconnected) => return Err(Stop::Eof),
                },
            };
            match msg["type"].as_str() {
                Some("control_request") if msg["request"]["subtype"] == "interrupt" => {
                    let id = msg["request_id"].as_str().unwrap_or_default();
                    return Err(Stop::Interrupted(id.to_owned()));
                }
                Some("control_request") => self.answer(&msg),
                Some("control_response") => {
                    let response = msg["response"].clone();
                    self.record(json!({"kind": "control_response", "response": response}));
                    return Ok(Some(response));
                }
                _ => {}
            }
        }
    }

    fn pause(&mut self, duration: Duration) -> Step {
        let deadline = Instant::now() + duration;
        while self.next_response(Some(deadline))?.is_some() {}
        Ok(())
    }

    fn await_response(&mut self, request_id: &str) -> Result<Value, Stop> {
        loop {
            if let Some(r) = self.next_response(None)?
                && r["request_id"] == request_id
            {
                return Ok(r);
            }
        }
    }

    fn play(&mut self, scenario: &str) -> Step {
        match scenario {
            "noinit" => exit(1),
            "resume_fail" => {
                eprintln!("No conversation found with session ID: {}", self.session_id);
                exit(1);
            }
            "hang_ignore" => {
                let pid = ignore_sigterm_with_grandchild();
                self.record(json!({"kind": "grandchild", "pid": pid}));
                self.init();
            }
            _ => self.init(),
        }
        match scenario {
            "simple" => self.simple(),
            "append" => self.append(),
            "approval" => self.approval()?,
            "slow" => {
                let n = count_env("FAKE_CLAUDE_SLOW_EVENTS", 20);
                for i in 1..=n {
                    self.pause(Duration::from_secs(1))?;
                    self.text(&format!("Passo {i}/{n}"));
                }
                self.result("success", false, Some("Lavoro lento completato."));
            }
            "hang" => loop {
                self.next_response(None)?;
            },
            "hang_ignore" => self.hang_ignore(),
            "background" => {
                let pid = sleep_in_own_group();
                self.record(json!({"kind": "background", "pid": pid}));
                self.text("Avviato un processo in background.");
                self.result("success", false, Some("Processo in background avviato."));
            }
            "crash" => {
                eprintln!("fake-claude: crash simulato");
                exit(1);
            }
            "big" => {
                let path = self.cwd.join("big.txt");
                let id = self.tool_use("Read", json!({"file_path": path}));
                let output: String = (0..3200).map(|i| format!("riga {i:010}\n")).collect();
                self.tool_result(&id, &output, false);
                self.text(&"x".repeat(20 << 20));
                self.text("Dopo la riga gigante.");
                self.result("success", false, Some("Output grande completato."));
            }
            "flood" => {
                let pause =
                    Duration::from_millis(count_env("FAKE_CLAUDE_FLOOD_PAUSE_MS", 0).into());
                for i in 0..count_env("FAKE_CLAUDE_FLOOD_EVENTS", 10_000) {
                    if i > 0 && i % 100 == 0 && !pause.is_zero() {
                        std::thread::sleep(pause);
                    }
                    self.text(&format!("Evento {i}"));
                }
                self.result("success", false, Some("Flood completato."));
            }
            "control" => {
                let request_id = self.next_id("fake_req");
                self.send(
                    json!({"type": "control_request", "request_id": request_id, "request": {
                        "subtype": "hook_callback",
                        "callback_id": "fake_hook",
                        "input": {"hook_event_name": "PreToolUse", "tool_name": "Bash"},
                        "tool_use_id": null,
                    }}),
                );
                let response = self.await_response(&request_id)?;
                self.text(&format!("Risposta al hook: {}", response["subtype"]));
                self.result("success", false, Some("Hook inviato."));
            }
            "usage_limit" => self.result(
                "success",
                true,
                Some("Claude AI usage limit reached. Your limit will reset at 5pm (Europe/Rome)."),
            ),
            "auth_fail" => {
                // The CLI's login is gone: its `auth status` says so from now on.
                if let Some(path) = std::env::var_os(AUTH_FILE_ENV) {
                    let _ = write_atomically(Path::new(&path), "out");
                }
                self.result("success", true, Some("Not logged in · Please run /login"));
            }
            "resolve_merge" => self.resolve_merge(),
            "subagents" => self.subagents()?,
            other => self.result(
                "error_during_execution",
                true,
                Some(&format!("fake-claude: unknown scenario {other}")),
            ),
        }
        Ok(())
    }

    /// Streams a short text, writes `hello.txt` with a Write tool call, then succeeds.
    fn simple(&mut self) {
        self.stream_text("Creo hello.txt nel worktree.");
        let path = self.cwd.join("hello.txt");
        let id = self.tool_use("Write", json!({"file_path": path, "content": "hello\n"}));
        match std::fs::write(&path, "hello\n") {
            Ok(()) => {
                let msg = format!("File created successfully at: {}", path.display());
                self.tool_result(&id, &msg, false);
            }
            Err(e) => self.tool_result(&id, &e.to_string(), true),
        }
        self.text("Fatto: hello.txt creato.");
        self.result("success", false, Some("Fatto: hello.txt creato."));
    }

    /// Appends the first line of the message (without the `[fake:…]` tag and the leading `#`
    /// of the app's task prompt) to `hello.txt` with a Write call, then succeeds.
    fn append(&mut self) {
        let path = self.cwd.join("hello.txt");
        let line = append_line(&self.prompt);
        let content = std::fs::read_to_string(&path).unwrap_or_default() + &line + "\n";
        let id = self.tool_use("Write", json!({"file_path": path, "content": content}));
        match std::fs::write(&path, &content) {
            Ok(()) => {
                let msg = format!("The file {} has been updated.", path.display());
                self.tool_result(&id, &msg, false);
            }
            Err(e) => self.tool_result(&id, &e.to_string(), true),
        }
        self.result(
            "success",
            false,
            Some(&format!("Aggiunto a hello.txt: {line}")),
        );
    }

    fn approval(&mut self) -> Step {
        let input = json!({"command": "echo hello", "description": "Print hello"});
        let tool_use_id = self.tool_use("Bash", input.clone());
        let response = self.can_use_tool(json!({
            "tool_name": "Bash",
            "display_name": "Bash",
            "input": input,
            "description": "echo hello",
            "permission_suggestions": [
                {"type": "addRules",
                 "rules": [{"toolName": "Bash", "ruleContent": "echo hello"}],
                 "behavior": "allow", "destination": "localSettings"},
                {"type": "addDirectories", "directories": [self.cwd],
                 "destination": "session"},
                {"type": "setMode", "mode": "acceptEdits", "destination": "session"},
            ],
            "decision_reason": "This command requires approval",
            "decision_reason_type": "other",
            "tool_use_id": tool_use_id,
        }))?;
        let decision = &response["response"];
        if response["subtype"] == "success" && decision["behavior"] == "allow" {
            self.tool_result(&tool_use_id, "hello\n", false);
        } else {
            let message = decision["message"].as_str().or(response["error"].as_str());
            self.tool_result(&tool_use_id, message.unwrap_or("denied"), true);
            if decision["interrupt"] == true {
                self.result("error_during_execution", true, None);
                return Ok(());
            }
        }
        self.simple();
        Ok(())
    }

    /// `FAKE_CLAUDE_SUBAGENTS` sub-agent spawns (default 3), one after the other, each asking
    /// the host like the real CLI under an `ask` rule on `Agent`; every answer is recorded
    /// (`kind: "subagent"`).
    fn subagents(&mut self) -> Step {
        let n = count_env("FAKE_CLAUDE_SUBAGENTS", 3);
        let mut allowed = 0;
        for i in 1..=n {
            let input = json!({"description": format!("Sub-agent {i}"),
                               "prompt": "Summarize README.md", "subagent_type": "general-purpose"});
            let tool_use_id = self.tool_use("Agent", input.clone());
            let response = self.can_use_tool(json!({
                "tool_name": "Agent",
                "display_name": "Agent",
                "input": input,
                "description": format!("Sub-agent {i}"),
                "permission_suggestions": [],
                "decision_reason": "Permission rule 'Agent' requires confirmation",
                "decision_reason_type": "rule",
                "tool_use_id": tool_use_id,
            }))?;
            let decision = &response["response"];
            let behavior = decision["behavior"].as_str().unwrap_or("error");
            self.record(json!({"kind": "subagent", "n": i, "behavior": behavior,
                               "message": decision["message"]}));
            if behavior == "allow" {
                allowed += 1;
                self.tool_result(
                    &tool_use_id,
                    &format!("Sub-agent {i}: README riassunto."),
                    false,
                );
            } else {
                let message = decision["message"].as_str().unwrap_or("denied");
                self.tool_result(&tool_use_id, message, true);
            }
        }
        self.result(
            "success",
            false,
            Some(&format!("Sub-agent avviati: {allowed} su {n}.")),
        );
        Ok(())
    }

    /// Sends a `can_use_tool` with the fields of `request` and waits for the host's answer; an
    /// interrupt meanwhile cancels the request first (`control_cancel_request`).
    fn can_use_tool(&mut self, mut request: Value) -> Result<Value, Stop> {
        request["subtype"] = "can_use_tool".into();
        let request_id = self.next_id("fake_req");
        self.send(json!({"type": "control_request", "request_id": request_id, "request": request}));
        match self.await_response(&request_id) {
            Err(Stop::Interrupted(id)) => {
                self.send(json!({"type": "control_cancel_request", "request_id": request_id}));
                Err(Stop::Interrupted(id))
            }
            other => other,
        }
    }

    /// Ignores interrupts and EOF (SIGTERM is already ignored): only SIGKILL ends it.
    fn hang_ignore(&mut self) -> ! {
        loop {
            if let Err(Stop::Eof) = self.next_response(None) {
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
        }
    }

    /// `git merge <target>`, conflicts resolved by concatenating ours + theirs. The target is
    /// `$FAKE_CLAUDE_TARGET`, else the branch named by the app's conflict prompt.
    fn resolve_merge(&mut self) {
        let target = std::env::var("FAKE_CLAUDE_TARGET")
            .ok()
            .or_else(|| conflict_target(&self.prompt));
        let Some(target) = target else {
            let text = "fake-claude: FAKE_CLAUDE_TARGET non impostata e nessun target nel prompt";
            return self.result("error_during_execution", true, Some(text));
        };
        let id = self.tool_use("Bash", json!({"command": format!("git merge {target}")}));
        match merge_concatenating(&self.cwd, &target) {
            Ok(log) => {
                self.tool_result(&id, &log, false);
                self.result("success", false, Some("Merge risolto."));
            }
            Err(log) => {
                self.tool_result(&id, &log, true);
                self.result("error_during_execution", true, Some("Merge non riuscito."));
            }
        }
    }

    // ---- output frames -------------------------------------------------------------------

    fn send(&self, frame: Value) {
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{frame}").and_then(|()| out.flush()).is_err() {
            exit(1);
        }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}_{}", self.counter)
    }

    fn envelope(&self, kind: &str, mut body: Value) -> Value {
        body["type"] = kind.into();
        body["session_id"] = self.session_id.clone().into();
        body["uuid"] = uuid::Uuid::new_v4().to_string().into();
        body
    }

    /// The real answer to `initialize` (M5): the account with email and organization (which
    /// the app must never log), the catalog lists, the mode, and two extra wrapper keys.
    fn initialize_answer(&self, request_id: &str) -> Value {
        json!({"type": "control_response", "response": {
            "subtype": "success",
            "request_id": request_id,
            "pending_permission_requests": [],
            "pending_user_dialog_requests": [],
            "response": {
                "commands": [], "agents": [], "output_style": "default",
                "available_output_styles": ["default"],
                "models": [{"value": "default", "displayName": "Default (recommended)"}],
                "account": {"email": "fake@example.com", "organization": "Fake Org",
                            "subscriptionType": "Claude Max", "apiProvider": "firstParty"},
                "pid": std::process::id(),
                "current_permission_mode": self.permission_mode,
                "session_state": "idle",
            },
        }})
    }

    fn init(&mut self) {
        let frame = self.envelope(
            "system",
            json!({
                "subtype": "init",
                "cwd": self.cwd,
                "tools": ["Bash", "Read", "Write", "Edit"],
                "mcp_servers": self.mcp_servers.iter()
                    .map(|name| json!({"name": name, "status": "connected"}))
                    .collect::<Vec<_>>(),
                "model": "claude-fake",
                "permissionMode": self.permission_mode,
                "apiKeySource": self.api_key_source,
                "claude_code_version": atm_types::CLAUDE_TESTED_VERSION,
                "slash_commands": [], "skills": [], "plugins": [], "agents": [],
                "output_style": "default",
                "capabilities": ["interrupt_receipt_v1"],
                "fast_mode_state": "off",
            }),
        );
        self.send(frame);
        self.status();
    }

    /// `system/status` `requesting`: the real CLI sends one before every API request.
    fn status(&mut self) {
        let frame = self.envelope(
            "system",
            json!({"subtype": "status", "status": "requesting"}),
        );
        self.send(frame);
    }

    /// The interrupt's answer, the user line the real CLI adds, and its `result`.
    fn interrupted(&mut self, request_id: &str) {
        self.send(control_success(request_id, json!({"still_queued": []})));
        let frame = self.envelope(
            "user",
            json!({"message": {"role": "user", "content": [
                {"type": "text", "text": "[Request interrupted by user]"}]},
                "parent_tool_use_id": null}),
        );
        self.send(frame);
        self.result("error_during_execution", true, None);
    }

    fn stream_text(&mut self, text: &str) {
        let events = [json!({"type": "content_block_start", "index": 0,
                             "content_block": {"type": "text", "text": ""}})]
        .into_iter()
        .chain(text.split_inclusive(' ').map(|chunk| {
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": chunk}})
        }))
        .chain([json!({"type": "content_block_stop", "index": 0})]);
        let delay = Duration::from_millis(count_env("FAKE_CLAUDE_DELTA_MS", 0).into());
        for event in events {
            if event["type"] == "content_block_delta" && !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let frame = self.envelope(
                "stream_event",
                json!({"event": event, "parent_tool_use_id": null}),
            );
            self.send(frame);
        }
        self.text(text);
    }

    fn assistant(&mut self, content: Value) {
        let id = self.next_id("msg_fake");
        let frame = self.envelope("assistant", json!({
            "message": {"id": id, "type": "message", "role": "assistant", "model": "claude-fake",
                        "content": content, "stop_reason": null,
                        "usage": {"input_tokens": 1, "output_tokens": 1}},
            "parent_tool_use_id": null,
        }));
        self.send(frame);
        if !std::mem::replace(&mut self.rate_limit_sent, true) {
            let frame = self.envelope("rate_limit_event", json!({"rate_limit_info": {
                "status": "allowed", "resetsAt": 1_790_000_000, "rateLimitType": "five_hour",
                "overageStatus": "rejected", "isUsingOverage": false,
                "unifiedWindows": {"five_hour": {"utilization": 0.1, "resetsAt": 1_790_000_000}},
            }}));
            self.send(frame);
        }
    }

    fn text(&mut self, text: &str) {
        self.assistant(json!([{"type": "text", "text": text}]));
    }

    fn tool_use(&mut self, name: &str, input: Value) -> String {
        let id = self.next_id("toolu_fake");
        self.assistant(
            json!([{"type": "tool_use", "id": id, "name": name, "input": input,
                               "caller": {"type": "direct"}}]),
        );
        id
    }

    fn tool_result(&mut self, tool_use_id: &str, content: &str, is_error: bool) {
        let frame = self.envelope(
            "user",
            json!({
                "message": {"role": "user", "content": [{"type": "tool_result",
                    "tool_use_id": tool_use_id, "content": content, "is_error": is_error}]},
                "parent_tool_use_id": null,
            }),
        );
        self.send(frame);
    }

    fn result(&mut self, subtype: &str, is_error: bool, text: Option<&str>) {
        let mut frame = self.envelope(
            "result",
            json!({
                "subtype": subtype,
                "is_error": is_error,
                "duration_ms": 1200,
                "duration_api_ms": 900,
                "num_turns": 1,
                "total_cost_usd": 0.0123,
                "usage": {"input_tokens": 10, "output_tokens": 20},
                "permission_denials": [],
                "stop_reason": if is_error { "tool_use" } else { "end_turn" },
                "terminal_reason": if subtype == "success" { "completed" } else { "aborted_streaming" },
            }),
        );
        match text {
            Some(text) => frame["result"] = text.into(),
            // The real CLI (M5) explains an error without text in `errors`.
            None if is_error => {
                frame["errors"] = json!(["[ede_diagnostic] result_type=user"]);
            }
            None => {}
        }
        self.send(frame);
    }
}

fn control_success(request_id: &str, response: Value) -> Value {
    json!({"type": "control_response",
           "response": {"subtype": "success", "request_id": request_id, "response": response}})
}

/// Ignores SIGTERM, then starts [`sleep_in_own_group`] and returns its pid: the ignore is
/// inherited across exec, so only the app's SIGKILL of the fake's group plus its descendants
/// ends both.
fn ignore_sigterm_with_grandchild() -> u32 {
    // SAFETY: changes this process's disposition of one signal; no handler code runs.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    sleep_in_own_group()
}

/// Starts `sleep 300` leading a process group of its own, as the real Bash tool does (M5), with
/// no pipe of the fake's, and returns its pid; never waited for.
fn sleep_in_own_group() -> u32 {
    let spawned = Command::new("sleep")
        .arg("300")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => child.id(),
        Err(e) => {
            eprintln!("fake-claude: sleep not started: {e}");
            exit(1);
        }
    }
}

/// Host frames, one JSON value per stdin line; the channel closes at EOF.
fn spawn_stdin_reader() -> Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str(&line)
                && tx.send(v).is_err()
            {
                break;
            }
        }
    });
    rx
}

fn user_text(msg: &Value) -> String {
    match &msg["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The target branch of the app's "Risolvi con l'agente" prompt: "This branch conflicts with
/// `<target>` in: …".
fn conflict_target(text: &str) -> Option<String> {
    let (target, _) = text.strip_prefix(CONFLICT_PROMPT)?.split_once('`')?;
    (!target.is_empty() && !target.starts_with('-')).then(|| target.to_owned())
}

/// The line `append` adds: the message's first non-empty line without its `[fake:…]` tag and
/// the leading `#` of the app's task prompt.
fn append_line(text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let untagged = match first.split_once("[fake:") {
        Some((head, rest)) => {
            let tail = rest.split_once(']').map_or("", |(_, tail)| tail);
            format!("{head}{tail}")
        }
        None => first.to_owned(),
    };
    untagged.trim().trim_start_matches('#').trim().to_owned()
}

/// Writes `content` to a temporary file next to `path`, then renames it over `path`: a reader
/// (the app's `auth status` probe) sees the old content or the new one, never an empty file.
fn write_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

fn pick_scenario(text: &str) -> String {
    text.split_once("[fake:")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(name, _)| name.trim().to_owned())
        .or_else(|| {
            text.starts_with(CONFLICT_PROMPT)
                .then(|| "resolve_merge".to_owned())
        })
        .or_else(|| std::env::var("FAKE_CLAUDE_SCENARIO").ok())
        .unwrap_or_else(|| "simple".into())
}

fn count_env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Runs git in `cwd` (hooks off: the fake stands for the agent, not for the app's git).
fn git(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))
}

fn merge_concatenating(cwd: &Path, target: &str) -> Result<String, String> {
    let merge = git(cwd, &["merge", "--no-edit", target])?;
    let mut log = String::from_utf8_lossy(&merge.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&merge.stderr));
    if merge.status.success() {
        return Ok(log);
    }
    let conflicted = git(cwd, &["diff", "--name-only", "-z", "--diff-filter=U"])?.stdout;
    let conflicted = String::from_utf8_lossy(&conflicted).into_owned();
    let paths: Vec<&str> = conflicted.split('\0').filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        return Err(log);
    }
    for path in paths {
        let ours = git(cwd, &["show", &format!(":2:{path}")])?.stdout;
        let theirs = git(cwd, &["show", &format!(":3:{path}")])?.stdout;
        std::fs::write(cwd.join(path), [ours, theirs].concat()).map_err(|e| e.to_string())?;
        git(cwd, &["add", "--", path])?;
    }
    let commit = git(cwd, &["commit", "--no-edit"])?;
    log.push_str(&String::from_utf8_lossy(&commit.stdout));
    log.push_str(&String::from_utf8_lossy(&commit.stderr));
    if commit.status.success() {
        Ok(log)
    } else {
        Err(log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conflict_prompt_plays_resolve_merge_on_its_target() {
        let prompt = "This branch conflicts with `release/1.2` in: hello.txt. Run `git merge \
                      release/1.2`, resolve every conflict.";
        assert_eq!(pick_scenario(prompt), "resolve_merge");
        assert_eq!(conflict_target(prompt).as_deref(), Some("release/1.2"));
        assert_eq!(pick_scenario("Crea hello [fake:approval]"), "approval");
        assert_eq!(conflict_target("Merge `main` please"), None);
        assert_eq!(conflict_target("This branch conflicts with `` in: x"), None);
        assert_eq!(
            conflict_target("This branch conflicts with `--all` in: x"),
            None
        );
    }

    #[test]
    fn append_adds_the_first_line_without_tag_and_heading() {
        assert_eq!(
            append_line("# Hola su hello [fake:append]\n\nDescrizione"),
            "Hola su hello"
        );
        assert_eq!(append_line("\nSaluta [fake:append] tutti"), "Saluta  tutti");
        assert_eq!(append_line("Solo testo"), "Solo testo");
        assert_eq!(pick_scenario("# Conflitto [fake:append]"), "append");
    }

    #[test]
    fn atomic_write_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("fake-claude-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth");
        std::fs::write(&path, "in").unwrap();
        write_atomically(&path, "out").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "out");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
