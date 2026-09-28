//! IPC facade. M1 adds the typed `call::<C: Command>()` and the `mock` backend (spec §6.6).

mod tauri;

pub use tauri::{call, call_with_channel};

/// Error decoded from a rejected command: `{code, message}`.
// TODO(M1): replace with `atm_types::AppError`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
}

impl AppError {
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "Internal".into(),
            message: message.into(),
        }
    }
}
