//! The user's Claude Code CLI: discovery, version gate, login-shell PATH, child environment,
//! exact argv, `auth status`, login script, spawn and process-group kill (spec §7.1–§7.3,
//! §7.10). Owner: M2-CLAUDE. Nothing here reads the Keychain or `~/.claude`.

// The OAuth token variable (CLAUDE_CODE_OAUTH_TOKEN) is never set, read, logged or stored by
// the app: if present in the app's environment it simply passes through to the child.
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use atm_types::{AppError, AuthState, ClaudeInfo, Effort, LoginMethod, PermissionMode};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};

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

/// A CLI that answered `--version` with `<semver> (Claude Code)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// As found (symlink path, never canonicalized: the CLI self-updates).
    pub path: PathBuf,
    pub version: String,
}

/// Environment of every child: `auth status` and agent turns alike (spec §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildEnv {
    vars: BTreeMap<OsString, OsString>,
}

impl ChildEnv {
    /// From `base` (normally `std::env::vars_os()`): `PATH` := `path`; `LANG=en_US.UTF-8` if
    /// absent; removes `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` (unless
    /// `allow_env_api_key`), `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT` and the scrubbed git
    /// variables (`git::is_scrubbed_git_var`); sets `GIT_EDITOR=true`,
    /// `GIT_SEQUENCE_EDITOR=true`, `GIT_TERMINAL_PROMPT=0`.
    pub fn new(
        base: impl IntoIterator<Item = (OsString, OsString)>,
        path: &OsStr,
        allow_env_api_key: bool,
    ) -> ChildEnv {
        todo!("M2-CLAUDE: ChildEnv::new")
    }

    /// Copy for one agent turn: `PWD=<worktree>` (the same string as `current_dir`) and
    /// `ATM_ATTEMPT_ID=<attempt_id>`.
    pub fn for_attempt(&self, worktree: &Path, attempt_id: &str) -> ChildEnv {
        todo!("M2-CLAUDE: ChildEnv::for_attempt")
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        todo!("M2-CLAUDE: ChildEnv::get")
    }

    /// `env_clear()` then exactly these variables.
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        todo!("M2-CLAUDE: ChildEnv::apply")
    }
}

/// `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set in the app's environment.
pub fn api_key_in_env(base: &[(OsString, OsString)]) -> bool {
    todo!("M2-CLAUDE: api_key_in_env")
}

/// The environment selects a third-party provider (`CLAUDE_CODE_USE_BEDROCK`, `_VERTEX`, …).
pub fn cloud_provider_env(base: &[(OsString, OsString)]) -> bool {
    todo!("M2-CLAUDE: cloud_provider_env")
}

/// Imports `PATH` once: `$SHELL -ilc 'printf "__ATM__%s__ATM__" "$PATH"'` (stdin null,
/// [`LOGIN_SHELL_TIMEOUT`]), text between the markers; else [`FALLBACK_PATH`] expanded.
pub async fn login_shell_path() -> OsString {
    todo!("M2-CLAUDE: login_shell_path")
}

/// Text between the first pair of [`PATH_MARKER`]s (rc-file noise around it is ignored).
pub fn extract_marked_path(output: &str) -> Option<String> {
    todo!("M2-CLAUDE: extract_marked_path")
}

/// Fixed candidates of spec §7.1 step 3, from `home`.
pub fn fixed_candidates(home: &Path) -> Vec<PathBuf> {
    todo!("M2-CLAUDE: fixed_candidates")
}

/// First valid candidate (spec §7.1): `override_path`, `ATM_CLAUDE_PATH` from `env`,
/// [`fixed_candidates`] from `env`'s `HOME`, then `claude` on `env`'s `PATH` skipping paths
/// under `$TMPDIR` or containing `/cmux-cli-shims/`. Valid = [`probe_version`] succeeds.
pub async fn discover(override_path: Option<&Path>, env: &ChildEnv) -> Option<Discovered> {
    todo!("M2-CLAUDE: discover")
}

/// `<claude> --version` with [`VERSION_TIMEOUT`] → version. Errors: `ClaudeNotFound` if it
/// cannot run or the output does not end with `(Claude Code)`.
pub async fn probe_version(claude: &Path, env: &ChildEnv) -> Result<String, AppError> {
    Err(AppError::not_implemented("claude::probe_version"))
}

/// `"2.1.283 (Claude Code)\n"` → `"2.1.283"`; `None` without the `(Claude Code)` suffix.
pub fn parse_version(output: &str) -> Option<String> {
    todo!("M2-CLAUDE: parse_version")
}

/// Semver comparison with `atm_types::CLAUDE_MIN_VERSION`.
pub fn version_supported(version: &str) -> bool {
    todo!("M2-CLAUDE: version_supported")
}

/// `ClaudeInfo` for the UI (path, version, `supported`, min and tested versions).
pub fn claude_info(found: Option<&Discovered>) -> ClaudeInfo {
    todo!("M2-CLAUDE: claude_info")
}

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
    /// [`append_prompt`], fixed for the attempt.
    pub append_prompt: String,
}

/// Exact argv of spec §7.3, `argv[0]` = the claude path; every value as `--flag=value`,
/// `--permission-mode=` always present, never `--bare` nor the skip-permissions flag.
/// The prompt goes only through stdin.
pub fn build_argv(args: &TurnArgs) -> Vec<String> {
    todo!("M2-CLAUDE: build_argv")
}

/// `{"permissions":{"deny":DENY_RULES,"allow":allow_rules}}` (spec §7.8).
pub fn settings_json(allow_rules: &[String]) -> String {
    todo!("M2-CLAUDE: settings_json")
}

/// `ATM_APPEND` of spec §7.3 for this worktree, branch and target.
pub fn append_prompt(worktree: &Path, branch: &str, target_branch: &str) -> String {
    todo!("M2-CLAUDE: append_prompt")
}

/// `claude auth status --json` with `env` and [`AUTH_STATUS_TIMEOUT`] (spec §7.10); never
/// fails: errors become `Unknown`.
pub async fn auth_status(claude: &Path, env: &ChildEnv) -> AuthState {
    todo!("M2-CLAUDE: auth_status")
}

/// Exit 0 → `LoggedIn` from `loggedIn, authMethod, apiProvider, email, orgName,
/// subscriptionType` (or `LoggedOut` if `loggedIn` is false); exit 1 → `LoggedOut`;
/// anything else (`None` = timeout/signal, bad JSON) → `Unknown`.
pub fn parse_auth_status(exit_code: Option<i32>, stdout: &str) -> AuthState {
    todo!("M2-CLAUDE: parse_auth_status")
}

/// Content of `claude-login.command` (spec §7.10): the single-quote-escaped absolute claude
/// path, `auth login` plus `--console`/`--sso`, then the "Accesso completato?" lines.
pub fn login_script(claude: &Path, method: LoginMethod) -> String {
    todo!("M2-CLAUDE: login_script")
}

/// Writes [`login_script`] to `<cache_dir>/claude-login.command` with mode 0700 and returns
/// its path (the caller deletes it when polling ends).
pub fn write_login_script(
    cache_dir: &Path,
    claude: &Path,
    method: LoginMethod,
) -> Result<PathBuf, AppError> {
    Err(AppError::not_implemented("claude::write_login_script"))
}

/// `open -a Terminal <script>` (argv, no shell, no pipes to the login process).
pub async fn open_login_terminal(script: &Path) -> Result<(), AppError> {
    Err(AppError::not_implemented("claude::open_login_terminal"))
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
/// all three pipes, environment exactly `env`. Errors: `Io` (→ failed / spawn_error).
pub fn spawn(argv: &[String], cwd: &Path, env: &ChildEnv) -> Result<Spawned, AppError> {
    Err(AppError::not_implemented("claude::spawn"))
}

/// `killpg(pgid, signal)`; `ESRCH` (group already gone) is `Ok`.
pub fn killpg(pgid: i32, signal: i32) -> std::io::Result<()> {
    todo!("M2-CLAUDE: killpg")
}

/// `kill(pid, 0)` succeeds.
pub fn pid_alive(pid: i32) -> bool {
    todo!("M2-CLAUDE: pid_alive")
}

/// `ps -o command= -p <pid>`, for the verified kill of orphans at startup (spec §7.9).
pub async fn process_command(pid: i32) -> Option<String> {
    todo!("M2-CLAUDE: process_command")
}
