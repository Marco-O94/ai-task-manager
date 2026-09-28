//! Debug-only IPC probes (spec §11.2 M0). The commands exist only in debug builds of the
//! shell; the types are always compiled so the UI can probe for them at runtime.

use serde::{Deserialize, Serialize};

use crate::api::{Command, Empty, cmd};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingReq {
    /// Reply with `AppError{Invalid}` instead of `"pong"`.
    pub fail: bool,
}

/// One message of `debug_channel_probe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeMsg {
    pub i: u32,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportReq {
    /// `{ping_ok, ping_err_typed, channel_50_in_order, csp_enforced, dialog_ok, csp_violations}`.
    pub report: serde_json::Value,
}

cmd!(DebugPing, "debug_ping", PingReq => String);
cmd!(
    /// Also takes `onEvent: Channel<ProbeMsg>`: 50 indexed messages, three of ~20 KiB.
    DebugChannelProbe, "debug_channel_probe", Empty => u32
);
cmd!(
    /// `ATM_SELFTEST=1` in the backend's environment.
    DebugSelftestEnabled, "debug_selftest_enabled", Empty => bool
);
cmd!(
    /// Live transcript forwarders (`Core::forwarder_count`): 0 after a page reload.
    DebugForwarderCount, "debug_forwarder_count", Empty => u32
);
cmd!(
    /// Prints the report on stdout and exits 0 if it passed, 1 otherwise.
    DebugSelftestReport, "debug_selftest_report", ReportReq => ()
);
