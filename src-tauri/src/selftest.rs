//! Debug-only IPC probes driven by the UI when `ATM_SELFTEST=1` (spec §11.2 M0).

use std::io::Write;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri::ipc::Channel;

const PROBE_COUNT: u32 = 50;
const PROBE_BIG: [u32; 3] = [7, 23, 41];
const PROBE_BIG_LEN: usize = 20 * 1024;
const WATCHDOG: Duration = Duration::from_secs(90);

// TODO(M1): replace with `atm_types::AppError`.
#[derive(Debug, Serialize)]
pub struct AppError {
    code: &'static str,
    message: String,
}

#[derive(Deserialize)]
pub struct PingReq {
    fail: bool,
}

#[derive(Clone, Serialize)]
pub struct ProbeMsg {
    i: u32,
    data: String,
}

#[derive(Deserialize)]
pub struct ReportReq {
    report: serde_json::Value,
}

pub fn selftest_enabled() -> bool {
    std::env::var("ATM_SELFTEST").as_deref() == Ok("1")
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
        Err(AppError {
            code: "Invalid",
            message: "debug_ping: failure requested".into(),
        })
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
        on_event.send(ProbeMsg { i, data }).map_err(|e| AppError {
            code: "Internal",
            message: e.to_string(),
        })?;
    }
    Ok(PROBE_COUNT)
}

#[tauri::command]
pub async fn debug_selftest_enabled() -> Result<bool, AppError> {
    Ok(selftest_enabled())
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
    fn app_error_serializes_as_code_and_message() {
        let e = AppError {
            code: "Invalid",
            message: "x".into(),
        };
        assert_eq!(
            serde_json::to_value(e).unwrap(),
            json!({"code": "Invalid", "message": "x"})
        );
    }
}
