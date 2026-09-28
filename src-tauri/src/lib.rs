//! Thin Tauri shell: window, plugins, navigation guard and IPC commands.

#[cfg(debug_assertions)]
mod selftest;

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Manager, Runtime, Url};

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
    builder
        .plugin(nav_guard())
        .setup(|_app| {
            #[cfg(debug_assertions)]
            selftest::start_watchdog();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            #[cfg(debug_assertions)]
            selftest::debug_ping,
            #[cfg(debug_assertions)]
            selftest::debug_channel_probe,
            #[cfg(debug_assertions)]
            selftest::debug_selftest_enabled,
            #[cfg(debug_assertions)]
            selftest::debug_selftest_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AI Task Manager");
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
