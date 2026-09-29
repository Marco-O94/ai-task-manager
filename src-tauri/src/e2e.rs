//! Debug-only in-app E2E of spec §12.2 (M4), driven by the UI (`ui/src/e2e.rs`) when
//! `ATM_E2E=1` (ignored under `ATM_SELFTEST=1`). `scripts/e2e.sh` launches the app twice with the
//! same `ATM_E2E_DIR`: `ATM_E2E_PHASE=1` runs steps 1–7 and quits during a turn with Cmd+Q
//! ([`debug_e2e_quit`]: a ⌘Q key event posted through the window server → the menu's Quit →
//! `NSApp terminate:` → `RunEvent::Exit`); `ATM_E2E_PHASE=2` relaunches on the same data, runs
//! the rest and exits through `app.exit` (`RunEvent::ExitRequested`) during another turn;
//! `ATM_E2E_PHASE=3` (M6) measures `[fake:flood]` on three concurrent attempts, after phase 2 on
//! the same data or alone on a fresh directory (`--perf`); `gatekeeper` only opens the real
//! login script in Terminal (spec §7.10, `--gatekeeper`).
//!
//! Everything lives under `ATM_E2E_DIR`: data and cache dirs, `HOME` (hence the worktree
//! root), the temporary repositories, fake-claude's record and login state. Agents always run
//! as fake-claude (`ATM_CLAUDE_PATH`, else the `fake-claude` next to this binary, checked by
//! [`verify_fake`] without running it); `open` is recorded instead of run, the native pickers
//! (folder, attachments) return queued paths.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use atm_core::claude::{self, ATM_CLAUDE_PATH_ENV};
use atm_core::{CoreConfig, git::is_scrubbed_git_var};
use atm_types::debug::{
    E2eAuthReq, E2eConfirmReq, E2eGatekeeper, E2eGitOut, E2eGitReq, E2eLoginScript, E2ePathReq,
    E2eSetup, E2eWriteReq, ReportReq,
};
use atm_types::{AppError, LoginMethod};
use serde_json::Value;
use tauri::{AppHandle, Manager};

/// Instead of the login shell's: a run must not depend on the user's `.zshrc`.
const E2E_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// A phase that has not reported by then exits 1 with its own message: shorter than the 300 s
/// `scripts/e2e.sh` gives each phase before killing it (a phase takes under a minute).
const WATCHDOG: Duration = Duration::from_secs(240);
/// Seconds of `[fake:slow]`: long enough to stop it halfway.
const SLOW_EVENTS: &str = "30";
/// Pause between the text deltas of a streamed text: the UI's progressive rendering is seen.
const DELTA_MS: &str = "300";
/// Assistant texts of one `[fake:flood]` turn (phase 3; fake-claude's default, made explicit),
/// with a pause after every 100: ~1000 texts per second per agent, so that the three floods,
/// started one after the other from the UI, stream together for several seconds.
const FLOOD_EVENTS: &str = "10000";
const FLOOD_PAUSE_MS: &str = "100";
/// Folder of `app_cache_dir` for the Gatekeeper check: a real login script is never touched.
const GATEKEEPER_DIR: &str = "e2e-gatekeeper";
const GATEKEEPER_WAIT: Duration = Duration::from_secs(30);
/// From Cmd+Q to the end of the process: the shutdown takes at most 10 s.
const QUIT_WAIT: Duration = Duration::from_secs(30);

/// Only fake-claude's binary contains it (its `auth login` line): a real CLI renamed or linked
/// as `fake-claude` does not.
const FAKE_MARKER: &[u8] = b"fake-claude: login simulato";
/// The variables fake-claude records the presence of (`RECORDED_VARS` in fake-claude.rs) that
/// no agent may get, a parent Claude Code session's and its host's included (M6), with cmux's
/// `NODE_OPTIONS` (its marker says the user had none).
const RECORDED_VARS: [&str; 18] = [
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
];
/// What `debug_e2e_git` may run: the UI's read-only checks.
const GIT_SUBCOMMANDS: [&str; 5] = ["show", "log", "status", "branch", "worktree"];
/// Options `debug_e2e_git` accepts after the subcommand (a trailing `=` takes a value).
const GIT_OPTIONS: [&str; 5] = ["-1", "--name-only", "--porcelain", "--list", "--format="];
/// The `mcp` repository's committed `.mcp.json`: one server whose env value the overview must
/// never show (only its key), as the UI checks (`MCP_SERVER`, `MCP_SECRET` in ui/src/e2e.rs).
const MCP_JSON: &str = r#"{
  "mcpServers": {
    "e2e-tools": {
      "command": "e2e-mcp-server",
      "args": ["--stdio"],
      "env": { "TOKEN": "secret-value" }
    }
  }
}
"#;
/// The `mcp` repository's committed CLAUDE.md, shown by its overview.
const MCP_CLAUDE_MD: &str = "# Istruzioni E2E\n\nLavora solo nel worktree del task.\n";
/// Repository the UI adds in phase 2 to try attachments and sub-agent limits on, then removes
/// from the sidebar menu (`SCRATCH` in ui/src/e2e.rs).
const SCRATCH_REPO: &str = "da-rimuovere";
/// The file the UI attaches to a task (`ATTACHMENT` in ui/src/e2e.rs): `<dir>/attach/<name>`,
/// outside every folder the core refuses to copy from (the run's `HOME/.claude*`, `.ssh`,
/// `.aws`, `Library/Keychains`, its data and cache dirs).
const ATTACHMENT: &str = "specifiche-e2e.txt";

/// fake-claude, checked once ([`verify_fake`]).
static FAKE: OnceLock<PathBuf> = OnceLock::new();
/// Path returned by the next native picker, `pick_repo_folder` or `pick_attachment_files`
/// (queued by `debug_e2e_queue_pick`).
static PICK: Mutex<Option<String>> = Mutex::new(None);
/// Answers of the next native confirmations (queued by `debug_e2e_queue_confirm`), in order.
static CONFIRM: Mutex<VecDeque<bool>> = Mutex::new(VecDeque::new());
/// `"<title>: <text>"` of every confirmation asked ([`take_confirm`]).
static CONFIRMS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// `"<command>: <code>"` of every failed IPC command ([`note_failure`]).
static FAILURES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// `ATM_E2E=1`, unless the selftest runs: it has its own core config, and the E2E's debug
/// commands would work on a directory that core does not use.
pub fn enabled() -> bool {
    std::env::var("ATM_E2E").as_deref() == Ok("1") && !crate::selftest::selftest_enabled()
}

fn phase() -> String {
    std::env::var("ATM_E2E_PHASE").unwrap_or_else(|_| "1".into())
}

/// Ends a run that cannot go on: exit code 1.
fn fail(why: &str) -> ! {
    eprintln!("e2e: {why}");
    std::process::exit(1)
}

/// The run's files, all under `ATM_E2E_DIR`.
struct Paths {
    dir: PathBuf,
    repos: PathBuf,
    home: PathBuf,
    auth: PathBuf,
    record: PathBuf,
    open_log: PathBuf,
    phase1: PathBuf,
    fake: PathBuf,
}

impl Paths {
    fn get() -> Paths {
        let dir = std::env::var_os("ATM_E2E_DIR")
            .map(PathBuf::from)
            .and_then(|d| d.canonicalize().ok())
            .unwrap_or_else(|| fail("ATM_E2E_DIR is not an existing directory"));
        let fake = FAKE
            .get_or_init(|| {
                let given = std::env::var_os(ATM_CLAUDE_PATH_ENV)
                    .map(PathBuf::from)
                    .or_else(|| Some(std::env::current_exe().ok()?.parent()?.join("fake-claude")))
                    .unwrap_or_else(|| fail("no ATM_CLAUDE_PATH and no fake-claude"));
                verify_fake(&given).unwrap_or_else(|e| {
                    fail(&format!(
                        "{e}: the E2E runs only fake-claude \
                         (`cargo build -p atm-core --bin fake-claude`)"
                    ))
                })
            })
            .clone();
        Paths {
            repos: dir.join("repos"),
            home: dir.join("home"),
            auth: dir.join("auth"),
            record: dir.join("record.jsonl"),
            open_log: dir.join("open.jsonl"),
            phase1: dir.join("phase1.json"),
            fake,
            dir,
        }
    }

    fn repo(&self, name: &str) -> PathBuf {
        self.repos.join(name)
    }

    /// `path` exists and lies inside the run's directory (the debug commands touch nothing
    /// else). Errors: `Invalid`.
    async fn inside(&self, path: &str) -> Result<PathBuf, AppError> {
        let path = Path::new(path);
        let canonical = match tokio::fs::canonicalize(path).await {
            Ok(p) => Ok(p),
            // A dangling symlink would pass the check on its directory, then be written
            // through to wherever it points.
            Err(_) if tokio::fs::symlink_metadata(path).await.is_ok() => {
                return Err(AppError::invalid(format!(
                    "{} is a dangling symlink",
                    path.display()
                )));
            }
            // A file about to be written: its directory must exist.
            Err(e) => match path.parent() {
                Some(parent) => tokio::fs::canonicalize(parent)
                    .await
                    .map(|p| p.join(path.file_name().unwrap_or_default())),
                None => Err(e),
            },
        }
        .map_err(|e| AppError::invalid(format!("{}: {e}", path.display())))?;
        if canonical.starts_with(&self.dir) {
            Ok(canonical)
        } else {
            Err(AppError::invalid(format!(
                "{} is outside the E2E directory",
                path.display()
            )))
        }
    }
}

/// `path` is fake-claude and not the real CLI under its name: resolved through symlinks, it is
/// a file named `fake-claude` whose binary carries [`FAKE_MARKER`] (read, never run). Returns
/// the resolved path.
fn verify_fake(path: &Path) -> Result<PathBuf, String> {
    let real = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !real.is_file() || real.file_name().is_none_or(|n| n != "fake-claude") {
        return Err(format!(
            "{} is not a file named fake-claude",
            real.display()
        ));
    }
    let bytes = std::fs::read(&real).map_err(|e| format!("{}: {e}", real.display()))?;
    if !bytes.windows(FAKE_MARKER.len()).any(|w| w == FAKE_MARKER) {
        return Err(format!("{} is not fake-claude's binary", real.display()));
    }
    Ok(real)
}

/// `args` are one of the UI's read-only checks: an allowed subcommand first (no option before
/// it: no `-c`, `-C`, `--git-dir`…), `worktree` only as `worktree list`, `branch` only with
/// `--list`, and after it only [`GIT_OPTIONS`] among the options. Errors: `Invalid`.
fn check_git_args(args: &[String]) -> Result<(), AppError> {
    let refuse = || AppError::invalid(format!("git {args:?} is not an E2E check"));
    let (sub, rest) = args.split_first().ok_or_else(refuse)?;
    let option_ok = |a: &str| {
        GIT_OPTIONS.iter().any(|o| match o.strip_suffix('=') {
            Some(_) => a.starts_with(o),
            None => a == *o,
        })
    };
    let ok = GIT_SUBCOMMANDS.contains(&sub.as_str())
        && (sub != "worktree" || rest.first().is_some_and(|a| a == "list"))
        && (sub != "branch" || rest.iter().any(|a| a == "--list"))
        && rest.iter().all(|a| !a.starts_with('-') || option_ok(a));
    if ok { Ok(()) } else { Err(refuse()) }
}

/// Data dir, cache dir, fake-claude and the child environment of a run. Phase 1 first creates
/// the repositories and logs fake-claude out (step 1 starts at the login gate); phase 3 alone
/// (`--perf`, no login state yet) creates them and logs it in.
pub fn core_config() -> CoreConfig {
    let p = Paths::get();
    for dir in [&p.home, &p.repos] {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(&format!("{}: {e}", dir.display())));
    }
    let fresh_perf = phase() == "3" && !p.auth.exists();
    if phase() == "1" || fresh_perf {
        if let Err(e) = create_repos(&p) {
            fail(&format!("repositories not created: {e}"));
        }
        write_auth(&p, fresh_perf).unwrap_or_else(|e| fail(&e.message));
    }
    CoreConfig {
        data_dir: p.dir.join("data"),
        cache_dir: p.dir.join("cache"),
        claude_path: Some(p.fake.clone()),
        path_env: Some(E2E_PATH.into()),
        extra_env: vec![
            (ATM_CLAUDE_PATH_ENV.into(), p.fake.clone().into()),
            ("HOME".into(), p.home.clone().into()),
            ("FAKE_CLAUDE_RECORD".into(), p.record.clone().into()),
            ("FAKE_CLAUDE_AUTH_FILE".into(), p.auth.clone().into()),
            // No `FAKE_CLAUDE_TARGET`: `resolve_merge` takes the target from the app's prompt.
            ("FAKE_CLAUDE_SLOW_EVENTS".into(), SLOW_EVENTS.into()),
            ("FAKE_CLAUDE_DELTA_MS".into(), DELTA_MS.into()),
            ("FAKE_CLAUDE_FLOOD_EVENTS".into(), FLOOD_EVENTS.into()),
            ("FAKE_CLAUDE_FLOOD_PAUSE_MS".into(), FLOOD_PAUSE_MS.into()),
        ],
        open_log: Some(p.open_log.clone()),
    }
}

/// Fails a phase that never reports (e.g. the WASM did not load).
pub fn start_watchdog() {
    if enabled() {
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            fail(&format!(
                "phase {} did not report within {WATCHDOG:?} (a UI built without \
                 `--features testkit` has no E2E driver: build with \
                 `--config src-tauri/tauri.testkit.conf.json`)",
                phase()
            ));
        });
    }
}

/// `Some(queued path)` in a run: `pick_repo_folder` and `pick_attachment_files` must not open
/// the native picker.
pub fn take_pick() -> Option<Option<String>> {
    enabled().then(|| PICK.lock().unwrap_or_else(|e| e.into_inner()).take())
}

/// In an E2E run, the queued answer of a native confirmation instead of the dialog, which the
/// run cannot click (like the folder picker): recorded with its text, and Annulla when nothing
/// is queued, so a confirmation the run did not expect never waits for a click. `None` outside
/// an E2E run.
pub fn take_confirm(title: &str, message: &str) -> Option<bool> {
    if !enabled() {
        return None;
    }
    CONFIRMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(format!("{title}: {message}"));
    let answer = CONFIRM
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop_front();
    Some(answer.unwrap_or(false))
}

/// Called by every command wrapper on failure: a run checks that only the failures it
/// provokes happen (e.g. no `get_diff` on a merged attempt).
pub fn note_failure(command: &str, e: &AppError) {
    if enabled() {
        let line = format!("{command}: {:?}", e.code);
        FAILURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line);
    }
}

/// `git` with the run's `HOME` (no global config of the user's), hooks off.
fn git(p: &Paths, dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new(atm_core::git::find_git(E2E_PATH.as_ref()));
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(is_scrubbed_git_var) {
            cmd.env_remove(key);
        }
    }
    cmd.args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(dir)
        .env("PATH", E2E_PATH)
        .env("HOME", &p.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
}

fn git_ok(p: &Paths, dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = git(p, dir, args).map_err(|e| format!("git {args:?}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// `repos/{main, mcp, da-rimuovere}` (one commit on `main`, a local identity; `mcp` commits a
/// CLAUDE.md and a `.mcp.json` server with an env value, `main` only its README), `repos/not-git`
/// (a plain folder), `repos/bare.git` (a mirror of `main`), `repos/empty` (no commits), all kept
/// if they exist, and the file to attach, `attach/specifiche-e2e.txt`.
fn create_repos(p: &Paths) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let committed = |name: &str, files: &[(&str, &str)]| -> Result<(), String> {
        let dir = p.repo(name);
        if dir.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&dir).map_err(io)?;
        git_ok(p, &dir, &["init", "-q", "-b", "main"])?;
        for (key, value) in [
            ("user.name", "ATM E2E"),
            ("user.email", "e2e@localhost"),
            ("commit.gpgsign", "false"),
        ] {
            git_ok(p, &dir, &["config", key, value])?;
        }
        for (file, content) in files {
            std::fs::write(dir.join(file), content).map_err(io)?;
        }
        git_ok(p, &dir, &["add", "-A"])?;
        git_ok(p, &dir, &["commit", "-q", "-m", "initial"])
    };
    committed("main", &[("README.md", "# Progetto E2E\n")])?;
    committed(
        "mcp",
        &[
            ("README.md", "# MCP\n"),
            ("CLAUDE.md", MCP_CLAUDE_MD),
            (".mcp.json", MCP_JSON),
        ],
    )?;
    committed(SCRATCH_REPO, &[("README.md", "# Da rimuovere\n")])?;
    let attach = p.dir.join("attach");
    std::fs::create_dir_all(&attach).map_err(io)?;
    std::fs::write(
        attach.join(ATTACHMENT),
        "Specifiche E2E: l'agente legge questo file dalla cartella degli allegati.\n",
    )
    .map_err(io)?;
    let not_git = p.repo("not-git");
    std::fs::create_dir_all(&not_git).map_err(io)?;
    std::fs::write(not_git.join("notes.txt"), "not a repository\n").map_err(io)?;
    let empty = p.repo("empty");
    if !empty.exists() {
        std::fs::create_dir_all(&empty).map_err(io)?;
        git_ok(p, &empty, &["init", "-q", "-b", "main"])?;
    }
    // A mirror clone is a bare repository (with commits: only its bareness is rejected).
    if !p.repo("bare.git").exists() {
        git_ok(
            p,
            &p.repos,
            &["clone", "-q", "--mirror", "main", "bare.git"],
        )?;
    }
    Ok(())
}

fn auth_state(logged_in: bool) -> &'static str {
    if logged_in { "in" } else { "out" }
}

/// Written to a temporary file renamed over the state file: fake-claude's `auth status`, which
/// the core may run at any time, reads the old state or the new one, never an empty file.
fn write_auth(p: &Paths, logged_in: bool) -> Result<(), AppError> {
    let tmp = p.auth.with_extension("tmp");
    std::fs::write(&tmp, auth_state(logged_in))
        .and_then(|()| std::fs::rename(&tmp, &p.auth))
        .map_err(|e| AppError::io(format!("{}: {e}", p.auth.display())))
}

fn disabled() -> AppError {
    AppError::invalid("E2E disabled (ATM_E2E=1 not set)")
}

fn paths() -> Result<Paths, AppError> {
    if enabled() {
        Ok(Paths::get())
    } else {
        Err(disabled())
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

async fn json_lines(path: &Path) -> Vec<Value> {
    tokio::fs::read_to_string(path)
        .await
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// `'…'` for `/bin/sh`.
fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// The run's setup. Phase 3 also floats the window above the others, so that its
/// responsiveness measure includes painting: a covered window's page is "hidden" and not
/// painted (its timers still run: the testkit window has `backgroundThrottling: disabled`).
#[tauri::command]
pub async fn debug_e2e_setup(app: AppHandle) -> Result<Option<E2eSetup>, AppError> {
    if !enabled() {
        return Ok(None);
    }
    if phase() == "3"
        && let Some(window) = app.get_webview_window("main")
        && let Err(e) = window.set_always_on_top(true)
    {
        eprintln!("e2e: always on top: {e}");
    }
    let p = Paths::get();
    let path = |name: &str| p.repo(name).display().to_string();
    let phase1 = tokio::fs::read_to_string(&p.phase1)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    Ok(Some(E2eSetup {
        phase: phase(),
        dir: p.dir.display().to_string(),
        repo: path("main"),
        not_git: path("not-git"),
        bare: path("bare.git"),
        empty: path("empty"),
        mcp_repo: path("mcp"),
        fake_claude: p.fake.display().to_string(),
        app_env: RECORDED_VARS
            .iter()
            .filter(|k| std::env::var_os(k).is_some())
            .map(|k| (*k).to_owned())
            .collect(),
        scrubbed_at_start: std::env::var(crate::SCRUBBED_AT_START_ENV)
            .unwrap_or_default()
            .split(',')
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect(),
        phase1,
    }))
}

#[tauri::command]
pub async fn debug_e2e_set_auth(req: E2eAuthReq) -> Result<(), AppError> {
    let p = paths()?;
    tauri::async_runtime::spawn_blocking(move || write_auth(&p, req.logged_in))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn debug_e2e_queue_pick(req: E2ePathReq) -> Result<(), AppError> {
    paths()?;
    *PICK.lock().unwrap_or_else(|e| e.into_inner()) = Some(req.path);
    Ok(())
}

#[tauri::command]
pub async fn debug_e2e_queue_confirm(req: E2eConfirmReq) -> Result<(), AppError> {
    paths()?;
    CONFIRM
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(req.accept);
    Ok(())
}

#[tauri::command]
pub async fn debug_e2e_confirms() -> Result<Vec<String>, AppError> {
    paths()?;
    Ok(CONFIRMS.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// The login script as `open_login_terminal` left it, the recorded `open` calls, and the
/// script run with `/bin/sh` as Terminal would (it must reach fake-claude's `auth login`).
#[tauri::command]
pub async fn debug_e2e_login_script() -> Result<E2eLoginScript, AppError> {
    let p = paths()?;
    let path = p.dir.join("cache").join(claude::LOGIN_SCRIPT_NAME);
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
    let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o7777;
    let opened = json_lines(&p.open_log)
        .await
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    let run = tokio::process::Command::new("/bin/sh")
        .arg(&path)
        .env("PATH", E2E_PATH)
        .env("HOME", &p.home)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let (run_code, run_output) = match tokio::time::timeout(Duration::from_secs(10), run).await {
        Ok(Ok(out)) => (out.status.code(), lossy(&[out.stdout, out.stderr].concat())),
        Ok(Err(e)) => (None, e.to_string()),
        Err(_) => (None, "timeout".into()),
    };
    Ok(E2eLoginScript {
        path: path.display().to_string(),
        content,
        mode,
        opened,
        run_code,
        run_output,
    })
}

#[tauri::command]
pub async fn debug_e2e_record() -> Result<Vec<Value>, AppError> {
    Ok(json_lines(&paths()?.record).await)
}

#[tauri::command]
pub async fn debug_e2e_git(req: E2eGitReq) -> Result<E2eGitOut, AppError> {
    let p = paths()?;
    let repo = p.inside(&req.repo).await?;
    check_git_args(&req.args)?;
    let args = req.args;
    let out = tauri::async_runtime::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(&p, &repo, &args)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    Ok(E2eGitOut {
        code: out.status.code().unwrap_or(-1),
        stdout: lossy(&out.stdout),
        stderr: lossy(&out.stderr),
    })
}

#[tauri::command]
pub async fn debug_e2e_write_file(req: E2eWriteReq) -> Result<(), AppError> {
    let path = paths()?.inside(&req.path).await?;
    Ok(tokio::fs::write(path, req.content).await?)
}

#[tauri::command]
pub async fn debug_e2e_exists(req: E2ePathReq) -> Result<bool, AppError> {
    let p = paths()?;
    let path = Path::new(&req.path);
    // Checked on the parent: a removed worktree has no canonical path of its own.
    p.inside(&path.parent().unwrap_or(path).display().to_string())
        .await?;
    Ok(tokio::fs::try_exists(path).await?)
}

#[tauri::command]
pub async fn debug_e2e_agents() -> Result<Vec<i32>, AppError> {
    live_agents(&paths()?).await
}

/// The run's agents still alive, recorded or not: the pids fake-claude recorded in this run's
/// record (its `-p` calls and the `sleep` grandchildren of `hang_ignore`) that `ps` still shows
/// running the same program (a pid can be reused), plus every fake-claude `-p` and `sleep 300`
/// on the machine whose working directory is inside the run's directory (the agents run in
/// worktrees under its `HOME`), found with `lsof`. Other fake-claude processes (a concurrent
/// `cargo test`, another app instance), the app's own git commands and the short `--version` /
/// `auth status` probes are never counted.
async fn live_agents(p: &Paths) -> Result<Vec<i32>, AppError> {
    let recorded: HashMap<u64, bool> = json_lines(&p.record)
        .await
        .iter()
        .filter_map(|l| {
            let pid = l["pid"].as_u64()?;
            match l["kind"].as_str()? {
                "call" => Some((pid, false)),
                "grandchild" => Some((pid, true)),
                _ => None,
            }
        })
        .collect();
    let in_dir = pids_in_dir(&p.dir).await?;
    let candidates: BTreeSet<u64> = recorded.keys().chain(&in_dir).copied().collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<String> = candidates.iter().map(u64::to_string).collect();
    // Exits 1 when some pid is gone: only its output matters.
    let out = tokio::process::Command::new("/bin/ps")
        .args(["-o", "pid=,command=", "-p", &list.join(",")])
        .stdin(Stdio::null())
        .output()
        .await?;
    let agent = format!("{} -p", p.fake.display());
    Ok(lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            let pid: u64 = pid.parse().ok()?;
            let command = command.trim();
            let alive = match recorded.get(&pid) {
                Some(false) => command.starts_with(&p.fake.display().to_string()),
                Some(true) => command == "sleep 300",
                None => command.starts_with(&agent) || command == "sleep 300",
            };
            alive.then_some(pid as i32)
        })
        .collect())
}

/// Pids of the processes of this user whose working directory lies inside `dir`.
async fn pids_in_dir(dir: &Path) -> Result<Vec<u64>, AppError> {
    // Exits 1 when some process could not be read: only its output matters.
    let out = tokio::process::Command::new("/usr/sbin/lsof")
        .args(["-w", "-d", "cwd", "-F", "pn"])
        .stdin(Stdio::null())
        .output()
        .await?;
    let mut pids = Vec::new();
    let mut pid = None;
    for line in lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let (Some(name), Some(p)) = (line.strip_prefix('n'), pid)
            && Path::new(name).starts_with(dir)
        {
            pids.push(p);
        }
    }
    Ok(pids)
}

#[tauri::command]
pub async fn debug_e2e_failures() -> Result<Vec<String>, AppError> {
    paths()?;
    Ok(FAILURES.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// Hands the partial report to phase 2 (with how ⌘Q is delivered in its `details.cmd_q`), then
/// presses Cmd+Q ([`press_cmd_q`]): the Quit item of the app menu (Tauri's default menu) sends
/// `terminate:` to `NSApp`; tao has no `applicationShouldTerminate:`, so AppKit goes straight
/// to `applicationWillTerminate:` → tao's `LoopDestroyed` → `RunEvent::Exit`, never
/// `ExitRequested`: `on_run_event` then runs `Core::shutdown` on the main thread before the
/// process exits with 0. (`app.exit` would take the other, `ExitRequested`, branch: phase 2
/// exits that way.)
#[tauri::command]
pub async fn debug_e2e_quit(app: AppHandle, req: ReportReq) -> Result<(), AppError> {
    let p = paths()?;
    let route = cmd_q_route();
    let mut report = req.report;
    if let Some(details) = report.get_mut("details").and_then(Value::as_object_mut) {
        details.insert("cmd_q".into(), route.describe().into());
    }
    tokio::fs::write(&p.phase1, report.to_string()).await?;
    eprintln!("e2e: phase 1 done, pressing Cmd+Q ({})", route.describe());
    press_cmd_q(&app, route)?;
    // A Cmd+Q that does not quit (no ⌘Q item in the menu, the page swallowing the key) fails
    // the phase now rather than at the watchdog. The shutdown itself ends well before.
    std::thread::spawn(|| {
        std::thread::sleep(QUIT_WAIT);
        fail(&format!("Cmd+Q did not quit the app within {QUIT_WAIT:?}"));
    });
    Ok(())
}

/// How [`press_cmd_q`] delivers ⌘Q.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmdQ {
    /// Key-down and key-up `q` with ⌘ as `CGEvent`s posted to this process through the window
    /// server (`CGEventPostToPid`), as a keyboard's reach the app: the terminal that started
    /// the run has the event-posting (Accessibility) access.
    WindowServer,
    /// Without that access (`CGPreflightPostEventAccess` false): the same key-down as an
    /// `NSEvent` handed to `-[NSApplication sendEvent:]` inside the app.
    SendEvent,
}

impl CmdQ {
    fn describe(self) -> &'static str {
        match self {
            CmdQ::WindowServer => "⌘Q CGEvent posted to the app's pid through the window server",
            CmdQ::SendEvent => {
                "⌘Q NSEvent given to -[NSApp sendEvent:] (no event-posting access: \
                 CGPreflightPostEventAccess false)"
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn cmd_q_route() -> CmdQ {
    // SAFETY: a plain query of this process's TCC access; never prompts.
    if unsafe { cocoa::CGPreflightPostEventAccess() } {
        CmdQ::WindowServer
    } else {
        CmdQ::SendEvent
    }
}

#[cfg(not(target_os = "macos"))]
fn cmd_q_route() -> CmdQ {
    CmdQ::SendEvent
}

/// Presses ⌘Q the `route` way. Either way AppKit offers the key-down as a key equivalent to
/// the key window (the WKWebView passes it to the page, which does not handle it) and then to
/// the main menu, whose Quit item has ⌘Q. Errors: `Internal` (no main window).
#[cfg(target_os = "macos")]
fn press_cmd_q(app: &AppHandle, route: CmdQ) -> Result<(), AppError> {
    if route == CmdQ::WindowServer {
        // SAFETY: CoreGraphics event calls, valid from any thread.
        unsafe { cocoa::post_cmd_q_to_self() };
        return Ok(());
    }
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| AppError::internal("no main window"))?;
    app.run_on_main_thread(move || {
        let ns_window = window.ns_window().unwrap_or(std::ptr::null_mut());
        // SAFETY: on the main thread, with AppKit running (the window is up).
        unsafe { cocoa::post_cmd_q(ns_window) }
    })
    .map_err(|e| AppError::internal(e.to_string()))
}

#[cfg(not(target_os = "macos"))]
fn press_cmd_q(app: &AppHandle, _route: CmdQ) -> Result<(), AppError> {
    app.exit(0);
    Ok(())
}

/// The CoreGraphics calls and the few Objective-C messages of [`press_cmd_q`], the latter sent
/// through `objc_msgSend` cast to each method's exact prototype.
#[cfg(target_os = "macos")]
mod cocoa {
    use std::ffi::{CStr, c_char, c_void};

    type Id = *mut c_void;
    type Sel = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NsPoint {
        x: f64,
        y: f64,
    }

    const NS_EVENT_TYPE_KEY_DOWN: u64 = 10;
    const NS_EVENT_MODIFIER_FLAG_COMMAND: u64 = 1 << 20;
    /// `kVK_ANSI_Q`.
    const KEY_CODE_Q: u16 = 12;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSApp: Id;
    }
    /// `kCGEventSourceStateHIDSystemState`: the state of the hardware keyboard.
    const HID_SYSTEM_STATE: i32 = 1;
    /// `kCGEventFlagMaskCommand`.
    const CG_FLAG_COMMAND: u64 = 0x0010_0000;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGPreflightPostEventAccess() -> bool;
        fn CGEventSourceCreate(state: i32) -> Id;
        fn CGEventCreateKeyboardEvent(source: Id, key: u16, down: bool) -> Id;
        fn CGEventSetFlags(event: Id, flags: u64);
        fn CGEventPostToPid(pid: i32, event: Id);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: Id);
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }

    unsafe fn sel(name: &CStr) -> Sel {
        unsafe { sel_registerName(name.as_ptr()) }
    }

    unsafe fn class(name: &CStr) -> Id {
        unsafe { objc_getClass(name.as_ptr()) }
    }

    /// Posts ⌘Q (key-down, key-up) to this process through the window server.
    ///
    /// # Safety
    /// Only CoreGraphics calls, each object released once.
    pub unsafe fn post_cmd_q_to_self() {
        let pid = std::process::id() as i32;
        unsafe {
            let source = CGEventSourceCreate(HID_SYSTEM_STATE);
            for down in [true, false] {
                let event = CGEventCreateKeyboardEvent(source, KEY_CODE_Q, down);
                if event.is_null() {
                    continue;
                }
                CGEventSetFlags(event, CG_FLAG_COMMAND);
                CGEventPostToPid(pid, event);
                CFRelease(event);
            }
            if !source.is_null() {
                CFRelease(source);
            }
        }
    }

    /// Queues `[NSApp sendEvent:<⌘Q key-down>]` on the main run loop.
    ///
    /// # Safety
    /// Main thread, AppKit running; `ns_window` is an `NSWindow` or null.
    pub unsafe fn post_cmd_q(ns_window: *mut c_void) {
        type GetId = unsafe extern "C" fn(Id, Sel) -> Id;
        type GetF64 = unsafe extern "C" fn(Id, Sel) -> f64;
        type GetIsize = unsafe extern "C" fn(Id, Sel) -> isize;
        type StringWithUtf8 = unsafe extern "C" fn(Id, Sel, *const c_char) -> Id;
        type KeyEvent = unsafe extern "C" fn(
            Id,
            Sel,
            u64,
            NsPoint,
            u64,
            f64,
            isize,
            Id,
            Id,
            Id,
            bool,
            u16,
        ) -> Id;
        type Perform = unsafe extern "C" fn(Id, Sel, Sel, Id, bool);
        let send = objc_msgSend as unsafe extern "C" fn();
        // SAFETY: every call goes through the exact prototype of the method it sends.
        unsafe {
            let get_id = std::mem::transmute::<unsafe extern "C" fn(), GetId>(send);
            let get_f64 = std::mem::transmute::<unsafe extern "C" fn(), GetF64>(send);
            let get_isize = std::mem::transmute::<unsafe extern "C" fn(), GetIsize>(send);
            let string = std::mem::transmute::<unsafe extern "C" fn(), StringWithUtf8>(send);
            let key_event = std::mem::transmute::<unsafe extern "C" fn(), KeyEvent>(send);
            let perform = std::mem::transmute::<unsafe extern "C" fn(), Perform>(send);

            let q = string(
                class(c"NSString"),
                sel(c"stringWithUTF8String:"),
                c"q".as_ptr(),
            );
            let process = get_id(class(c"NSProcessInfo"), sel(c"processInfo"));
            let now = get_f64(process, sel(c"systemUptime"));
            let window_number = if ns_window.is_null() {
                0
            } else {
                get_isize(ns_window, sel(c"windowNumber"))
            };
            let sel_key_event = sel(
                c"keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:",
            );
            let event = key_event(
                class(c"NSEvent"),
                sel_key_event,
                NS_EVENT_TYPE_KEY_DOWN,
                NsPoint { x: 0.0, y: 0.0 },
                NS_EVENT_MODIFIER_FLAG_COMMAND,
                now,
                window_number,
                std::ptr::null_mut(),
                q,
                q,
                false,
                KEY_CODE_Q,
            );
            // Not sent from here: this runs inside tao's event handler, which `terminate:`
            // would re-enter. The run loop delivers it on its next turn (it retains `event`).
            perform(
                NSApp,
                sel(c"performSelectorOnMainThread:withObject:waitUntilDone:"),
                sel(c"sendEvent:"),
                event,
                false,
            );
        }
    }
}

/// Prints the report on stdout and exits: 0 if it passed ([`report_passed`]), 1 otherwise.
#[tauri::command]
pub async fn debug_e2e_report(app: AppHandle, req: ReportReq) -> Result<(), AppError> {
    paths()?;
    let passed = report_passed(&req.report, &phase());
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", req.report);
    let _ = out.flush();
    app.exit(if passed { 0 } else { 1 });
    Ok(())
}

/// The page reload of phase 1: `-[WKWebView reload]`, as the WebView's own "Reload" (the app
/// binds no Cmd+R). The UI stored its state first; the answer may not reach the old page.
#[tauri::command]
pub async fn debug_e2e_reload(app: AppHandle) -> Result<(), AppError> {
    paths()?;
    app.get_webview_window("main")
        .ok_or_else(|| AppError::internal("no main window"))?
        .reload()
        .map_err(|e| AppError::internal(e.to_string()))
}

/// Phase 2: `step_1`..`step_12` all true; phase 3: `perf_flood` true; the gatekeeper phase:
/// `gatekeeper_ok` true. Every other boolean must be true as well, and `csp_violations` must
/// be 0.
fn report_passed(report: &Value, phase: &str) -> bool {
    let Some(fields) = report.as_object() else {
        return false;
    };
    let required: Vec<String> = match phase {
        "gatekeeper" => vec!["gatekeeper_ok".into()],
        "3" => vec!["perf_flood".into()],
        _ => (1..=12).map(|n| format!("step_{n}")).collect(),
    };
    report.get("csp_violations").and_then(Value::as_u64) == Some(0)
        && required
            .iter()
            .all(|k| report.get(k) == Some(&Value::Bool(true)))
        && fields.values().all(|v| v.as_bool().unwrap_or(true))
}

/// Only in `ATM_E2E_PHASE=gatekeeper` (opens ONE Terminal window). The login's own code on the
/// real `app_cache_dir` (in a folder of its own): `write_login_script` writes
/// `claude-login.command` and `open_login_terminal` opens it with `open -a Terminal`. Its
/// `claude` is a wrapper in the run's directory that writes its arguments to a marker and then
/// runs fake-claude: the marker proves that Gatekeeper let Terminal run the script and that the
/// script called `claude auth login`.
#[tauri::command]
pub async fn debug_e2e_gatekeeper(app: AppHandle) -> Result<E2eGatekeeper, AppError> {
    let p = paths()?;
    if phase() != "gatekeeper" {
        return Err(AppError::invalid("only in ATM_E2E_PHASE=gatekeeper"));
    }
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::io(e.to_string()))?
        .join(GATEKEEPER_DIR);
    let marker = p.dir.join("gatekeeper.marker");
    let bin = p.dir.join("bin");
    let wrapper = bin.join("claude");
    let content = format!(
        "#!/bin/sh\nprintf '%s' \"$*\" > {}\nexec {} \"$@\"\n",
        sh_quote(&marker),
        sh_quote(&p.fake)
    );
    let script = {
        let (cache, wrapper, marker) = (cache.clone(), wrapper.clone(), marker.clone());
        tauri::async_runtime::spawn_blocking(move || -> Result<PathBuf, AppError> {
            // As the app itself keeps its cache (spec §4): 0700.
            crate::create_private_dir(&cache)?;
            crate::create_private_dir(&bin)?;
            for stale in [&marker, &wrapper] {
                if let Err(e) = std::fs::remove_file(stale)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(e.into());
                }
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&wrapper)?
                .write_all(content.as_bytes())?;
            claude::write_login_script(&cache, &wrapper, LoginMethod::ClaudeAi)
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))??
    };
    let quarantined = tokio::process::Command::new("/usr/bin/xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(&script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success());
    let started = Instant::now();
    claude::open_login_terminal(&script).await?;
    let mut recorded = String::new();
    while started.elapsed() < GATEKEEPER_WAIT {
        if let Ok(text) = tokio::fs::read_to_string(&marker).await
            && !text.is_empty()
        {
            recorded = text;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let waited_ms = started.elapsed().as_millis() as u64;
    // Terminal's shell keeps the script open: removing it now is safe.
    let _ = tokio::fs::remove_dir_all(&cache).await;
    Ok(E2eGatekeeper {
        script: script.display().to_string(),
        quarantined,
        marker_ok: recorded.trim() == "auth login",
        marker: recorded,
        waited_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atm_types::Command as _;
    use atm_types::debug::*;
    use serde_json::json;

    #[test]
    fn report_requires_every_step_and_no_csp_violation() {
        let mut ok = json!({"csp_violations": 0, "channel_big_ok": true, "details": {"x": 1}});
        for n in 1..=12 {
            ok[format!("step_{n}")] = true.into();
        }
        assert!(report_passed(&ok, "2"));
        let mut missing = ok.clone();
        missing.as_object_mut().unwrap().remove("step_12");
        assert!(!report_passed(&missing, "2"));
        let mut failed = ok.clone();
        failed["channel_big_ok"] = false.into();
        assert!(!report_passed(&failed, "2"));
        let mut csp = ok.clone();
        csp["csp_violations"] = 2.into();
        assert!(!report_passed(&csp, "2"));
        assert!(report_passed(
            &json!({"gatekeeper_ok": true, "csp_violations": 0}),
            "gatekeeper"
        ));
        assert!(!report_passed(&json!({"csp_violations": 0}), "gatekeeper"));
        assert!(report_passed(
            &json!({"perf_flood": true, "command_failures_phase3": true, "csp_violations": 0}),
            "3"
        ));
        assert!(!report_passed(&json!({"csp_violations": 0}), "3"));
        assert!(!report_passed(
            &json!({"perf_flood": false, "csp_violations": 0}),
            "3"
        ));
    }

    #[test]
    fn git_takes_only_the_ui_checks() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        for ok in [
            &["show", "main:hello.txt"][..],
            &["show", "--name-only", "--format=", "main"],
            &["log", "-1", "--format=%H%n%s", "main"],
            &["status", "--porcelain"],
            &["branch", "--list", "atm/x"],
            &["worktree", "list", "--porcelain"],
        ] {
            assert!(check_git_args(&args(ok)).is_ok(), "{ok:?}");
        }
        for bad in [
            &[][..],
            &["-c", "alias.x=!sh", "x"],
            &["-C", "/", "status"],
            &["--git-dir=/tmp/x", "log"],
            &["log", "--output=/tmp/x"],
            &["show", "-c", "core.fsmonitor=x"],
            &["branch", "evil"],
            &["worktree", "add", "/tmp/x"],
            &["commit", "-m", "x"],
            &["config", "core.hooksPath", "/tmp"],
        ] {
            assert!(check_git_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_fake_claudes_binary_passes() {
        let dir = std::env::temp_dir().join(format!("atm-e2e-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-claude");
        std::fs::write(
            &fake,
            [b"\0bin ".as_slice(), FAKE_MARKER, b" rest"].concat(),
        )
        .unwrap();
        assert_eq!(verify_fake(&fake).unwrap(), fake.canonicalize().unwrap());
        // The real CLI copied or linked under the name.
        let real = dir.join("claude");
        std::fs::write(&real, "#!/bin/sh\necho '2.1.283 (Claude Code)'\n").unwrap();
        let copy = dir.join("copy");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::copy(&real, copy.join("fake-claude")).unwrap();
        assert!(verify_fake(&copy.join("fake-claude")).is_err());
        let link = dir.join("link");
        std::fs::create_dir_all(&link).unwrap();
        std::os::unix::fs::symlink(&real, link.join("fake-claude")).unwrap();
        assert!(verify_fake(&link.join("fake-claude")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inside_rejects_escapes_and_dangling_symlinks() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("atm-e2e-inside-{}", std::process::id()));
        let dir = root.join("run");
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        std::fs::write(dir.join("repo/hello.txt"), "hello\n").unwrap();
        std::os::unix::fs::symlink(root.join("outside.txt"), dir.join("repo/dangling")).unwrap();
        std::os::unix::fs::symlink(&root, dir.join("repo/up")).unwrap();
        let p = Paths {
            dir: dir.clone(),
            repos: dir.clone(),
            home: dir.clone(),
            auth: dir.join("auth"),
            record: dir.join("record.jsonl"),
            open_log: dir.join("open.jsonl"),
            phase1: dir.join("phase1.json"),
            fake: PathBuf::new(),
        };
        let inside =
            |path: PathBuf| tauri::async_runtime::block_on(p.inside(&path.display().to_string()));
        assert!(inside(dir.join("repo/hello.txt")).is_ok());
        assert!(inside(dir.join("repo/new.txt")).is_ok());
        assert!(inside(dir.join("repo/dangling")).is_err());
        assert!(inside(dir.join("repo/up/outside.txt")).is_err());
        assert!(inside(dir.join("../outside.txt")).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn processes_working_in_the_run_dir_are_found() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("atm-e2e-cwd-{}", std::process::id()));
        let (dir, sibling) = (root.join("run"), root.join("run-other"));
        for d in [&dir, &sibling] {
            std::fs::create_dir_all(d.join("wt")).unwrap();
        }
        let sleep_in = |d: &Path| {
            Command::new("sleep")
                .arg("30")
                .current_dir(d.join("wt"))
                .spawn()
                .unwrap()
        };
        let (mut inside, mut other) = (sleep_in(&dir), sleep_in(&sibling));
        let found = tauri::async_runtime::block_on(pids_in_dir(&dir)).unwrap();
        for child in [&mut inside, &mut other] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        std::fs::remove_dir_all(&root).unwrap();
        assert!(found.contains(&u64::from(inside.id())), "{found:?}");
        assert!(!found.contains(&u64::from(other.id())), "{found:?}");
    }

    #[test]
    fn e2e_fns_match_marker_names() {
        let _ = (
            debug_e2e_setup,
            debug_e2e_set_auth,
            debug_e2e_queue_pick,
            debug_e2e_queue_confirm,
            debug_e2e_confirms,
            debug_e2e_login_script,
            debug_e2e_record,
            debug_e2e_git,
            debug_e2e_write_file,
            debug_e2e_exists,
            debug_e2e_agents,
            debug_e2e_failures,
            debug_e2e_quit,
            debug_e2e_reload,
            debug_e2e_report,
            debug_e2e_gatekeeper,
        );
        for (f, name) in [
            ("debug_e2e_setup", DebugE2eSetup::NAME),
            ("debug_e2e_set_auth", DebugE2eSetAuth::NAME),
            ("debug_e2e_queue_pick", DebugE2eQueuePick::NAME),
            ("debug_e2e_queue_confirm", DebugE2eQueueConfirm::NAME),
            ("debug_e2e_confirms", DebugE2eConfirms::NAME),
            ("debug_e2e_login_script", DebugE2eLoginScript::NAME),
            ("debug_e2e_record", DebugE2eRecord::NAME),
            ("debug_e2e_git", DebugE2eGit::NAME),
            ("debug_e2e_write_file", DebugE2eWriteFile::NAME),
            ("debug_e2e_exists", DebugE2eExists::NAME),
            ("debug_e2e_agents", DebugE2eAgents::NAME),
            ("debug_e2e_failures", DebugE2eFailures::NAME),
            ("debug_e2e_quit", DebugE2eQuit::NAME),
            ("debug_e2e_reload", DebugE2eReload::NAME),
            ("debug_e2e_report", DebugE2eReport::NAME),
            ("debug_e2e_gatekeeper", DebugE2eGatekeeper::NAME),
        ] {
            assert_eq!(f, name);
        }
    }
}
