//! App updates (round 2026-10-02, ARCHITECTURE.md «Versione e aggiornamenti»): the shell
//! checks GitHub Releases' `latest.json` (tauri-plugin-updater, signature checked against the
//! `pubkey` of tauri.conf.json) at startup and every 6 h, keeps what it found for
//! `check_update` and announces it with `update_available`. The webview has no updater
//! permission: everything goes through the commands below.
//!
//! Never in debug builds (selftest, E2E, `cargo tauri dev`) nor with `ATM_NO_UPDATE_CHECK`
//! set. A failed check (offline, no release yet, bad signature) is a line on stderr: the app
//! goes on as it is.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use atm_types::{AppError, EVENT_UPDATE_AVAILABLE, UpdateInfo};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Between two background checks.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Turns the checks off in a release build too.
const NO_CHECK_ENV: &str = "ATM_NO_UPDATE_CHECK";
/// In the data dir: why the last install failed, written before the restart that follows it
/// and read back (then removed) by the next launch, so the modal can say why and offer a way
/// out instead of looping on «Aggiorna e riavvia».
const INSTALL_ERROR_FILE: &str = "update-install-error.txt";

/// Managed state: the update the latest check found (the plugin's handle, needed to install).
#[derive(Default)]
pub struct Pending {
    update: Mutex<Option<Update>>,
    installing: AtomicBool,
    /// The previous launch's failed install (see [`INSTALL_ERROR_FILE`]).
    install_error: Mutex<Option<String>>,
}

/// Whether this build checks for updates at all.
pub fn checks_enabled() -> bool {
    !cfg!(debug_assertions) && std::env::var_os(NO_CHECK_ENV).is_none()
}

/// The background loop: a check now, then every [`CHECK_EVERY`]. No-op when checks are off.
pub fn start(app: &AppHandle) {
    if !checks_enabled() {
        return;
    }
    if let Some(path) = install_error_path(app) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let text = text.trim().to_owned();
            *app.state::<Pending>().install_error.lock().unwrap() =
                (!text.is_empty()).then_some(text);
        }
        let _ = std::fs::remove_file(&path);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check(&app).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

async fn check(app: &AppHandle) {
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };
    let update = match found {
        Ok(update) => update,
        Err(e) => {
            eprintln!("update check failed: {e}");
            return;
        }
    };
    let pending = app.state::<Pending>();
    let info = update.as_ref().map(|u| info_of(&pending, u));
    let had = std::mem::replace(&mut *pending.update.lock().unwrap(), update).is_some();
    match &info {
        Some(info) => eprintln!(
            "update available: {} -> {} (required: {})",
            info.current, info.latest, info.required
        ),
        // Nothing new to say.
        None if !had => return,
        None => eprintln!("update no longer available"),
    }
    // `None` takes back an announced update (a pulled release).
    if let Err(e) = app.emit(EVENT_UPDATE_AVAILABLE, &info) {
        eprintln!("emit {EVENT_UPDATE_AVAILABLE}: {e}");
    }
}

fn info_of(pending: &Pending, update: &Update) -> UpdateInfo {
    UpdateInfo {
        required: is_required(&update.current_version, &update.version),
        current: update.current_version.clone(),
        latest: update.version.clone(),
        notes: update.body.clone().filter(|n| !n.trim().is_empty()),
        install_error: pending.install_error.lock().unwrap().clone(),
    }
}

fn install_error_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join(INSTALL_ERROR_FILE))
}

/// `check_update`: what the latest background check found.
pub fn pending(app: &AppHandle) -> Option<UpdateInfo> {
    let pending = app.state::<Pending>();
    let update = pending.update.lock().unwrap().clone();
    update.as_ref().map(|u| info_of(&pending, u))
}

/// `install_update`: download and verify first (a failure leaves the app as it is), then stop
/// the agents through the exit path's graceful shutdown, install over the bundle and restart
/// (`request_restart` goes through `RunEvent::ExitRequested`, whose shutdown has already run).
/// Once the agents are stopped there is no way back: a failed install restarts the old version,
/// which shows why ([`INSTALL_ERROR_FILE`]). What can be told before stopping anything (a
/// bundle macOS runs translocated or from a read-only volume) is an error here instead.
pub async fn install(app: &AppHandle) -> Result<(), AppError> {
    let pending = app.state::<Pending>();
    let Some(update) = pending.update.lock().unwrap().clone() else {
        return Err(AppError::invalid("Nessun aggiornamento disponibile"));
    };
    if pending.installing.swap(true, Ordering::SeqCst) {
        return Err(AppError::busy("Aggiornamento già in corso"));
    }
    if let Err(e) = installable() {
        pending.installing.store(false, Ordering::SeqCst);
        return Err(e);
    }
    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => bytes,
        Err(e) => {
            pending.installing.store(false, Ordering::SeqCst);
            return Err(AppError::io(format!("Download dell'aggiornamento: {e}")));
        }
    };
    crate::shutdown_core(app).await;
    if let Err(e) = update.install(bytes) {
        eprintln!("update install failed, restarting the current version: {e}");
        if let Some(path) = install_error_path(app) {
            let text = format!("Installazione della v{} non riuscita: {e}", update.version);
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("{}: {e}", path.display());
            }
        }
    }
    app.request_restart();
    Ok(())
}

/// The running bundle can be replaced: not translocated by Gatekeeper (an app opened from
/// Downloads or a disk image without being moved) and not on a read-only volume. A folder the
/// user cannot write is fine: the updater asks for an administrator's password.
fn installable() -> Result<(), AppError> {
    let exe =
        std::env::current_exe().map_err(|e| AppError::io(format!("Percorso dell'app: {e}")))?;
    let Some(bundle) = bundle_of(&exe) else {
        // Not a bundle (a bare binary): let the updater say.
        return Ok(());
    };
    if is_translocated(bundle) {
        return Err(AppError::invalid(
            "L'app è in esecuzione da una posizione temporanea di macOS: spostala nella cartella Applicazioni, riaprila e riprova",
        ));
    }
    let parent = bundle.parent().unwrap_or(Path::new("/"));
    let probe = parent.join(format!(".atm-update-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        // EROFS: a disk image or another read-only volume.
        Err(e) if e.raw_os_error() == Some(30) => Err(AppError::invalid(
            "L'app è su un volume di sola lettura: copiala nella cartella Applicazioni, riaprila e riprova",
        )),
        Err(_) => Ok(()),
    }
}

/// The `….app` directory that contains `exe`.
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
}

fn is_translocated(bundle: &Path) -> bool {
    bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
}

/// `(major, minor, patch)` of `1.2.3`, `v1.2.3` or `1.2.3-beta.1+build` (pre-release and
/// build metadata ignored); `None` if not of that shape.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// The update is required (the UI blocks until it is installed) when it breaks compatibility by
/// semver: a higher major, or, while both majors are 0, a higher minor (0.x convention). Any
/// other newer release is optional; an unparsable version is never required.
pub fn is_required(current: &str, latest: &str) -> bool {
    match (parse(current), parse(latest)) {
        (Some((cur_major, cur_minor, _)), Some((new_major, new_minor, _))) => {
            new_major > cur_major || (cur_major == 0 && new_major == 0 && new_minor > cur_minor)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_with_or_without_v_and_suffixes() {
        assert_eq!(parse("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse("v1.20.3"), Some((1, 20, 3)));
        assert_eq!(parse("2.0.0-beta.1"), Some((2, 0, 0)));
        assert_eq!(parse("2.0.0+abc"), Some((2, 0, 0)));
        assert_eq!(parse("1.2"), None);
        assert_eq!(parse("1.2.3.4"), None);
        assert_eq!(parse("1.x.3"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn severity_follows_semver_with_the_0x_convention() {
        // 0.x: a new minor breaks, a new patch does not.
        assert!(is_required("0.1.0", "0.2.0"));
        assert!(is_required("0.1.5", "v0.3.0"));
        assert!(!is_required("0.1.0", "0.1.1"));
        // A new major always breaks.
        assert!(is_required("0.9.0", "1.0.0"));
        assert!(is_required("1.4.2", "2.0.0"));
        // From 1.0 on, minors and patches are optional.
        assert!(!is_required("1.0.0", "1.1.0"));
        assert!(!is_required("1.0.0", "1.0.1"));
        // Not newer, or unparsable: never required.
        assert!(!is_required("0.2.0", "0.1.0"));
        assert!(!is_required("2.0.0", "1.9.9"));
        assert!(!is_required("0.1.0", "latest"));
        assert!(!is_required("dev", "1.0.0"));
    }

    #[test]
    fn bundle_and_translocation_are_found_in_the_path() {
        let exe = Path::new("/Applications/AI Task Manager.app/Contents/MacOS/ai-task-manager");
        assert_eq!(
            bundle_of(exe),
            Some(Path::new("/Applications/AI Task Manager.app"))
        );
        assert!(!is_translocated(bundle_of(exe).unwrap()));
        let moved = Path::new(
            "/private/var/folders/x/AppTranslocation/1234/d/AI Task Manager.app/Contents/MacOS/a",
        );
        assert!(is_translocated(bundle_of(moved).unwrap()));
        assert_eq!(bundle_of(Path::new("/usr/local/bin/atm")), None);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_builds_never_check() {
        assert!(!checks_enabled());
    }
}
