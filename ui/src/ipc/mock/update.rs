//! App version and updates in the mock: `?update=optional` or `?update=required` announces a
//! newer release (v0.1.1 or v0.2.0), `?update=required,fail` makes the first install fail, so
//! the banner, the blocking modal and its error/retry state can be seen without a release;
//! `?update=required,installfail` is the app restarted after a failed install (the modal
//! shows why and offers the release page or «Continua per ora»).
//! The install "restarts" by staying in its last state: there is no app to relaunch.

use std::cell::{Cell, RefCell};

use atm_types::*;
use serde_json::Value;

/// Commands served here.
pub const COMMANDS: &[&str] = &[GetAppInfo::NAME, CheckUpdate::NAME, InstallUpdate::NAME];

const VERSION: &str = env!("CARGO_PKG_VERSION");

thread_local! {
    static PENDING: RefCell<Option<UpdateInfo>> = RefCell::new(seed());
    static FAILS_LEFT: Cell<u32> = Cell::new(u32::from(flags().iter().any(|f| f == "fail")));
}

fn flags() -> Vec<String> {
    let search = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("update="))
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_default()
}

fn seed() -> Option<UpdateInfo> {
    let flags = flags();
    let required = flags.iter().any(|f| f == "required");
    if !required && !flags.iter().any(|f| f == "optional") {
        return None;
    }
    Some(UpdateInfo {
        current: VERSION.to_owned(),
        latest: if required { "0.2.0" } else { "0.1.1" }.to_owned(),
        required,
        notes: Some("Correzioni e miglioramenti.".to_owned()),
        install_error: flags.iter().any(|f| f == "installfail").then(|| {
            "Installazione dell'aggiornamento: file system di sola lettura (simulato)".to_owned()
        }),
    })
}

pub async fn handle(cmd: &str, _req: Value) -> Result<Value, AppError> {
    let res = match cmd {
        GetAppInfo::NAME => serde_json::to_value(AppInfo {
            version: VERSION.to_owned(),
        })?,
        CheckUpdate::NAME => serde_json::to_value(PENDING.with_borrow(Clone::clone))?,
        InstallUpdate::NAME => {
            if PENDING.with_borrow(Option::is_none) {
                return Err(AppError::invalid("Nessun aggiornamento disponibile"));
            }
            super::sleep(1500).await;
            if FAILS_LEFT.get() > 0 {
                FAILS_LEFT.set(FAILS_LEFT.get() - 1);
                return Err(AppError::io(
                    "Download dell'aggiornamento: connessione non riuscita (simulato)",
                ));
            }
            Value::Null
        }
        _ => return Err(AppError::not_implemented(cmd)),
    };
    Ok(res)
}
