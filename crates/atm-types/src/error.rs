//! Typed command error (spec §6.1, §6.2). A rejected invoke carries `{code, message}`.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppError {
    pub code: ErrorCode,
    /// Human-readable, shown to the user as is (Italian for user-facing failures).
    pub message: String,
}

/// Serialized as the variant name, e.g. `"NotImplemented"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    NotFound,
    Invalid,
    Conflict,
    Busy,
    ConcurrencyLimit,
    UsageLimited,
    ClaudeNotFound,
    NotLoggedIn,
    WorktreeMissing,
    BranchMismatch,
    TargetCheckoutDirty,
    GitIdentityMissing,
    Git,
    Io,
    Db,
    NotImplemented,
    Internal,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Placeholder result of every M1 contract stub; `what` names the function or command.
    pub fn not_implemented(what: &str) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("{what}: not implemented yet"),
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Busy, message)
    }

    pub fn git(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Git, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Io, message)
    }

    pub fn db(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Db, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}

/// `"<Code>: <message>"`, e.g. `NotImplemented: get_env: not implemented yet`.
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::io(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::internal(format!("json: {e}"))
    }
}
