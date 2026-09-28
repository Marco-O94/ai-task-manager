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
/// Set by a parent Claude Code; removed so that the child never runs as a nested session.
pub const CLAUDE_NESTING_VARS: &[&str] = &["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"];
/// A truthy value of any of these selects a third-party provider.
pub const CLOUD_PROVIDER_VARS: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

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
        let removed = |k: &str| {
            CLAUDE_NESTING_VARS.contains(&k)
                || is_scrubbed_git_var(k)
                || (!allow_env_api_key && API_KEY_VARS.contains(&k))
        };
        let mut vars: BTreeMap<OsString, OsString> = base
            .into_iter()
            .filter(|(k, _)| !k.to_str().is_some_and(removed))
            .collect();
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
/// kept only if the file exists.
pub fn candidates(override_path: Option<&Path>, env: &ChildEnv) -> Vec<PathBuf> {
    let non_empty = |k| env.get(k).filter(|v| !v.is_empty());
    let mut out: Vec<PathBuf> = override_path.map(Path::to_path_buf).into_iter().collect();
    out.extend(non_empty(ATM_CLAUDE_PATH_ENV).map(PathBuf::from));
    if let Some(home) = non_empty("HOME") {
        out.extend(fixed_candidates(Path::new(home)));
    }
    let tmpdir = non_empty("TMPDIR").map(Path::new);
    if let Some(path) = env.get("PATH") {
        out.extend(
            std::env::split_paths(path)
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
pub async fn discover(override_path: Option<&Path>, env: &ChildEnv) -> Option<Discovered> {
    for path in candidates(override_path, env) {
        if !path.is_file() {
            continue;
        }
        if let Ok(version) = probe_version(&path, env).await {
            return Some(Discovered { path, version });
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
/// The prompt goes only through stdin. The fixed flags keep their constant value as a
/// separate word, as written in spec §7.3.
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
    argv.push(format!("--permission-mode={}", args.permission_mode));
    if args.allow_bypass {
        argv.push("--allow-dangerously-skip-permissions".into());
    }
    let session_flag = if args.resume {
        "--resume"
    } else {
        "--session-id"
    };
    argv.push(format!("{session_flag}={}", args.session_id));
    argv.push("--disallowedTools=AskUserQuestion".into());
    argv.push(format!("--settings={}", settings_json(&args.allow_rules)));
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
    argv.push(format!("--append-system-prompt={}", args.append_prompt));
    argv
}

/// `{"permissions":{"deny":DENY_RULES,"allow":allow_rules}}` (spec §7.8).
pub fn settings_json(allow_rules: &[String]) -> String {
    // Structs rather than `json!`: the key order stays fixed whatever serde_json features
    // the workspace enables.
    #[derive(Serialize)]
    struct Settings<'a> {
        permissions: Permissions<'a>,
    }
    #[derive(Serialize)]
    struct Permissions<'a> {
        deny: &'a [&'a str],
        allow: &'a [String],
    }
    serde_json::to_string(&Settings {
        permissions: Permissions {
            deny: DENY_RULES,
            allow: allow_rules,
        },
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
         If a CLAUDE.md or AGENTS.md exists at the repository root, read it first and follow \
         its conventions.",
        worktree.display()
    )
}

/// `claude auth status --json` with `env` and [`AUTH_STATUS_TIMEOUT`] (spec §7.10); never
/// fails: errors become `Unknown`.
pub async fn auth_status(claude: &Path, env: &ChildEnv) -> AuthState {
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "status", "--json"])
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
/// all three pipes, environment exactly `env`. Errors: `Io` (→ failed / spawn_error).
pub fn spawn(argv: &[String], cwd: &Path, env: &ChildEnv) -> Result<Spawned, AppError> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| AppError::io("argv vuoto"))?;
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

/// `killpg(pgid, signal)`; `ESRCH` (group already gone) is `Ok`.
pub fn killpg(pgid: i32, signal: i32) -> std::io::Result<()> {
    // 0 would signal our own group and 1 is launchd: never the group of a turn.
    if pgid <= 1 {
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
