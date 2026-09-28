//! Debug-only in-app E2E of spec §12.2 (M4), driven by the UI (`ui/src/e2e.rs`) when
//! `ATM_E2E=1` (ignored under `ATM_SELFTEST=1`). `scripts/e2e.sh` launches the app twice with the
//! same `ATM_E2E_DIR`: `ATM_E2E_PHASE=1` runs steps 1–7 and quits during a turn the way Cmd+Q
//! does ([`debug_e2e_quit`]: `NSApp terminate:` → `RunEvent::Exit`); `ATM_E2E_PHASE=2`
//! relaunches on the same data, runs the rest and exits through `app.exit`
//! (`RunEvent::ExitRequested`) during another turn; `gatekeeper` only opens the real login
//! script in Terminal (spec §7.10, run by hand).
//!
//! Everything lives under `ATM_E2E_DIR`: data and cache dirs, `HOME` (hence the worktree
//! root), the temporary repositories, fake-claude's record and login state. Agents always run
//! as fake-claude (`ATM_CLAUDE_PATH`, else the `fake-claude` next to this binary); `open` is
//! recorded instead of run, the native folder picker returns queued paths.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atm_core::claude::{self, ATM_CLAUDE_PATH_ENV};
use atm_core::{CoreConfig, git::is_scrubbed_git_var};
use atm_types::debug::{
    E2eAuthReq, E2eGatekeeper, E2eGitOut, E2eGitReq, E2eLoginScript, E2ePathReq, E2eSetup,
    E2eWriteReq, ReportReq,
};
use atm_types::{AppError, LoginMethod};
use serde_json::Value;
use tauri::{AppHandle, Manager};

/// Instead of the login shell's: a run must not depend on the user's `.zshrc`.
const E2E_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// A phase that has not reported by then exits 1 with its own message: shorter than the 300 s
/// `scripts/e2e.sh` gives each phase before killing it (a phase takes ~20 s).
const WATCHDOG: Duration = Duration::from_secs(240);
/// Seconds of `[fake:slow]`: long enough to stop it halfway.
const SLOW_EVENTS: &str = "30";
/// Pause between the text deltas of a streamed text: the UI's progressive rendering is seen.
const DELTA_MS: &str = "300";
/// Folder of `app_cache_dir` for the Gatekeeper check: a real login script is never touched.
const GATEKEEPER_DIR: &str = "e2e-gatekeeper";
const GATEKEEPER_WAIT: Duration = Duration::from_secs(30);

/// Path returned by the next `pick_repo_folder` (queued by `debug_e2e_queue_pick`).
static PICK: Mutex<Option<String>> = Mutex::new(None);
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
        let fake = std::env::var_os(ATM_CLAUDE_PATH_ENV)
            .map(PathBuf::from)
            .or_else(|| Some(std::env::current_exe().ok()?.parent()?.join("fake-claude")))
            .filter(|p| p.is_file() && p.file_name().is_some_and(|n| n == "fake-claude"))
            .unwrap_or_else(|| {
                fail("no fake-claude: run `cargo build -p atm-core --bin fake-claude`")
            });
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

/// Data dir, cache dir, fake-claude and the child environment of a run. Phase 1 first creates
/// the repositories and logs fake-claude out (step 1 starts at the login gate).
pub fn core_config() -> CoreConfig {
    let p = Paths::get();
    for dir in [&p.home, &p.repos] {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(&format!("{}: {e}", dir.display())));
    }
    if phase() == "1" {
        if let Err(e) = create_repos(&p) {
            fail(&format!("repositories not created: {e}"));
        }
        write_auth(&p, false).unwrap_or_else(|e| fail(&e.message));
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
                "phase {} did not report within {WATCHDOG:?}",
                phase()
            ));
        });
    }
}

/// `Some(queued path)` in a run: `pick_repo_folder` must not open the native picker.
pub fn take_pick() -> Option<Option<String>> {
    enabled().then(|| PICK.lock().unwrap_or_else(|e| e.into_inner()).take())
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

/// `repos/{main, mcp}` (one commit on `main`, a local identity), `repos/not-git` (a plain
/// folder), `repos/bare.git` (a mirror of `main`), `repos/empty` (no commits). Kept if they
/// exist.
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
            (".mcp.json", "{\"mcpServers\": {}}\n"),
        ],
    )?;
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

fn write_auth(p: &Paths, logged_in: bool) -> Result<(), AppError> {
    std::fs::write(&p.auth, auth_state(logged_in))
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

#[tauri::command]
pub async fn debug_e2e_setup() -> Result<Option<E2eSetup>, AppError> {
    if !enabled() {
        return Ok(None);
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
        phase1,
    }))
}

#[tauri::command]
pub async fn debug_e2e_set_auth(req: E2eAuthReq) -> Result<(), AppError> {
    let p = paths()?;
    tokio::fs::write(&p.auth, auth_state(req.logged_in))
        .await
        .map_err(|e| AppError::io(format!("{}: {e}", p.auth.display())))
}

#[tauri::command]
pub async fn debug_e2e_queue_pick(req: E2ePathReq) -> Result<(), AppError> {
    paths()?;
    *PICK.lock().unwrap_or_else(|e| e.into_inner()) = Some(req.path);
    Ok(())
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

/// The run's agents still alive: the pids fake-claude recorded in this run's record (its
/// `-p` calls and the `sleep` grandchildren of `hang_ignore`) that `ps` still shows running
/// the same program (a pid can be reused). Other fake-claude processes on the machine (a
/// concurrent `cargo test`, another app instance) and the short `--version`/`auth status`
/// probes are never counted.
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
    if recorded.is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<String> = recorded.keys().map(u64::to_string).collect();
    // Exits 1 when some pid is gone: only its output matters.
    let out = tokio::process::Command::new("/bin/ps")
        .args(["-o", "pid=,command=", "-p", &list.join(",")])
        .stdin(Stdio::null())
        .output()
        .await?;
    let fake = p.fake.display().to_string();
    Ok(lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            let pid: u64 = pid.parse().ok()?;
            let command = command.trim();
            let alive = match recorded.get(&pid)? {
                false => command.starts_with(&fake),
                true => command == "sleep 300",
            };
            alive.then_some(pid as i32)
        })
        .collect())
}

#[tauri::command]
pub async fn debug_e2e_failures() -> Result<Vec<String>, AppError> {
    paths()?;
    Ok(FAILURES.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// Hands the partial report to phase 2, then quits the way Cmd+Q does. The default menu's Quit
/// sends `terminate:` to `NSApp`; tao has no `applicationShouldTerminate:`, so AppKit goes
/// straight to `applicationWillTerminate:` → tao's `LoopDestroyed` → `RunEvent::Exit`, never
/// `ExitRequested`: `on_run_event` then runs `Core::shutdown` on the main thread before the
/// process exits with 0. (`app.exit` would take the other, `ExitRequested`, branch: phase 2
/// exits that way.)
#[tauri::command]
pub async fn debug_e2e_quit(app: AppHandle, req: ReportReq) -> Result<(), AppError> {
    let p = paths()?;
    tokio::fs::write(&p.phase1, req.report.to_string()).await?;
    eprintln!("e2e: phase 1 done, quitting like Cmd+Q (NSApp terminate:)");
    terminate_like_cmd_q(&app);
    Ok(())
}

/// `[NSApp performSelectorOnMainThread:@selector(terminate:) withObject:nil
/// waitUntilDone:NO]`: the main run loop sends `terminate:` outside any tao callback, as the
/// Quit menu item does (a `run_on_main_thread` closure would run inside tao's handler).
#[cfg(target_os = "macos")]
fn terminate_like_cmd_q(_app: &AppHandle) {
    use std::ffi::{c_char, c_void};
    type Id = *mut c_void;
    type Perform = unsafe extern "C" fn(Id, Id, Id, Id, bool);
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSApp: Id;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }
    // SAFETY: `NSApp` is set once AppKit runs (the window is up); `objc_msgSend` is called
    // through the exact prototype of `-[NSObject performSelectorOnMainThread:withObject:
    // waitUntilDone:]` (id, SEL, SEL, id, BOOL), which may be sent from any thread.
    unsafe {
        let perform = std::mem::transmute::<unsafe extern "C" fn(), Perform>(objc_msgSend);
        let selector =
            sel_registerName(c"performSelectorOnMainThread:withObject:waitUntilDone:".as_ptr());
        let terminate = sel_registerName(c"terminate:".as_ptr());
        perform(NSApp, selector, terminate, std::ptr::null_mut(), false);
    }
}

#[cfg(not(target_os = "macos"))]
fn terminate_like_cmd_q(app: &AppHandle) {
    app.exit(0);
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

/// Phase 2: `step_1`..`step_12` all true; the gatekeeper phase: `gatekeeper_ok` true. Every
/// other boolean must be true as well, and `csp_violations` must be 0.
fn report_passed(report: &Value, phase: &str) -> bool {
    let Some(fields) = report.as_object() else {
        return false;
    };
    let required: Vec<String> = match phase {
        "gatekeeper" => vec!["gatekeeper_ok".into()],
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
    }

    #[test]
    fn e2e_fns_match_marker_names() {
        let _ = (
            debug_e2e_setup,
            debug_e2e_set_auth,
            debug_e2e_queue_pick,
            debug_e2e_login_script,
            debug_e2e_record,
            debug_e2e_git,
            debug_e2e_write_file,
            debug_e2e_exists,
            debug_e2e_agents,
            debug_e2e_failures,
            debug_e2e_quit,
            debug_e2e_report,
            debug_e2e_gatekeeper,
        );
        for (f, name) in [
            ("debug_e2e_setup", DebugE2eSetup::NAME),
            ("debug_e2e_set_auth", DebugE2eSetAuth::NAME),
            ("debug_e2e_queue_pick", DebugE2eQueuePick::NAME),
            ("debug_e2e_login_script", DebugE2eLoginScript::NAME),
            ("debug_e2e_record", DebugE2eRecord::NAME),
            ("debug_e2e_git", DebugE2eGit::NAME),
            ("debug_e2e_write_file", DebugE2eWriteFile::NAME),
            ("debug_e2e_exists", DebugE2eExists::NAME),
            ("debug_e2e_agents", DebugE2eAgents::NAME),
            ("debug_e2e_failures", DebugE2eFailures::NAME),
            ("debug_e2e_quit", DebugE2eQuit::NAME),
            ("debug_e2e_report", DebugE2eReport::NAME),
            ("debug_e2e_gatekeeper", DebugE2eGatekeeper::NAME),
        ] {
            assert_eq!(f, name);
        }
    }
}
