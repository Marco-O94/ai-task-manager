//! Turn lifecycle: preflight, spawn, stdout routing, approvals, stop sequence, finalize
//! (spec §7.7–§7.9). Owner: M3-CORE, which adds the turn driver itself; the items below are
//! the pieces with a fixed contract (timings, classification table, capped logs).
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use atm_types::{AppError, ProcessStatus, StopReason};

use crate::normalize::TurnResult;

/// Wait for the `initialize` response (spec §7.4 step 1).
pub const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// From `result` to process exit (spec §7.4 step 3).
pub const EXIT_AFTER_RESULT: Duration = Duration::from_secs(30);
/// Budget of `Core::shutdown` (spec §7.9).
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(8);
/// Raw log caps (spec §7.5).
pub const MAX_STDOUT_LOG: u64 = 64 << 20;
pub const MAX_STDERR_LOG: u64 = 8 << 20;

/// Phases of the stop sequence (spec §7.9): interrupt → wait `interrupt`; close stdin →
/// wait `eof`; SIGTERM the group → wait `term`; SIGKILL and reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopTimings {
    pub interrupt: Duration,
    pub eof: Duration,
    pub term: Duration,
}

impl StopTimings {
    /// User stop: 5/3/3 s (≤ 13 s in total).
    pub const NORMAL: Self = Self {
        interrupt: Duration::from_secs(5),
        eof: Duration::from_secs(3),
        term: Duration::from_secs(3),
    };
    /// App shutdown: 2/2/2 s.
    pub const SHUTDOWN: Self = Self {
        interrupt: Duration::from_secs(2),
        eof: Duration::from_secs(2),
        term: Duration::from_secs(2),
    };
}

/// Why the app stopped a turn; the final state depends on this flag, not on the `result`
/// subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    User,
    Shutdown,
}

impl StopCause {
    pub fn stop_reason(self) -> StopReason {
        match self {
            Self::User => StopReason::UserStop,
            Self::Shutdown => StopReason::AppShutdown,
        }
    }
}

/// What the runner observed about one turn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnOutcome {
    pub result: Option<TurnResult>,
    /// `None` = terminated by a signal or never reaped.
    pub exit_code: Option<i32>,
    pub stop: Option<StopCause>,
    pub spawn_error: bool,
    pub init_timeout: bool,
    /// `result` arrived but the process did not exit within [`EXIT_AFTER_RESULT`].
    pub exit_timeout: bool,
}

/// The classification table of spec §7.7 → final `(status, stop_reason)`.
pub fn classify(outcome: &TurnOutcome) -> (ProcessStatus, Option<StopReason>) {
    todo!("M3-CORE: classify")
}

/// `<data_dir>/logs/<attempt_id>/<process_id>` (spec §4); files inside are 0600.
pub fn log_dir(data_dir: &Path, attempt_id: &str, process_id: &str) -> PathBuf {
    todo!("M3-CORE: log_dir")
}

/// Append-only log file (`stdout.jsonl`, `stderr.log`) that stops writing at `cap` bytes.
#[derive(Debug)]
pub struct CappedLog {
    file: tokio::fs::File,
    written: u64,
    cap: u64,
}

impl CappedLog {
    /// Creates the file with mode 0600.
    pub async fn create(path: &Path, cap: u64) -> Result<CappedLog, AppError> {
        Err(AppError::not_implemented("CappedLog::create"))
    }

    /// Appends `line` and `\n` while under the cap. Returns `true` exactly once, when the cap
    /// first drops a line (the caller then emits a warning Notice).
    pub async fn write_line(&mut self, line: &[u8]) -> std::io::Result<bool> {
        Err(std::io::Error::other("M3-CORE: CappedLog::write_line"))
    }
}
