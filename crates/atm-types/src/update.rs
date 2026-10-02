//! App version and updates (round 2026-10-02, ARCHITECTURE.md «Versione e aggiornamenti»).
//!
//! The update check runs in the shell (tauri-plugin-updater, GitHub Releases' `latest.json`)
//! at startup and every 6 h, never in debug builds; the UI only reads its result and asks for
//! the install.

use serde::{Deserialize, Serialize};

use crate::Empty;
use crate::api::{Command, cmd};

/// Global event carrying the result of a background check: `Some(`[`UpdateInfo`]`)` when it
/// found an update, `None` when it found none after an earlier one did (a release pulled).
pub const EVENT_UPDATE_AVAILABLE: &str = "update_available";

/// Payload of `app_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// The bundle's version (`Cargo.toml` workspace version), e.g. `0.1.0`.
    pub version: String,
}

/// A newer release than the running one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    /// A breaking release (a higher major, or a higher minor while both majors are 0): the UI
    /// blocks until it is installed.
    pub required: bool,
    /// The release notes, as published.
    pub notes: Option<String>,
    /// Why the previous install of an update failed (kept across the restart that followed
    /// it): the UI shows it and offers a way out of the blocking modal.
    #[serde(default)]
    pub install_error: Option<String>,
}

/// The page of the latest release, for a manual download when the install fails.
pub const RELEASES_URL: &str = "https://github.com/Marco-O94/ai-task-manager/releases/latest";

cmd!(GetAppInfo, "app_info", Empty => AppInfo);
cmd!(
    /// The result of the latest background check; `None` if none found an update (or checks
    /// are off: debug builds, `ATM_NO_UPDATE_CHECK`). Never goes to the network itself.
    CheckUpdate, "check_update", Empty => Option<UpdateInfo>
);
cmd!(
    /// Downloads and verifies the update found by the latest check, stops the agents (the exit
    /// path's graceful shutdown), installs it and restarts the app: on success it never returns.
    /// `Invalid` if no update is pending.
    InstallUpdate, "install_update", Empty => ()
);
