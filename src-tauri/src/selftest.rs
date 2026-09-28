//! Debug-only IPC probes driven by the UI when `ATM_SELFTEST=1` (spec §11.2 M0).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use atm_core::Core;
use atm_types::AppError;
use atm_types::debug::{PingReq, ProbeMsg, ReportReq};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

const PROBE_COUNT: u32 = 50;
const PROBE_BIG: [u32; 3] = [7, 23, 41];
const PROBE_BIG_LEN: usize = 20 * 1024;
const WATCHDOG: Duration = Duration::from_secs(90);

pub fn selftest_enabled() -> bool {
    std::env::var("ATM_SELFTEST").as_deref() == Ok("1")
}

/// Data and cache dirs of a selftest run (`$TMPDIR/atm-selftest-<pid>`), used instead of the
/// app's: the probe may run next to an open instance, whose DB and agents it must not touch.
pub fn private_dir() -> PathBuf {
    std::env::temp_dir().join(format!("atm-selftest-{}", std::process::id()))
}

/// Best effort, at exit.
pub fn remove_private_dir() {
    if selftest_enabled() {
        let _ = std::fs::remove_dir_all(private_dir());
    }
}

/// Fails the selftest run if the UI never reports (e.g. the WASM did not load).
pub fn start_watchdog() {
    if selftest_enabled() {
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            eprintln!("selftest: no report from the UI within {WATCHDOG:?}");
            std::process::exit(1);
        });
    }
}

#[tauri::command]
pub async fn debug_ping(req: PingReq) -> Result<String, AppError> {
    if req.fail {
        Err(AppError::invalid("debug_ping: failure requested"))
    } else {
        Ok("pong".into())
    }
}

/// Sends `PROBE_COUNT` indexed messages; the `PROBE_BIG` ones carry ~20 KiB, which
/// Tauri delivers through a different path than small payloads.
#[tauri::command]
pub async fn debug_channel_probe(on_event: Channel<ProbeMsg>) -> Result<u32, AppError> {
    for i in 0..PROBE_COUNT {
        let data = if PROBE_BIG.contains(&i) {
            char::from(b'a' + (i % 26) as u8)
                .to_string()
                .repeat(PROBE_BIG_LEN)
        } else {
            String::new()
        };
        on_event
            .send(ProbeMsg { i, data })
            .map_err(|e| AppError::internal(e.to_string()))?;
    }
    Ok(PROBE_COUNT)
}

#[tauri::command]
pub async fn debug_selftest_enabled() -> Result<bool, AppError> {
    Ok(selftest_enabled())
}

#[tauri::command]
pub async fn debug_forwarder_count(core: State<'_, Arc<Core>>) -> Result<u32, AppError> {
    Ok(core.forwarder_count().try_into().unwrap_or(u32::MAX))
}

/// Prints the UI's report on stdout and exits: 0 if every check passed, 1 otherwise.
#[tauri::command]
pub async fn debug_selftest_report(app: AppHandle, req: ReportReq) -> Result<(), AppError> {
    let passed = report_passed(&req.report);
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", req.report);
    let _ = out.flush();
    app.exit(if passed { 0 } else { 1 });
    Ok(())
}

/// Every boolean must be true and `csp_violations` must be 0.
fn report_passed(report: &serde_json::Value) -> bool {
    let Some(fields) = report.as_object() else {
        return false;
    };
    report.get("csp_violations").and_then(|v| v.as_u64()) == Some(0)
        && fields.values().all(|v| v.as_bool().unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atm_types::Command;
    use atm_types::debug::{
        DebugChannelProbe, DebugForwarderCount, DebugPing, DebugSelftestEnabled,
        DebugSelftestReport,
    };
    use serde_json::json;

    #[test]
    fn report_passes_only_when_all_checks_hold() {
        let ok = json!({"ping_ok": true, "ping_err_typed": true, "channel_50_in_order": true, "csp_violations": 0});
        assert!(report_passed(&ok));
        assert!(!report_passed(
            &json!({"ping_ok": false, "csp_violations": 0})
        ));
        assert!(!report_passed(
            &json!({"ping_ok": true, "csp_violations": 1})
        ));
        assert!(!report_passed(&json!({"ping_ok": true})));
        assert!(!report_passed(&json!([])));
    }

    #[test]
    fn probe_fns_match_marker_names() {
        let _ = (
            debug_ping,
            debug_channel_probe,
            debug_selftest_enabled,
            debug_forwarder_count,
            debug_selftest_report,
        );
        assert_eq!(DebugPing::NAME, "debug_ping");
        assert_eq!(DebugChannelProbe::NAME, "debug_channel_probe");
        assert_eq!(DebugSelftestEnabled::NAME, "debug_selftest_enabled");
        assert_eq!(DebugForwarderCount::NAME, "debug_forwarder_count");
        assert_eq!(DebugSelftestReport::NAME, "debug_selftest_report");
    }
}
