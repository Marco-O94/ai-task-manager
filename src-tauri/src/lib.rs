//! Thin Tauri shell (spec §3): window, plugins, navigation guard, the IPC commands delegating
//! to `atm_core::Core`, event forwarding, page-load and exit hooks.

mod commands;
mod confirm;
#[cfg(debug_assertions)]
mod selftest;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use atm_core::runner::SHUTDOWN_DEADLINE;
use atm_core::{AppEvent, Core, CoreConfig, Notify};
use atm_types::AppError;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, Url};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Past `SHUTDOWN_DEADLINE`, the exit no longer waits for `Core::shutdown`.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

pub fn run() {
    // A selftest run must not hand off to an instance that is already open (debug and release
    // share the socket): the plugin would exit 0 before anything was tested.
    #[cfg(debug_assertions)]
    let single_instance = !selftest::selftest_enabled();
    #[cfg(not(debug_assertions))]
    let single_instance = true;

    let mut builder = tauri::Builder::default();
    if single_instance {
        // Must be registered first: a second launch focuses the running window and exits.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(nav_guard())
        .setup(|app| {
            #[cfg(debug_assertions)]
            selftest::start_watchdog();
            let handle = app.handle().clone();
            match start_core(&handle) {
                Ok(core) => {
                    app.manage(core);
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
            commands::get_board,
            commands::create_task,
            commands::update_task,
            commands::move_task,
            commands::delete_task,
            commands::get_task_detail,
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building AI Task Manager");
    app.run(on_run_event);
}

/// Builds the core and runs its recovery before the UI can load any data (spec §7.9), both
/// inside the async runtime. A failed `startup` is logged; the app still runs.
fn start_core(app: &AppHandle) -> Result<Arc<Core>, AppError> {
    let config = core_config(app).map_err(|e| AppError::io(e.to_string()))?;
    let notify = notifier(app.clone());
    tauri::async_runtime::block_on(async move {
        let core = Arc::new(Core::new(config, notify)?);
        if let Err(e) = core.startup().await {
            eprintln!("core startup failed: {e}");
        }
        Ok(core)
    })
}

fn core_config(app: &AppHandle) -> tauri::Result<CoreConfig> {
    // A selftest must never open the user's DB nor recover (kill) the agents of an open app.
    #[cfg(debug_assertions)]
    if selftest::selftest_enabled() {
        let dir = selftest::private_dir();
        return Ok(CoreConfig {
            data_dir: dir.join("data"),
            cache_dir: dir.join("cache"),
            ..CoreConfig::default()
        });
    }
    Ok(CoreConfig {
        data_dir: app.path().app_data_dir()?,
        cache_dir: app.path().app_cache_dir()?,
        ..CoreConfig::default()
    })
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

static EXITING: AtomicBool = AtomicBool::new(false);

/// The first exit request is held back while `Core::shutdown` stops the agents (spec §7.9);
/// then the app exits with the requested code (the selftest's 0/1 included). The exit
/// happens even if the shutdown panics or overruns: a windowless process that keeps the
/// single-instance socket would swallow every relaunch.
fn on_run_event(app: &AppHandle, event: RunEvent) {
    if let RunEvent::ExitRequested { code, api, .. } = event {
        if EXITING.swap(true, Ordering::SeqCst) {
            return;
        }
        api.prevent_exit();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let core = app.try_state::<Arc<Core>>().map(|s| Arc::clone(&s));
            if let Some(core) = core {
                let shutdown = tauri::async_runtime::spawn(async move {
                    core.shutdown(SHUTDOWN_DEADLINE).await;
                });
                match tokio::time::timeout(SHUTDOWN_DEADLINE + SHUTDOWN_GRACE, shutdown).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("core shutdown failed: {e}"),
                    Err(_) => eprintln!("core shutdown overran {SHUTDOWN_DEADLINE:?}"),
                }
            }
            #[cfg(debug_assertions)]
            selftest::remove_private_dir();
            app.exit(code.unwrap_or(0));
        });
    }
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
}
