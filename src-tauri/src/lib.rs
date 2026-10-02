//! Thin Tauri shell (spec §3): window, plugins, navigation guard, the IPC commands delegating
//! to `atm_core::Core`, event forwarding, page-load and exit hooks.

mod commands;
mod confirm;
#[cfg(debug_assertions)]
mod e2e;
#[cfg(debug_assertions)]
mod selftest;
mod updater;

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atm_core::runner::SHUTDOWN_DEADLINE;
use atm_core::{AppEvent, Core, CoreConfig, Notify};
use atm_types::AppError;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, Url};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::OnceCell;

/// Past `SHUTDOWN_DEADLINE`, the exit no longer waits for `Core::shutdown`.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Held in the data dir for the whole process lifetime (see [`lock_data_dir`]).
const LOCK_FILE: &str = "atm.lock";
/// A quitting instance ends within `SHUTDOWN_DEADLINE + SHUTDOWN_GRACE`; this covers the rest.
const LOCK_MARGIN: Duration = Duration::from_secs(2);

/// Debug builds: the names (never the values) [`scrub_inherited_session`] removed, for the E2E.
#[cfg(debug_assertions)]
pub(crate) const SCRUBBED_AT_START_ENV: &str = "ATM_SCRUBBED_AT_START";

/// M6 (spec §7.2, §10.2): the variables of a parent Claude Code session or of its host
/// (`atm_core::claude::is_nesting_var`: the parent's messaging socket and token, cmux's
/// automation sockets, …) are not only kept from the agents, they leave the app's own process:
/// if any is set, the app re-executes itself at once (same executable, same arguments, same
/// pid) without them. Removing them in place would not do: `ps -E` (sysctl `KERN_PROCARGS2`)
/// shows the environment a process was *started* with, to any process of the same user, and
/// an agent can run `ps`. `NODE_OPTIONS` becomes what the user had before a cmux terminal
/// rewrote it (`atm_core::claude::scrub_host_env`). Called first thing in `main`, before any
/// thread exists. If the re-exec fails the app goes on (the agents still never get them) and
/// says so on stderr.
pub fn scrub_inherited_session() {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::os::unix::process::CommandExt as _;

    let before: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    let mut after = before.clone();
    atm_core::claude::scrub_host_env(&mut after);
    if after == before {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("inherited session variables kept in the app's environment: {e}");
            return;
        }
    };
    let mut args = std::env::args_os();
    let arg0 = args.next().unwrap_or_else(|| exe.clone().into());
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg0(arg0).args(args).env_clear().envs(&after);
    #[cfg(debug_assertions)]
    cmd.env(
        SCRUBBED_AT_START_ENV,
        before
            .keys()
            .filter(|k| !after.contains_key(*k))
            .map(|k| k.to_string_lossy())
            .collect::<Vec<_>>()
            .join(","),
    );
    let e = cmd.exec();
    eprintln!("inherited session variables kept in the app's environment: re-exec failed: {e}");
}

/// Path of the single-instance plugin's socket (tauri-plugin-single-instance 2.5.0 on macOS,
/// without its `semver` feature): `/tmp/<identifier with . and - as _>_si.sock`.
fn single_instance_socket(identifier: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        "/tmp/{}_si.sock",
        identifier.replace(['.', '-'], "_")
    ))
}

/// `/tmp` is shared by every account of the Mac: a socket at the plugin's path owned by
/// another user would receive this launch's cwd and argv and make it exit 0, silently (the
/// plugin connects before binding). Such a socket turns the single-instance check off
/// instead, with a line on stderr (spec §10.2).
fn foreign_socket(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let uid = atm_core::current_uid();
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.uid() != uid => {
            eprintln!(
                "single instance off: {} belongs to another user (uid {})",
                path.display(),
                meta.uid()
            );
            true
        }
        _ => false,
    }
}

pub fn run() {
    // A selftest or E2E run must not hand off to an instance that is already open (debug and
    // release share the socket): the plugin would exit 0 before anything was tested.
    #[cfg(debug_assertions)]
    let single_instance = !selftest::selftest_enabled() && !e2e::enabled();
    #[cfg(not(debug_assertions))]
    let single_instance = true;
    let context = tauri::generate_context!();
    let single_instance =
        single_instance && !foreign_socket(&single_instance_socket(&context.config().identifier));

    let mut builder = tauri::Builder::default();
    if single_instance {
        // Must be registered first: a second launch focuses the running window and exits.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Runs off the main thread: until the core is up the window must stay hidden.
            if app.try_state::<Arc<Core>>().is_some()
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        // Driven from Rust only (`updater.rs`): the capabilities grant the webview none of its
        // commands.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::Pending::default())
        .plugin(nav_guard())
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                selftest::start_watchdog();
                e2e::start_watchdog();
            }
            let handle = app.handle().clone();
            match start_core(&handle) {
                Ok(core) => {
                    app.manage(core);
                    updater::start(&handle);
                    // Hidden in tauri.conf.json until recovery is done: no blank window.
                    if let Some(window) = app.get_webview_window("main")
                        && let Err(e) = window.show()
                    {
                        eprintln!("show main window: {e}");
                    }
                }
                Err(e) => exit_with_error(&handle, &e),
            }
            Ok(())
        })
        // A reload must not leave transcript forwarders behind (spec §6.5).
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started
                && let Some(core) = webview.try_state::<Arc<Core>>()
            {
                core.drop_subscriptions();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_env,
            commands::open_login_terminal,
            commands::resume_agents,
            commands::get_settings,
            commands::update_settings,
            commands::list_projects,
            commands::pick_repo_folder,
            commands::add_project,
            commands::update_project,
            commands::set_project_security,
            commands::remove_project,
            commands::list_branches,
            commands::get_project_overview,
            commands::get_board,
            commands::create_task,
            commands::update_task,
            commands::move_task,
            commands::delete_task,
            commands::get_task_detail,
            commands::pick_attachment_files,
            commands::add_task_attachments,
            commands::remove_task_attachment,
            commands::start_attempt,
            commands::send_follow_up,
            commands::stop_attempt,
            commands::respond_approval,
            commands::subscribe_transcript,
            commands::unsubscribe_transcript,
            commands::get_entries,
            commands::get_diff,
            commands::get_branch_status,
            commands::merge_attempt,
            commands::discard_attempt,
            commands::delete_branch,
            commands::open_attempt,
            commands::open_url,
            commands::start_plan,
            commands::get_plan,
            commands::resolve_plan,
            commands::app_info,
            commands::check_update,
            commands::install_update,
            #[cfg(debug_assertions)]
            selftest::debug_ping,
            #[cfg(debug_assertions)]
            selftest::debug_channel_probe,
            #[cfg(debug_assertions)]
            selftest::debug_selftest_enabled,
            #[cfg(debug_assertions)]
            selftest::debug_forwarder_count,
            #[cfg(debug_assertions)]
            selftest::debug_selftest_report,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_setup,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_set_auth,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_queue_pick,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_queue_confirm,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_confirms,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_login_script,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_record,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_git,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_write_file,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_exists,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_agents,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_failures,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_quit,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_reload,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_report,
            #[cfg(debug_assertions)]
            e2e::debug_e2e_gatekeeper,
        ])
        .build(context)
        .expect("error while building AI Task Manager");
    app.run(on_run_event);
}

/// Builds the core and runs its recovery before the UI can load any data (spec §7.9), both
/// inside the async runtime. A failed `startup` is logged; the app still runs.
fn start_core(app: &AppHandle) -> Result<Arc<Core>, AppError> {
    let config = core_config(app)?;
    let wait = SHUTDOWN_DEADLINE + SHUTDOWN_GRACE + LOCK_MARGIN;
    // Released by the kernel when the process exits, whichever way.
    std::mem::forget(lock_data_dir(&config.data_dir, wait)?);
    let notify = notifier(app.clone());
    tauri::async_runtime::block_on(async move {
        let core = Arc::new(Core::new(config, notify)?);
        if let Err(e) = core.startup().await {
            eprintln!("core startup failed: {e}");
        }
        Ok(core)
    })
}

/// The core's paths, with the data and cache dirs created private (spec §4).
fn core_config(app: &AppHandle) -> Result<CoreConfig, AppError> {
    let config = dirs_config(app).map_err(|e| AppError::io(e.to_string()))?;
    for dir in [&config.data_dir, &config.cache_dir] {
        create_private_dir(dir).map_err(|e| AppError::io(format!("{}: {e}", dir.display())))?;
    }
    Ok(config)
}

fn dirs_config(app: &AppHandle) -> tauri::Result<CoreConfig> {
    #[cfg(debug_assertions)]
    if selftest::selftest_enabled() {
        return Ok(selftest::core_config());
    }
    #[cfg(debug_assertions)]
    if e2e::enabled() {
        return Ok(e2e::core_config());
    }
    Ok(CoreConfig {
        data_dir: app.path().app_data_dir()?,
        cache_dir: app.path().app_cache_dir()?,
        ..CoreConfig::default()
    })
}

/// `dir` and its missing parents are created 0700, an existing `dir` is tightened to 0700:
/// the DB, logs and login script inside (created 0600/0700 by the core) stay the user's.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// The data dir belongs to one process at a time. Cmd+Q, the Dock's Quit and logout end the
/// event loop without `ExitRequested`: the single-instance socket is removed before `Exit`
/// runs the shutdown, so a relaunch in that window waits here (up to `wait`) instead of
/// recovering the agents the quitting instance is still finalizing. Errors: `Busy`, `Io`.
fn lock_data_dir(dir: &Path, wait: Duration) -> Result<File, AppError> {
    let path = dir.join(LOCK_FILE);
    let io_err = |e: std::io::Error| AppError::io(format!("{}: {e}", path.display()));
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(io_err)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(AppError::busy(format!(
                    "Un'altra istanza di AI Task Manager sta ancora usando {}.",
                    dir.display()
                )));
            }
            Err(TryLockError::Error(e)) => return Err(io_err(e)),
        }
    }
}

/// The app cannot run without its core (e.g. an unreadable or newer DB): a launch from the
/// Finder would otherwise just vanish. The window stays hidden; OK exits with 1.
fn exit_with_error(app: &AppHandle, e: &AppError) {
    eprintln!("core init failed: {e}");
    let exit = app.clone();
    app.dialog()
        .message(format!(
            "AI Task Manager non può avviarsi.\n\n{}",
            e.message
        ))
        .title("Errore di avvio")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(move |_| exit.exit(1));
}

/// `changed` / `env_changed` for every window (spec §6.4).
fn notifier(app: AppHandle) -> Notify {
    Arc::new(move |event: AppEvent| {
        let sent = match &event {
            AppEvent::Changed(payload) => app.emit(event.name(), payload),
            AppEvent::EnvChanged(payload) => app.emit(event.name(), payload),
        };
        if let Err(e) = sent {
            eprintln!("emit {}: {e}", event.name());
        }
    })
}

static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// The first exit request is held back while [`shutdown_core`] runs (spec §7.9); then the
/// app exits with the requested code (the selftest's 0/1 included). Cmd+Q (`NSApp
/// terminate:`) ends the event loop without `ExitRequested`, so `Exit` runs the shutdown too,
/// blocking the main thread: the process ends as soon as this callback returns, and only
/// then releases the data-dir lock a relaunch waits on ([`lock_data_dir`]).
fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { code, api, .. } => {
            if EXIT_REQUESTED.swap(true, Ordering::SeqCst) {
                return;
            }
            // Debug builds: which branch ran the shutdown (the E2E checks both are taken).
            #[cfg(debug_assertions)]
            eprintln!("exit: RunEvent::ExitRequested");
            api.prevent_exit();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                shutdown_core(&app).await;
                app.exit(code.unwrap_or(0));
            });
        }
        RunEvent::Exit => {
            #[cfg(debug_assertions)]
            eprintln!("exit: RunEvent::Exit");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
            tauri::async_runtime::block_on(shutdown_core(app));
            #[cfg(debug_assertions)]
            selftest::remove_private_dir();
        }
        _ => {}
    }
}

/// Stops the agents once, whichever exit path gets here first; a second caller waits for the
/// first. Bounded even if the shutdown panics or overruns: a windowless process that keeps
/// the single-instance socket would swallow every relaunch.
pub(crate) async fn shutdown_core(app: &AppHandle) {
    static DONE: OnceCell<()> = OnceCell::const_new();
    DONE.get_or_init(|| async {
        let Some(core) = app.try_state::<Arc<Core>>().map(|s| Arc::clone(&s)) else {
            return;
        };
        let shutdown = tauri::async_runtime::spawn(async move {
            core.shutdown(SHUTDOWN_DEADLINE).await;
        });
        match tokio::time::timeout(SHUTDOWN_DEADLINE + SHUTDOWN_GRACE, shutdown).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("core shutdown failed: {e}"),
            Err(_) => eprintln!("core shutdown overran {SHUTDOWN_DEADLINE:?}"),
        }
    })
    .await;
}

/// Blocks every webview navigation that does not target the app's own origin.
fn nav_guard<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("nav-guard")
        .on_navigation(|_webview, url| is_allowed_origin(url))
        .build()
}

fn is_allowed_origin(url: &Url) -> bool {
    let origin = (url.scheme(), url.host_str(), url.port());
    matches!(
        origin,
        ("tauri", Some("localhost"), None) | ("http", Some("tauri.localhost"), None)
    ) || (cfg!(debug_assertions) && origin == ("http", Some("localhost"), Some(1420)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_guard_allows_only_app_origins() {
        let ok = |s: &str| is_allowed_origin(&Url::parse(s).unwrap());
        assert!(ok("tauri://localhost/index.html"));
        assert!(ok("http://tauri.localhost/"));
        assert_eq!(ok("http://localhost:1420/"), cfg!(debug_assertions));
        assert!(!ok("https://example.com/"));
        assert!(!ok("http://localhost:8080/"));
        assert!(!ok("http://tauri.localhost.evil.com/"));
        assert!(!ok("file:///etc/passwd"));
    }

    #[test]
    fn single_instance_socket_path_and_owner() {
        assert_eq!(
            single_instance_socket("dev.aitaskmanager.desktop"),
            Path::new("/tmp/dev_aitaskmanager_desktop_si.sock")
        );
        let dir = std::env::temp_dir().join(format!("atm-si-{}", std::process::id()));
        create_private_dir(&dir).unwrap();
        let mine = dir.join("mine.sock");
        std::fs::write(&mine, "").unwrap();
        assert!(!foreign_socket(&mine), "our own file");
        assert!(!foreign_socket(&dir.join("missing.sock")));
        // `/` belongs to root: what another account's socket looks like to this one.
        assert!(foreign_socket(Path::new("/")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn data_dir_lock_is_exclusive_until_released() {
        let dir = std::env::temp_dir().join(format!("atm-data-lock-{}", std::process::id()));
        create_private_dir(&dir).unwrap();
        let held = lock_data_dir(&dir, Duration::ZERO).unwrap();
        let busy = lock_data_dir(&dir, Duration::from_millis(250)).unwrap_err();
        assert_eq!(busy.code, atm_types::ErrorCode::Busy);
        drop(held);
        lock_data_dir(&dir, Duration::ZERO).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn private_dirs_are_0700_even_if_they_existed() {
        let root = std::env::temp_dir().join(format!("atm-private-dir-{}", std::process::id()));
        let existing = root.join("existing");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        let nested = root.join("new/nested");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for dir in [&existing, &nested] {
            create_private_dir(dir).unwrap();
            assert_eq!(mode(dir), 0o700, "{}", dir.display());
        }
        assert_eq!(mode(&root.join("new")), 0o700);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
