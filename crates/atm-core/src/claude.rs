//! The user's Claude Code CLI: discovery, version gate, login-shell PATH, child environment,
//! exact argv, `auth status`, login script, spawn and process-group kill (spec §7.1–§7.3,
//! §7.10). Owner: M2-CLAUDE. Nothing here reads the Keychain or `~/.claude`.

// The OAuth token variable (CLAUDE_CODE_OAUTH_TOKEN) is never set, read, logged or stored by
// the app: if present in the app's environment it simply passes through to the child.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use atm_types::{
    AppError, AuthState, CLAUDE_MIN_VERSION, CLAUDE_TESTED_VERSION, ClaudeInfo, Effort, ErrorCode,
    LoginMethod, PermissionMode,
};
use serde::Serialize;
use serde_json::Value;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::git::is_scrubbed_git_var;

/// Dev and tests point this at `fake-claude` (spec §7.1, candidate 2).
pub const ATM_CLAUDE_PATH_ENV: &str = "ATM_CLAUDE_PATH";

/// PATH used when the login shell cannot be queried; `~` is expanded against `HOME`.
pub const FALLBACK_PATH: &str =
    "~/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Markers around `$PATH` in the login-shell output (spec §7.2).
pub const PATH_MARKER: &str = "__ATM__";

/// Timeouts (spec §7.11).
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
pub const AUTH_STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// `permissions.deny` of the `--settings` JSON, passed on every turn (spec §7.8).
pub const DENY_RULES: &[&str] = &[
    "Bash(git push *)",
    "Bash(git push)",
    "Read(~/.ssh/**)",
    "Read(~/.aws/**)",
    "Read(~/.claude/.credentials.json)",
    "Edit(~/.claude/**)",
    "Edit(~/.ssh/**)",
];

/// API credentials removed from the child environment unless `allow_env_api_key`.
pub const API_KEY_VARS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];
/// Set by a parent Claude Code session for itself and its tools, and inherited by the app when
/// it is launched from inside one (a Bash tool, a terminal of such a session): removed, with
/// [`CLAUDE_NESTING_PREFIXES`], so that every child runs as a top-level session of its own.
/// Seen in the environment of a Claude Code 2.1.x Bash tool (M6, 2026-09-28):
/// - `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`: mark a nested CLI and carry the parent's entry
///   point (spec §7.2);
/// - `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED`: the
///   parent's session identity and state; an agent has its own `--session-id` and nobody
///   attending its terminal;
/// - `CLAUDE_CODE_EXECPATH`, `CLAUDE_PID`: the parent's executable and process;
/// - `CLAUDE_EFFORT`: the parent's effort level, which would silently become the effort of an
///   attempt started without `--effort`.
///
/// Kept on purpose, being the user's configuration rather than a session's: `CLAUDE_CONFIG_DIR`,
/// the other `CLAUDE_CODE_*` settings (e.g. `CLAUDE_CODE_MAX_OUTPUT_TOKENS`), the OAuth token
/// variable (passthrough, see the note at the top of this file), `CLAUDE_CODE_USE_BEDROCK`/
/// `_VERTEX`/`_FOUNDRY`, which select the provider, and the [`BASE_URL_VARS`], which send the
/// requests elsewhere: both surfaced instead ([`cloud_provider_env`], [`base_url_env`] →
/// `EnvStatus` → billing banners).
pub const CLAUDE_NESTING_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];
/// Prefixes of parent-session variables: `CLAUDE_CODE_MESSAGING_*` are the socket and the
/// token of the parent session's messaging channel, a credential that would let an agent talk
/// to the session that launched the app.
pub const CLAUDE_NESTING_PREFIXES: &[&str] = &["CLAUDE_CODE_MESSAGING_"];
/// The host that ran the parent session, inherited the same way (M6, seen in a cmux terminal
/// and in IDE terminals, 2026-09-28), removed with [`HOST_SESSION_PREFIXES`]:
/// - `CLAUDE_CODE_SSE_PORT`, `ENABLE_IDE_INTEGRATION`: an IDE terminal's link to its editor
///   extension; the child `claude` would connect to that IDE, which nobody asked for.
pub const HOST_SESSION_VARS: &[&str] = &["CLAUDE_CODE_SSE_PORT", "ENABLE_IDE_INTEGRATION"];
/// `CMUX_*`: the cmux terminal's automation channel (`CMUX_SOCKET_PATH`, `CMUX_SOCKET`, the
/// computer-use socket `CMUX_CUA_SOCKET_PATH` and its `CMUX_CUA_AUTH_TOKEN_FILE`, surface and
/// workspace ids, its hook binary): a ready-made path for an agent to drive the terminal and
/// the screen, native confirmation dialogs included.
pub const HOST_SESSION_PREFIXES: &[&str] = &["CMUX_"];
/// A truthy value of any of these selects a third-party provider.
pub const CLOUD_PROVIDER_VARS: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
/// Another endpoint for the CLI's requests: `ANTHROPIC_BASE_URL` (a proxy or a gateway, which
/// receives every request with the user's subscription credentials and may bill its own way;
/// `system/init` still says `apiKeySource: "none"`, so the runtime stop never sees it) and a
/// provider's (`ANTHROPIC_BEDROCK_BASE_URL`, `_VERTEX_`, `_FOUNDRY_`, used with that provider).
/// In the app's environment they are the user's configuration: kept, like
/// [`CLOUD_PROVIDER_VARS`], and surfaced ([`base_url_env`] → `EnvStatus::base_url_env` →
/// billing banner, spec §7.2). In a repository's settings they are never approved (spec §8.9).
pub const BASE_URL_VARS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
];

/// A variable of a parent Claude Code session or of its host ([`CLAUDE_NESTING_VARS`],
/// [`CLAUDE_NESTING_PREFIXES`], [`HOST_SESSION_VARS`], [`HOST_SESSION_PREFIXES`]): never
/// passed to a child, and removed from the app's own process at startup (the shell re-executes
/// itself without them, so that `ps -E` of the app does not show them either).
pub fn is_nesting_var(name: &str) -> bool {
    CLAUDE_NESTING_VARS.contains(&name)
        || HOST_SESSION_VARS.contains(&name)
        || CLAUDE_NESTING_PREFIXES
            .iter()
            .chain(HOST_SESSION_PREFIXES)
            .any(|p| name.starts_with(p))
}

/// `NODE_OPTIONS`, which cmux rewrites (below).
pub const NODE_OPTIONS: &str = "NODE_OPTIONS";
/// cmux's record of the user's own `NODE_OPTIONS`: `0` = the user had none, `1` = the user had
/// [`CMUX_ORIGINAL_NODE_OPTIONS`].
pub const CMUX_NODE_OPTIONS_PRESENT: &str = "CMUX_ORIGINAL_NODE_OPTIONS_PRESENT";
/// The user's own `NODE_OPTIONS` before cmux rewrote it.
pub const CMUX_ORIGINAL_NODE_OPTIONS: &str = "CMUX_ORIGINAL_NODE_OPTIONS";

/// What becomes of `NODE_OPTIONS` without the host session ([`cmux_node_options`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOptions {
    /// As it is: not cmux's, or cmux did not say what the user had.
    Keep,
    /// cmux's alone: removed.
    Remove,
    /// Back to the user's value.
    Restore(OsString),
}

/// A cmux terminal (seen 2026-09-29) makes every node program load its own module:
/// `NODE_OPTIONS=--require=<cmux file> …`, the user's value (if any) saved in
/// [`CMUX_ORIGINAL_NODE_OPTIONS`] and [`CMUX_NODE_OPTIONS_PRESENT`] saying whether there was
/// one. The `claude` CLI is a node program: an agent started by an app launched from such a
/// terminal would load cmux's module (its channel to the terminal, like `CMUX_*`, spec §7.2).
/// The rule, on the environment `get` reads: marker `0` → remove `NODE_OPTIONS`; `1` with the
/// saved value → restore it; marker absent (or anything else) → leave it untouched.
pub fn cmux_node_options<'a>(get: impl Fn(&str) -> Option<&'a OsStr>) -> NodeOptions {
    match get(CMUX_NODE_OPTIONS_PRESENT).map(OsStr::as_encoded_bytes) {
        Some(b"0") => NodeOptions::Remove,
        Some(b"1") => get(CMUX_ORIGINAL_NODE_OPTIONS)
            .map_or(NodeOptions::Keep, |v| NodeOptions::Restore(v.to_owned())),
        _ => NodeOptions::Keep,
    }
}

/// The environment `env` without a parent Claude Code session and its host: every
/// [`is_nesting_var`] removed and `NODE_OPTIONS` as the user had it before cmux
/// ([`cmux_node_options`], decided before its markers, `CMUX_*` themselves, are removed). Used
/// for every child (agents, `auth status`, git, `open`) and by the shell's re-execution at
/// startup.
pub fn scrub_host_env(env: &mut BTreeMap<OsString, OsString>) {
    let node = cmux_node_options(|k| env.get(OsStr::new(k)).map(OsString::as_os_str));
    env.retain(|k, _| !k.to_str().is_some_and(is_nesting_var));
    match node {
        NodeOptions::Keep => {}
        NodeOptions::Remove => {
            env.remove(OsStr::new(NODE_OPTIONS));
        }
        NodeOptions::Restore(value) => {
            env.insert(NODE_OPTIONS.into(), value);
        }
    }
}

/// Working directory of the probes (`--version`, `auth status`, the login shell): never a
/// repository, whose `.claude`, `.envrc` or the like must not apply outside a turn (M6).
pub const PROBE_CWD: &str = "/";

/// Name of the login script inside the cache dir (spec §7.10).
pub const LOGIN_SCRIPT_NAME: &str = "claude-login.command";

const VERSION_SUFFIX: &str = "(Claude Code)";

/// A CLI that answered `--version` with `<semver> (Claude Code)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// As found (symlink path, never canonicalized: the CLI self-updates).
    pub path: PathBuf,
    pub version: String,
}

/// Environment of every child: `auth status` and agent turns alike (spec §7.2). `Debug`
/// shows only the variable names: values are never logged (spec §10.2).
#[derive(Clone, PartialEq, Eq)]
pub struct ChildEnv {
    vars: BTreeMap<OsString, OsString>,
}

impl std::fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildEnv")
            .field("vars", &self.vars.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ChildEnv {
    /// From `base` (normally `std::env::vars_os()`; a later entry wins): `PATH` := `path`;
    /// `LANG=en_US.UTF-8` if absent; removes `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN`
    /// (unless `allow_env_api_key`), the variables of a parent Claude Code session and of its
    /// host with cmux's `NODE_OPTIONS` ([`scrub_host_env`]) and the scrubbed git variables
    /// (`git::is_scrubbed_git_var`); sets `GIT_EDITOR=true`, `GIT_SEQUENCE_EDITOR=true`,
    /// `GIT_TERMINAL_PROMPT=0`.
    pub fn new(
        base: impl IntoIterator<Item = (OsString, OsString)>,
        path: &OsStr,
        allow_env_api_key: bool,
    ) -> ChildEnv {
        let removed =
            |k: &str| is_scrubbed_git_var(k) || (!allow_env_api_key && API_KEY_VARS.contains(&k));
        let mut vars: BTreeMap<OsString, OsString> = base.into_iter().collect();
        scrub_host_env(&mut vars);
        vars.retain(|k, _| !k.to_str().is_some_and(removed));
        vars.insert("PATH".into(), path.into());
        vars.entry("LANG".into())
            .or_insert_with(|| "en_US.UTF-8".into());
        for (k, v) in [
            ("GIT_EDITOR", "true"),
            ("GIT_SEQUENCE_EDITOR", "true"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ] {
            vars.insert(k.into(), v.into());
        }
        ChildEnv { vars }
    }

    /// Copy for one agent turn: `PWD=<worktree>` (the same string as `current_dir`) and
    /// `ATM_ATTEMPT_ID=<attempt_id>`.
    pub fn for_attempt(&self, worktree: &Path, attempt_id: &str) -> ChildEnv {
        let mut env = self.clone();
        env.vars.insert("PWD".into(), worktree.into());
        env.vars.insert("ATM_ATTEMPT_ID".into(), attempt_id.into());
        env
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(key)).map(OsString::as_os_str)
    }

    /// `env_clear()` then exactly these variables.
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        cmd.env_clear().envs(&self.vars);
    }
}

fn any_var(base: &[(OsString, OsString)], names: &[&str], matches: fn(&OsStr) -> bool) -> bool {
    base.iter()
        .any(|(k, v)| k.to_str().is_some_and(|k| names.contains(&k)) && matches(v))
}

/// `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set in the app's environment.
pub fn api_key_in_env(base: &[(OsString, OsString)]) -> bool {
    any_var(base, API_KEY_VARS, |v| !v.is_empty())
}

/// The environment selects a third-party provider (`CLAUDE_CODE_USE_BEDROCK`, `_VERTEX`, …).
pub fn cloud_provider_env(base: &[(OsString, OsString)]) -> bool {
    any_var(base, CLOUD_PROVIDER_VARS, |v| {
        !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
    })
}

/// The environment sends the CLI's requests to another endpoint ([`BASE_URL_VARS`], not
/// empty).
pub fn base_url_env(base: &[(OsString, OsString)]) -> bool {
    any_var(base, BASE_URL_VARS, |v| !v.is_empty())
}

/// Imports `PATH` once: `$SHELL -ilc 'printf "__ATM__%s__ATM__" "$PATH"'` (stdin null,
/// [`LOGIN_SHELL_TIMEOUT`]), text between the markers; else [`FALLBACK_PATH`] expanded.
pub async fn login_shell_path() -> OsString {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    match shell_path(Path::new(&shell), LOGIN_SHELL_TIMEOUT).await {
        Some(path) => path.into(),
        None => expand_home(FALLBACK_PATH, std::env::var_os("HOME").as_deref()),
    }
}

/// `<shell> -ilc 'printf …'` in a new session, then [`extract_marked_path`]; `None` on
/// failure or after `timeout`.
pub async fn shell_path(shell: &Path, timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(shell);
    cmd.arg("-ilc")
        .arg(format!(r#"printf "{PATH_MARKER}%s{PATH_MARKER}" "$PATH""#))
        .current_dir(PROBE_CWD)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // An interactive shell may grab the controlling terminal's foreground group and leave
    // the app in the background (`cargo tauri dev`); a new session has no terminal.
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .ok()?
        .ok()?;
    extract_marked_path(&String::from_utf8_lossy(&out.stdout))
}

/// Text between the first pair of [`PATH_MARKER`]s (rc-file noise around it is ignored).
pub fn extract_marked_path(output: &str) -> Option<String> {
    let (_, rest) = output.split_once(PATH_MARKER)?;
    let (path, _) = rest.split_once(PATH_MARKER)?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// Replaces the leading `~` of every `:`-separated entry with `home`.
fn expand_home(path: &str, home: Option<&OsStr>) -> OsString {
    let Some(home) = home.map(OsStr::to_string_lossy) else {
        return path.into();
    };
    path.split(':')
        .map(|p| match p.strip_prefix('~') {
            Some(rest) => format!("{home}{rest}"),
            None => p.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(":")
        .into()
}

/// Fixed candidates of spec §7.1 step 3, from `home`.
pub fn fixed_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ]
}

/// The candidates of [`discover`] in order, without running any of them; `PATH` entries are
/// kept only if the file exists. Every candidate is absolute, so a turn spawned in a worktree
/// never resolves `argv[0]` against the repo: a relative override or `ATM_CLAUDE_PATH` is
/// made absolute against the app's cwd; a relative `HOME` and relative or empty `PATH`
/// entries are skipped.
pub fn candidates(override_path: Option<&Path>, env: &ChildEnv) -> Vec<PathBuf> {
    let non_empty = |k| env.get(k).filter(|v| !v.is_empty());
    let absolute = |p: &Path| std::path::absolute(p).ok();
    let mut out: Vec<PathBuf> = override_path.and_then(absolute).into_iter().collect();
    out.extend(non_empty(ATM_CLAUDE_PATH_ENV).and_then(|p| absolute(Path::new(p))));
    if let Some(home) = non_empty("HOME").map(Path::new).filter(|h| h.is_absolute()) {
        out.extend(fixed_candidates(home));
    }
    let tmpdir = non_empty("TMPDIR").map(Path::new);
    if let Some(path) = env.get("PATH") {
        out.extend(
            std::env::split_paths(path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("claude"))
                .filter(|p| p.is_file())
                .filter(|p| !tmpdir.is_some_and(|t| p.starts_with(t)))
                .filter(|p| !p.to_string_lossy().contains("/cmux-cli-shims/")),
        );
    }
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// First valid candidate (spec §7.1): `override_path`, `ATM_CLAUDE_PATH` from `env`,
/// [`fixed_candidates`] from `env`'s `HOME`, then `claude` on `env`'s `PATH` skipping paths
/// under `$TMPDIR` or containing `/cmux-cli-shims/`. Valid = [`probe_version`] succeeds.
///
/// A set `ATM_CLAUDE_PATH` that is not valid ends the search (`None`, "not found"): it points
/// at `fake-claude` in dev and tests, and falling through to the next candidate would run the
/// real CLI on the user's subscription without a word (a slow first `--version` is enough).
pub async fn discover(override_path: Option<&Path>, env: &ChildEnv) -> Option<Discovered> {
    let pinned = env
        .get(ATM_CLAUDE_PATH_ENV)
        .filter(|v| !v.is_empty())
        .and_then(|p| std::path::absolute(Path::new(p)).ok());
    for path in candidates(override_path, env) {
        if path.is_file()
            && let Ok(version) = probe_version(&path, env).await
        {
            return Some(Discovered { path, version });
        }
        if pinned.as_ref() == Some(&path) {
            return None;
        }
    }
    None
}

/// `<claude> --version` with [`VERSION_TIMEOUT`] → version. Errors: `ClaudeNotFound` if it
/// cannot run or the output does not end with `(Claude Code)`.
pub async fn probe_version(claude: &Path, env: &ChildEnv) -> Result<String, AppError> {
    let not_found = |why: &str| {
        AppError::new(
            ErrorCode::ClaudeNotFound,
            format!("{} non è Claude Code: {why}", claude.display()),
        )
    };
    let mut cmd = Command::new(claude);
    cmd.arg("--version")
        .current_dir(PROBE_CWD)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    env.apply(&mut cmd);
    let out = tokio::time::timeout(VERSION_TIMEOUT, cmd.output())
        .await
        .map_err(|_| not_found("timeout di --version"))?
        .map_err(|e| not_found(&e.to_string()))?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| not_found("output di --version inatteso"))
}

/// `"2.1.283 (Claude Code)\n"` → `"2.1.283"`; `None` without the `(Claude Code)` suffix.
pub fn parse_version(output: &str) -> Option<String> {
    let line = output.trim().lines().last()?.trim();
    let version = line
        .strip_suffix(VERSION_SUFFIX)?
        .split_whitespace()
        .next()?;
    Some(version.to_owned())
}

/// Numeric `MAJOR.MINOR.PATCH` comparison (pre-release and build suffixes ignored); `None`
/// if either side does not parse.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    fn parts(v: &str) -> Option<Vec<u64>> {
        let core = v.split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse().ok()).collect()
    }
    Some(parts(a)?.cmp(&parts(b)?))
}

/// Semver comparison with `atm_types::CLAUDE_MIN_VERSION`.
pub fn version_supported(version: &str) -> bool {
    compare_versions(version, CLAUDE_MIN_VERSION).is_some_and(Ordering::is_ge)
}

/// `ClaudeInfo` for the UI (path, version, `supported`, min and tested versions).
pub fn claude_info(found: Option<&Discovered>) -> ClaudeInfo {
    ClaudeInfo {
        path: found.map(|d| d.path.to_string_lossy().into_owned()),
        version: found.map(|d| d.version.clone()),
        supported: found.is_some_and(|d| version_supported(&d.version)),
        min_version: CLAUDE_MIN_VERSION.into(),
        tested_version: CLAUDE_TESTED_VERSION.into(),
    }
}

/// Tools the main agent spawns a sub-agent with (spec F6): `Task` is `Agent`'s older name,
/// which the CLI maps to it. Under a limit they are `ask` rules, so that every spawn reaches
/// the host (`can_use_tool`, whatever the mode), or disallowed once none is left.
pub const SUBAGENT_TOOLS: &[&str] = &["Agent", "Task"];
/// Runs agents from a script without any `Agent` call: disallowed whenever a limit applies.
pub const WORKFLOW_TOOL: &str = "Workflow";
/// Always disallowed: in v1 questions go in the reply's text (spec §7.8).
const ASK_USER_QUESTION: &str = "AskUserQuestion";
/// The in-process MCP server of the board tools (spec §7.3, §7.8): declared by
/// [`MCP_CONFIG`], its JSON-RPC travels on the control protocol as `mcp_message` requests.
pub const MCP_SERVER: &str = "atm";
/// `--mcp-config=` value: an SDK server, answered by the host itself (no process, no socket).
pub const MCP_CONFIG: &str = r#"{"mcpServers":{"atm":{"type":"sdk","name":"atm"}}}"#;
/// Board tools that read or create: `allow` rules, never asked.
pub const BOARD_ALLOW: &[&str] = &[
    "mcp__atm__list_tasks",
    "mcp__atm__get_task",
    "mcp__atm__create_task",
];
/// Board tools that change or start a task: `ask` rules, so that every call reaches the host
/// (`can_use_tool`, whatever the mode) and waits for the user's approval.
pub const BOARD_ASK: &[&str] = &[
    "mcp__atm__update_task",
    "mcp__atm__move_task",
    "mcp__atm__start_task",
];
/// Model of the sub-agents that ask for none, set in the `env` of `--settings` (flag settings
/// win over the user's and the repository's).
pub const SUBAGENT_MODEL_ENV: &str = "CLAUDE_CODE_SUBAGENT_MODEL";

/// Inputs of one turn's argv (spec §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnArgs {
    pub claude: PathBuf,
    pub permission_mode: PermissionMode,
    /// Adds `--allow-dangerously-skip-permissions` (project opt-in).
    pub allow_bypass: bool,
    pub session_id: String,
    /// `--resume=<id>` instead of `--session-id=<id>` (session already started).
    pub resume: bool,
    /// Policy Isolated (or Trusted with a stale fingerprint): `--setting-sources=user
    /// --strict-mcp-config`.
    pub isolated: bool,
    /// `attempts.allow_rules`, the `allow` list of [`settings_json`].
    pub allow_rules: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// Sub-agents the main agent may still spawn in the attempt (spec F6): `None` = no limit,
    /// the argv of an attempt without one; `Some(0)` = [`SUBAGENT_TOOLS`] disallowed; `Some(n)`
    /// = they are `ask` rules, each spawn is counted by the host. [`WORKFLOW_TOOL`] is
    /// disallowed with either `Some`.
    pub subagents_left: Option<u32>,
    /// [`SUBAGENT_MODEL_ENV`] in the `env` of `--settings`.
    pub subagent_model: Option<String>,
    /// The task's attachments folder, canonical, when the task has any (spec F5): `--add-dir=`.
    pub attachments_dir: Option<PathBuf>,
    /// [`append_prompt`], fixed for the attempt.
    pub append_prompt: String,
}

/// Exact argv of spec §7.3, `argv[0]` = the claude path; every value as `--flag=value`,
/// `--permission-mode=` always present, never `--bare` nor the skip-permissions flag.
/// The prompt goes only through stdin. The fixed flags keep their constant value as a
/// separate word, as written in spec §7.3. Defence in depth: `BypassPermissions` without
/// `allow_bypass` (the project opt-in) runs as `default`.
pub fn build_argv(args: &TurnArgs) -> Vec<String> {
    let mut argv = vec![args.claude.to_string_lossy().into_owned()];
    argv.extend(
        [
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
        ]
        .map(String::from),
    );
    let mode = match args.permission_mode {
        PermissionMode::BypassPermissions if !args.allow_bypass => PermissionMode::Default,
        mode => mode,
    };
    argv.push(format!("--permission-mode={mode}"));
    if args.allow_bypass {
        argv.push("--allow-dangerously-skip-permissions".into());
    }
    let session_flag = if args.resume {
        "--resume"
    } else {
        "--session-id"
    };
    argv.push(format!("{session_flag}={}", args.session_id));
    let mut disallowed = vec![ASK_USER_QUESTION];
    let mut ask = BOARD_ASK.to_vec();
    if let Some(left) = args.subagents_left {
        if left == 0 {
            disallowed.extend(SUBAGENT_TOOLS);
        } else {
            ask.extend(SUBAGENT_TOOLS);
        }
        disallowed.push(WORKFLOW_TOOL);
    }
    argv.push(format!("--disallowedTools={}", disallowed.join(",")));
    let env: Vec<(&str, &str)> = args
        .subagent_model
        .iter()
        .map(|model| (SUBAGENT_MODEL_ENV, model.as_str()))
        .collect();
    let allow: Vec<String> = BOARD_ALLOW
        .iter()
        .map(|&tool| tool.to_owned())
        .chain(args.allow_rules.iter().cloned())
        .collect();
    let settings = SettingsParts {
        allow: &allow,
        ask: &ask,
        env: &env,
    };
    argv.push(format!("--settings={}", settings_json(&settings)));
    if args.isolated {
        argv.push("--setting-sources=user".into());
        argv.push("--strict-mcp-config".into());
    }
    if let Some(model) = &args.model {
        argv.push(format!("--model={model}"));
    }
    if let Some(effort) = args.effort {
        argv.push(format!("--effort={effort}"));
    }
    if let Some(dir) = &args.attachments_dir {
        argv.push(format!("--add-dir={}", dir.to_string_lossy()));
    }
    argv.push(format!("--mcp-config={MCP_CONFIG}"));
    argv.push(format!("--append-system-prompt={}", args.append_prompt));
    argv
}

/// What the `--settings` JSON carries besides [`DENY_RULES`] (spec §7.8).
#[derive(Debug, Clone, Copy, Default)]
pub struct SettingsParts<'a> {
    /// `permissions.allow`: [`BOARD_ALLOW`], then `attempts.allow_rules`.
    pub allow: &'a [String],
    /// `permissions.ask`: whole tools each call of which goes to the host, whatever the mode;
    /// left out when empty.
    pub ask: &'a [&'a str],
    /// `env` of the CLI's session, in this order; left out when empty.
    pub env: &'a [(&'a str, &'a str)],
}

/// `{"permissions":{"deny":DENY_RULES,"allow":…[,"ask":…]}[,"env":{…}]}` (spec §7.8): the
/// deny rules always first; without `ask` and `env`, exactly the JSON of spec §7.8.
pub fn settings_json(parts: &SettingsParts<'_>) -> String {
    // Structs rather than `json!`: the key order stays fixed whatever serde_json features
    // the workspace enables.
    #[derive(Serialize)]
    struct Settings<'a> {
        permissions: Permissions<'a>,
        #[serde(skip_serializing_if = "Env::is_empty")]
        env: Env<'a>,
    }
    #[derive(Serialize)]
    struct Permissions<'a> {
        deny: &'a [&'a str],
        allow: &'a [String],
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        ask: &'a [&'a str],
    }
    /// The pairs as one JSON object, in their order.
    struct Env<'a>(&'a [(&'a str, &'a str)]);
    impl Env<'_> {
        fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }
    impl Serialize for Env<'_> {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_map(self.0.iter().copied())
        }
    }
    serde_json::to_string(&Settings {
        permissions: Permissions {
            deny: DENY_RULES,
            allow: parts.allow,
            ask: parts.ask,
        },
        env: Env(parts.env),
    })
    .expect("a struct of strings always serializes")
}

/// `ATM_APPEND` of spec §7.3 for this worktree, branch and target.
pub fn append_prompt(worktree: &Path, branch: &str, target_branch: &str) -> String {
    format!(
        "You are working on a task from AI Task Manager inside a dedicated git worktree `{}` \
         on branch `{branch}` (created from `{target_branch}`). Work only inside this \
         directory. Do not push, switch or delete branches, rewrite history, or change git \
         remotes/config. The host app commits your changes automatically after each turn. \
         The user's later messages continue this task, even once it looks done: carry out \
         what they ask. If a CLAUDE.md or AGENTS.md exists at the repository root, read it \
         first and follow its conventions. \
         The mcp__atm__* tools manage this project's task board. To split your work into \
         subtasks, use mcp__atm__create_task with parent_id \"self\".",
        worktree.display()
    )
}

/// `claude auth status --json` with `env` and [`AUTH_STATUS_TIMEOUT`] in [`PROBE_CWD`] (spec
/// §7.10); never fails: errors become `Unknown`.
pub async fn auth_status(claude: &Path, env: &ChildEnv) -> AuthState {
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "status", "--json"])
        .current_dir(PROBE_CWD)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    env.apply(&mut cmd);
    match tokio::time::timeout(AUTH_STATUS_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => parse_auth_status(out.status.code(), &String::from_utf8_lossy(&out.stdout)),
        Ok(Err(e)) => AuthState::Unknown {
            reason: format!("claude auth status non avviabile: {e}"),
        },
        Err(_) => AuthState::Unknown {
            reason: "claude auth status: timeout".into(),
        },
    }
}

/// Exit 0 → `LoggedIn` from `loggedIn, authMethod, apiProvider, email, orgName,
/// subscriptionType` (or `LoggedOut` if `loggedIn` is false); exit 1 → `LoggedOut`;
/// anything else (`None` = timeout/signal, bad JSON) → `Unknown`.
pub fn parse_auth_status(exit_code: Option<i32>, stdout: &str) -> AuthState {
    // Reasons never quote stdout: it may carry the email.
    let unknown = |reason: String| AuthState::Unknown { reason };
    match exit_code {
        Some(0) => {}
        Some(1) => return AuthState::LoggedOut,
        Some(code) => return unknown(format!("claude auth status: exit {code}")),
        None => return unknown("claude auth status: terminato da un segnale".into()),
    }
    let Ok(v) = serde_json::from_str::<Value>(stdout) else {
        return unknown("claude auth status: JSON non valido".into());
    };
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    match v.get("loggedIn").and_then(Value::as_bool) {
        Some(true) => AuthState::LoggedIn {
            auth_method: field("authMethod"),
            api_provider: field("apiProvider"),
            email: field("email"),
            org_name: field("orgName"),
            subscription_type: field("subscriptionType"),
        },
        Some(false) => AuthState::LoggedOut,
        None => unknown("claude auth status: campo loggedIn assente".into()),
    }
}

/// `'…'` with every `'` written as `'\''`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Content of `claude-login.command` (spec §7.10): the single-quote-escaped absolute claude
/// path, `auth login` plus `--console`/`--sso`, then the "Accesso completato?" lines.
pub fn login_script(claude: &Path, method: LoginMethod) -> String {
    let flag = match method {
        LoginMethod::ClaudeAi => "",
        LoginMethod::Console => " --console",
        LoginMethod::Sso => " --sso",
    };
    format!(
        "#!/bin/sh\n{} auth login{flag}\necho\necho \"Accesso completato? Puoi chiudere questa \
         finestra e tornare ad AI Task Manager.\"\n",
        sh_quote(&claude.to_string_lossy())
    )
}

/// Writes [`login_script`] to `<cache_dir>/claude-login.command` with mode 0700 and returns
/// its path (the caller deletes it when polling ends).
pub fn write_login_script(
    cache_dir: &Path,
    claude: &Path,
    method: LoginMethod,
) -> Result<PathBuf, AppError> {
    std::fs::create_dir_all(cache_dir)?;
    let path = cache_dir.join(LOGIN_SCRIPT_NAME);
    // Always a fresh file, so the mode applies; `create_new` never follows a planted symlink.
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(e.into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)?;
    file.write_all(login_script(claude, method).as_bytes())?;
    Ok(path)
}

/// `open -a Terminal <script>` (argv, no shell, no pipes to the login process).
pub async fn open_login_terminal(script: &Path) -> Result<(), AppError> {
    let status = Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::io(format!(
            "apertura del Terminale non riuscita ({status})"
        )))
    }
}

/// A spawned turn with its pipes taken out of the `Child`.
#[derive(Debug)]
pub struct Spawned {
    pub child: Child,
    /// = pid, since the child leads its own group (`process_group(0)`).
    pub pgid: i32,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

/// `argv[0]` with `argv[1..]`, `current_dir(cwd)`, `process_group(0)`, `kill_on_drop(true)`,
/// all three pipes, environment exactly `env`. Errors: `Io` (→ failed / spawn_error), also
/// for a relative `argv[0]`, which would resolve against `cwd` (the repo).
pub fn spawn(argv: &[String], cwd: &Path, env: &ChildEnv) -> Result<Spawned, AppError> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| AppError::io("argv vuoto"))?;
    if !Path::new(program).is_absolute() {
        return Err(AppError::io(format!(
            "path di claude non assoluto: {program}"
        )));
    }
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.apply(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::io(format!("avvio di {program} non riuscito: {e}")))?;
    let pgid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .ok_or_else(|| AppError::io("processo figlio senza pid"))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(AppError::io("pipe del processo figlio mancanti"));
    };
    Ok(Spawned {
        child,
        pgid,
        stdin,
        stdout,
        stderr,
    })
}

/// `killpg(pgid, signal)`; `ESRCH` (group already gone) is `Ok`. Refuses `pgid <= 1` and the
/// app's own group (`InvalidInput`).
pub fn killpg(pgid: i32, signal: i32) -> std::io::Result<()> {
    // 0 would signal our own group and 1 is launchd: never the group of a turn, which leads
    // its own group (`process_group(0)`).
    // SAFETY: getpgrp has no preconditions and cannot fail.
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid process group {pgid}"),
        ));
    }
    // SAFETY: a plain syscall on integers.
    if unsafe { libc::killpg(pgid, signal) } == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err)
    }
}

/// One row of `ps -A -o pid=,ppid=,pgid=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
}

const PS_ARGS: [&str; 3] = ["-A", "-o", "pid=,ppid=,pgid="];
/// Bound of the async `ps` of [`process_table_async`].
const PS_TIMEOUT: Duration = Duration::from_secs(2);

/// Parses the output of `ps -A -o pid=,ppid=,pgid=`; malformed rows are skipped.
pub fn parse_process_table(output: &str) -> Vec<Proc> {
    output
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(str::parse::<i32>);
            Some(Proc {
                pid: it.next()?.ok()?,
                ppid: it.next()?.ok()?,
                pgid: it.next()?.ok()?,
            })
        })
        .collect()
}

/// Every process of the machine (blocking `ps`); empty if `ps` fails.
pub fn process_table() -> Vec<Proc> {
    std::process::Command::new("/bin/ps")
        .args(PS_ARGS)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|out| parse_process_table(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

/// [`process_table`] without blocking the runtime, bounded by 2 s; empty on failure.
pub async fn process_table_async() -> Vec<Proc> {
    let out = tokio::time::timeout(
        PS_TIMEOUT,
        Command::new("/bin/ps")
            .args(PS_ARGS)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match out {
        Ok(Ok(out)) => parse_process_table(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// The descendants of `pid` (children, grandchildren, …) in `table`, never `pid`, 0 or 1.
pub fn tree_of(table: &[Proc], pid: i32) -> Vec<Proc> {
    if pid <= 1 {
        return Vec::new();
    }
    let mut found: Vec<Proc> = Vec::new();
    let mut frontier = vec![pid];
    while let Some(parent) = frontier.pop() {
        for p in table {
            if p.ppid == parent
                && p.pid > 1
                && p.pid != pid
                && !found.iter().any(|f| f.pid == p.pid)
            {
                found.push(*p);
                frontier.push(p.pid);
            }
        }
    }
    found
}

/// The descendants of `pid` in one `ps` snapshot; empty if `ps` fails. Collect them while `pid`
/// lives: an orphan moves to launchd. M5: the CLI 2.1.283 runs every Bash command in a process
/// group of its own, which a `killpg` of the agent's group never reaches.
pub fn descendants(pid: i32) -> Vec<i32> {
    tree_of(&process_table(), pid)
        .into_iter()
        .map(|p| p.pid)
        .collect()
}

/// The pids of `snapshot` (a [`tree_of`] `root`, taken while `root` lived) that `now` still
/// shows as the same process: same group, and a parent that is `root`, launchd (reparented when
/// `root` exited) or itself in the snapshot. A pid reused since then fails one of the two in
/// practice.
pub fn survivors(root: i32, snapshot: &[Proc], now: &[Proc]) -> Vec<i32> {
    now.iter()
        .filter(|p| {
            snapshot.iter().any(|s| {
                s.pid == p.pid
                    && s.pgid == p.pgid
                    && (p.ppid == 1
                        || p.ppid == root
                        || snapshot.iter().any(|parent| parent.pid == p.ppid))
            })
        })
        .map(|p| p.pid)
        .collect()
}

/// `signal` to each pid of `pids` (never 0, 1 or this process); `ESRCH` ignored.
pub fn kill_all(pids: &[i32], signal: i32) {
    let me = std::process::id() as i32;
    for &pid in pids {
        if pid > 1 && pid != me {
            // SAFETY: a plain syscall on integers.
            unsafe { libc::kill(pid, signal) };
        }
    }
}

/// SIGKILL to the group `pgid` and to every descendant of its leader (collected first).
pub fn kill_tree(pgid: i32) {
    let tree = descendants(pgid);
    let _ = killpg(pgid, libc::SIGKILL);
    kill_all(&tree, libc::SIGKILL);
}

/// `kill(pid, 0)` succeeds.
pub fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence and permission.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// `killpg(pgid, 0)` succeeds: some process of the group still exists (zombies included,
/// so reap the leader first).
pub fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks existence and permission.
    pgid > 1 && unsafe { libc::killpg(pgid, 0) } == 0
}

/// `ps -o command= -p <pid>`, for the verified kill of orphans at startup (spec §7.9).
pub async fn process_command(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let out = tokio::time::timeout(
        VERSION_TIMEOUT,
        Command::new("/bin/ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let command = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !command.is_empty()).then_some(command)
}
