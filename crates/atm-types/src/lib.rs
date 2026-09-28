//! IPC contract shared by the Tauri shell and the WASM UI (spec §6.2): serde types only.
//! Compiles for both the host and `wasm32-unknown-unknown`. Frozen after M1.
//!
//! Serde field names are the Rust field names (no global `rename_all`); the only renames are the
//! enums whose strings are shared with the DB or the CLI (§5.3, see [`model`]).

pub mod api;
pub mod debug;
pub mod error;
pub mod model;
pub mod review;
pub mod transcript;

pub use api::*;
pub use error::*;
pub use model::*;
pub use review::*;
pub use transcript::*;

/// UUID v4, lowercase.
pub type Id = String;
/// Unix time in milliseconds.
pub type Millis = i64;
