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
    /// Selftest: `{ping_ok, ping_err_typed, channel_50_in_order, csp_enforced, dialog_ok,
    /// csp_violations, …}`. E2E: `{step_1 … step_12, …, csp_violations, details}`.
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

// ---- in-app E2E (spec §11.2 M4, §12.2): `ATM_E2E=1`, debug builds only -----------------------

/// The run the backend prepared: its phase and the temporary repositories (all under `dir`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct E2eSetup {
    /// `"1"` (until the quit of step 8), `"2"` (after the relaunch) or `"gatekeeper"`.
    pub phase: String,
    pub dir: String,
    /// A repository with one commit on `main`, checked out.
    pub repo: String,
    /// A plain folder.
    pub not_git: String,
    /// A bare repository.
    pub bare: String,
    /// A repository without commits.
    pub empty: String,
    /// A repository with one commit and a `.mcp.json` (warning, not rejection).
    pub mcp_repo: String,
    pub fake_claude: String,
    /// Which of the variables fake-claude records the presence of (`ANTHROPIC_API_KEY`,
    /// `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `GIT_DIR`, …) are set in
    /// the app's own environment (`scripts/e2e.sh` sets them all: the agents must not get them).
    pub app_env: Vec<String>,
    /// The variables of a parent session the app removed from its own environment at startup
    /// by re-executing itself (M6), names only.
    #[serde(default)]
    pub scrubbed_at_start: Vec<String>,
    /// What phase 1 handed over with `debug_e2e_quit`.
    pub phase1: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eAuthReq {
    pub logged_in: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2ePathReq {
    pub path: String,
}

/// The answer of the next native confirmation (spec §10.2): OK (`true`) or Annulla.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eConfirmReq {
    pub accept: bool,
}

/// The login `.command` written by `open_login_terminal`, the `open` calls recorded instead of
/// run, and the script run with `/bin/sh` (it calls fake-claude's `auth login`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eLoginScript {
    pub path: String,
    pub content: String,
    pub mode: u32,
    pub opened: Vec<Vec<String>>,
    pub run_code: Option<i32>,
    pub run_output: String,
}

/// `git <args>` in `repo` (a directory of the run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eGitReq {
    pub repo: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eGitOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eWriteReq {
    pub path: String,
    pub content: String,
}

/// The real login script (`write_login_script`, in a folder of `app_cache_dir`) opened with the
/// real `open_login_terminal`; its `claude` is a wrapper that records its arguments in a
/// marker, then runs fake-claude. `marker_ok`: Terminal ran it (Gatekeeper let it through)
/// and it called `claude auth login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eGatekeeper {
    pub script: String,
    /// The script carries `com.apple.quarantine` (it must not).
    pub quarantined: bool,
    pub marker_ok: bool,
    /// What the wrapper recorded (its arguments).
    pub marker: String,
    pub waited_ms: u64,
}

cmd!(
    /// `None` unless `ATM_E2E=1`; phase 1 has created the repositories before the core started.
    DebugE2eSetup, "debug_e2e_setup", Empty => Option<E2eSetup>
);
cmd!(
    /// What fake-claude's `auth status` answers from now on (`FAKE_CLAUDE_AUTH_FILE`).
    DebugE2eSetAuth, "debug_e2e_set_auth", E2eAuthReq => ()
);
cmd!(
    /// The next native picker, `pick_repo_folder` or `pick_attachment_files` (one slot:
    /// whichever opens first takes it), returns this path instead of opening; with nothing
    /// queued, in a run `pick_repo_folder` returns `None` and `pick_attachment_files` no file.
    DebugE2eQueuePick, "debug_e2e_queue_pick", E2ePathReq => ()
);
cmd!(
    /// The next native confirmation (Trusted, bypass, API key; M6) gets this answer instead of
    /// being shown: the run cannot click a native dialog. One answer per confirmation; one
    /// asked with nothing queued is answered Annulla.
    DebugE2eQueueConfirm, "debug_e2e_queue_confirm", E2eConfirmReq => ()
);
cmd!(
    /// `"<title>: <text>"` of every native confirmation asked since the app started.
    DebugE2eConfirms, "debug_e2e_confirms", Empty => Vec<String>
);
cmd!(DebugE2eLoginScript, "debug_e2e_login_script", Empty => E2eLoginScript);
cmd!(
    /// The lines of `FAKE_CLAUDE_RECORD`, one per fake-claude call or answer.
    DebugE2eRecord, "debug_e2e_record", Empty => Vec<serde_json::Value>
);
cmd!(DebugE2eGit, "debug_e2e_git", E2eGitReq => E2eGitOut);
cmd!(DebugE2eWriteFile, "debug_e2e_write_file", E2eWriteReq => ());
cmd!(DebugE2eExists, "debug_e2e_exists", E2ePathReq => bool);
cmd!(
    /// Pids of the run's agents still alive: the fake-claude calls and `hang_ignore`
    /// grandchildren recorded in its `FAKE_CLAUDE_RECORD`, and any fake-claude `-p` or `sleep`
    /// on the machine whose working directory is inside the run's directory, recorded or not
    /// (never other processes).
    DebugE2eAgents, "debug_e2e_agents", Empty => Vec<i32>
);
cmd!(
    /// `"<command>: <code>"` of every IPC command that failed since the app started.
    DebugE2eFailures, "debug_e2e_failures", Empty => Vec<String>
);
cmd!(
    /// Stores the partial report for phase 2 and presses Cmd+Q: a ⌘Q key event posted to the
    /// app's process through the window server (`CGEventPostToPid`), which AppKit hands to the
    /// menu's Quit (`NSApp terminate:`).
    DebugE2eQuit, "debug_e2e_quit", ReportReq => ()
);
cmd!(
    /// Reloads the page natively (`-[WKWebView reload]`, as the WebView's "Reload" does).
    DebugE2eReload, "debug_e2e_reload", Empty => ()
);
cmd!(
    /// Prints the report on stdout and exits 0 if every check passed, 1 otherwise.
    DebugE2eReport, "debug_e2e_report", ReportReq => ()
);
cmd!(DebugE2eGatekeeper, "debug_e2e_gatekeeper", Empty => E2eGatekeeper);
