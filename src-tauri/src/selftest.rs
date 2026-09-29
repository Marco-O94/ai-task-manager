//! Debug-only IPC probes driven by the UI when `ATM_SELFTEST=1` (spec §11.2 M0, M3).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use atm_core::claude::ATM_CLAUDE_PATH_ENV;
use atm_core::{Core, CoreConfig};
use atm_types::debug::{PingReq, ProbeMsg, ReportReq};
use atm_types::{AppError, ErrorCode};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

const PROBE_COUNT: u32 = 50;
const PROBE_BIG: [u32; 3] = [7, 23, 41];
const PROBE_BIG_LEN: usize = 20 * 1024;
const WATCHDOG: Duration = Duration::from_secs(90);
/// Report keys of the transcript checks (spec §11.2 M3-TAURI). The UI reports them as null
/// while the core is the M1 stub, so they are required as soon as it is not ([`core_is_stub`]).
const FULL_KEYS: [&str; 3] = [
    "transcript_subscribe_ok",
    "forwarder_unsub_ok",
    "forwarder_reload_ok",
];
/// Instead of the login shell's: its `.zshrc` has no business in a selftest.
const SELFTEST_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

pub fn selftest_enabled() -> bool {
    env_flag("ATM_SELFTEST")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).as_deref() == Ok("1")
}

/// Ends a selftest run that cannot report: exit code 1, private dir removed.
fn fail(why: &str) -> ! {
    eprintln!("selftest: {why}");
    remove_private_dir();
    std::process::exit(1)
}

/// Data and cache dirs of a selftest run (`$TMPDIR/atm-selftest-<pid>`), used instead of the
/// app's: the probe may run next to an open instance, whose DB and agents it must not touch.
pub fn private_dir() -> PathBuf {
    std::env::temp_dir().join(format!("atm-selftest-{}", std::process::id()))
}

/// A selftest never opens the user's DB or recovers (kills) the agents of an open app: it
/// gets a fresh [`private_dir`] (a dead run with the same pid may have left one behind).
///
/// Nor may it run the user's Claude CLI, yet `claude_path` is only discovery's first
/// candidate (spec §7.1): the run exits 1 unless `fake-claude` was built next to this binary
/// (by `cargo test`/`cargo build`, not by `cargo tauri build`), and `ATM_CLAUDE_PATH` and
/// `HOME` point the next candidates, and any `~/.claude` read, away from the user's.
#[allow(clippy::needless_update)] // M3-CORE may add fields to `CoreConfig`
pub fn core_config() -> CoreConfig {
    let dir = private_dir();
    let _ = std::fs::remove_dir_all(&dir);
    let fake_claude = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("fake-claude")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| {
            fail("no fake-claude next to the binary: run `cargo build -p atm-core --bin fake-claude`")
        });
    CoreConfig {
        data_dir: dir.join("data"),
        cache_dir: dir.join("cache"),
        claude_path: Some(fake_claude.clone()),
        path_env: Some(SELFTEST_PATH.into()),
        extra_env: vec![
            (ATM_CLAUDE_PATH_ENV.into(), fake_claude.into()),
            ("HOME".into(), dir.into()),
        ],
        ..CoreConfig::default()
    }
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
            fail(&format!(
                "no report from the UI within {WATCHDOG:?} (a UI built without \
                 `--features testkit` has no selftest: build with \
                 `--config src-tauri/tauri.testkit.conf.json`)"
            ));
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
pub async fn debug_selftest_report(
    app: AppHandle,
    core: State<'_, Arc<Core>>,
    req: ReportReq,
) -> Result<(), AppError> {
    let passed = report_passed(&req.report, !core_is_stub(&core).await);
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", req.report);
    let _ = out.flush();
    app.exit(if passed { 0 } else { 1 });
    Ok(())
}

/// The M1 stub answers every service with `NotImplemented`; a core past it must pass the
/// transcript checks, so a regression to `NotImplemented` (null) fails the run.
async fn core_is_stub(core: &Core) -> bool {
    matches!(core.get_settings().await, Err(e) if e.code == ErrorCode::NotImplemented)
}

/// Every boolean must be true and `csp_violations` must be 0; with `full`, every
/// [`FULL_KEYS`] check must also have run and passed (null = skipped).
fn report_passed(report: &serde_json::Value, full: bool) -> bool {
    let Some(fields) = report.as_object() else {
        return false;
    };
    report.get("csp_violations").and_then(|v| v.as_u64()) == Some(0)
        && fields.values().all(|v| v.as_bool().unwrap_or(true))
        && (!full
            || FULL_KEYS
                .iter()
                .all(|k| report.get(k) == Some(&true.into())))
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
        assert!(report_passed(&ok, false));
        assert!(!report_passed(
            &json!({"ping_ok": false, "csp_violations": 0}),
            false
        ));
        assert!(!report_passed(
            &json!({"ping_ok": true, "csp_violations": 1}),
            false
        ));
        assert!(!report_passed(&json!({"ping_ok": true}), false));
        assert!(!report_passed(&json!([]), false));
    }

    #[test]
    fn transcript_checks_are_required_only_in_full_mode() {
        let stub = json!({"ping_ok": true, "transcript_subscribe_ok": null,
            "forwarder_unsub_ok": null, "forwarder_reload_ok": null, "csp_violations": 0});
        assert!(report_passed(&stub, false));
        assert!(!report_passed(&stub, true));
        assert!(!report_passed(
            &json!({"ping_ok": true, "csp_violations": 0}),
            true
        ));

        let mut full = stub.clone();
        for k in FULL_KEYS {
            full[k] = true.into();
        }
        assert!(report_passed(&full, true));
        full["forwarder_reload_ok"] = false.into();
        assert!(!report_passed(&full, false));
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
