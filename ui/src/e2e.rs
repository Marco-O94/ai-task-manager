//! In-app E2E of spec §12.2 (M4). Runs only when the debug backend reports `ATM_E2E=1`
//! (`debug_e2e_setup`). Compiled only with `--features testkit` (debug bundles), never in the
//! release WASM (M6), and not with `--features mock`.
//!
//! It drives the real DOM of the running WKWebView with events (clicks, `input`/`change`,
//! keys, HTML5 drag events carrying a `DataTransfer`) and checks the outcome in the DOM, over
//! IPC reads and through the backend's debug helpers (fake-claude's record, git, files).
//! `scripts/e2e.sh` runs two launches of the app on the same data:
//!
//! - phase 1: steps 1–7 and step 8 up to the quit, with two native page reloads
//!   (`-[WKWebView reload]`) carried over in `sessionStorage` (step 4's order re-read from the
//!   DB, the resubscription check after step 7). The quit runs during `[fake:hang]` and
//!   `[fake:hang_ignore]` turns: a ⌘Q key event posted through the window server (the menu's
//!   Quit → `NSApp terminate:` → `RunEvent::Exit`);
//! - phase 2: step 4's order after the real process restart, the rest of step 8, steps 9–12,
//!   then the report on stdout; the app then exits through `app.exit` (`ExitRequested`)
//!   during one more `[fake:hang_ignore]` turn, which the script checks afterwards;
//! - phase 3 (M6, also alone with `scripts/e2e.sh --perf`): `[fake:flood]` on three concurrent
//!   attempts, one of them open, with the page's responsiveness and the transcript's DOM window
//!   measured ([`perf_flood`]).
//!
//! Step 2 runs inside step 1, at the login gate: "Accedi" is only on the gate. Every IPC
//! command that fails is counted by the backend: only the three rejected folders of step 3 may.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;

use atm_types::debug::{
    DebugE2eAgents, DebugE2eConfirms, DebugE2eExists, DebugE2eFailures, DebugE2eGatekeeper,
    DebugE2eGit, DebugE2eLoginScript, DebugE2eQueueConfirm, DebugE2eQueuePick, DebugE2eQuit,
    DebugE2eRecord, DebugE2eReload, DebugE2eReport, DebugE2eSetAuth, DebugE2eSetup,
    DebugForwarderCount, E2eAuthReq, E2eConfirmReq, E2eGitOut, E2eGitReq, E2ePathReq, E2eSetup,
    ReportReq,
};
use atm_types::{
    AttemptIdReq, AttemptState, CONTINUE_PROMPT, ConfigPolicy, Empty, Entry, EntryBody, FileStatus,
    GetBoard, GetBranchStatus, GetDiff, GetEntries, GetEntriesReq, GetEnv, GetEnvReq,
    GetTaskDetail, IdReq, ListProjects, ProcessInfo, ProcessStatus, Project, ProjectIdReq,
    StopReason, TaskCard, TaskDetail, TaskStatus, ToolStatus, WorktreeState,
};
use js_sys::{Array, Function, Object, Reflect};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, HtmlElement};

use crate::ipc;
use crate::selftest::{channel_in_order, csp_violations, sleep};

/// `sessionStorage` key carrying the run across the page reloads of phase 1.
const STATE_KEY: &str = "atm-e2e-state";
const T1_TITLE: &str = "Crea hello [fake:approval]";
const T2_TITLE: &str = "Secondo task";
/// T3 and T6 both append their title to hello.txt (`[fake:append]`): once T6 is merged, T3
/// conflicts with main (step 10).
const T3_TITLE: &str = "Conflitto su hello [fake:append]";
const T6_TITLE: &str = "Hola su hello [fake:append]";
const T4_TITLE: &str = "Limite d'uso [fake:usage_limit]";
/// Runs through the quit of step 8 next to T1's `[fake:hang]`: ignores interrupt, EOF and
/// SIGTERM, with a `sleep 300` grandchild, so only the shutdown's `killpg` SIGKILL ends it.
const T5_TITLE: &str = "Ignora lo stop [fake:hang_ignore]";
/// The text `simple` streams word by word (`FAKE_CLAUDE_DELTA_MS` apart).
const STREAMED: &str = "Creo hello.txt nel worktree.";
/// The Notice of a turn stopped by the app's shutdown (`runner::turn::end_notice`).
const SHUTDOWN_NOTICE: &str = "Esecuzione fermata alla chiusura dell'app";
/// Variables every agent must be spared (spec §7.2), a parent Claude Code session's and its
/// host's included (M6), with the `NODE_OPTIONS` a cmux terminal sets when the user has none
/// (marker `CMUX_ORIGINAL_NODE_OPTIONS_PRESENT=0`), set in the app's environment by
/// `scripts/e2e.sh`; fake-claude records their presence.
const SCRUBBED: [&str; 18] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "GIT_DIR",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_SSE_PORT",
    "ENABLE_IDE_INTEGRATION",
    "CMUX_SOCKET_PATH",
    "CMUX_CUA_AUTH_TOKEN_FILE",
    "CMUX_ORIGINAL_NODE_OPTIONS_PRESENT",
    "NODE_OPTIONS",
];
/// Of [`SCRUBBED`], those the app keeps in its own environment (the API keys, for the opt-in
/// passthrough; `GIT_DIR`, removed per child): the others it removes from itself at startup by
/// re-executing (M6), so `ps -E` of the app does not show them.
const KEPT_BY_THE_APP: [&str; 3] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "GIT_DIR"];
/// A user Stop of a turn that ignores the interrupt, EOF and SIGTERM ends with the SIGKILL of
/// its group: interrupt 5 s + EOF 3 s + SIGTERM 3 s (spec §7.9), within the 13 s objective.
const ESCALATION_MS: (f64, f64) = (10_000.0, 13_000.0);
/// The only command failures the run provokes: the three folders of step 3.
const EXPECTED_FAILURES: [&str; 3] = ["add_project: Invalid"; 3];
/// Default wait of a UI reaction.
const UI: u32 = 10_000;
/// Wait of a whole agent turn.
const TURN: u32 = 30_000;
/// Phase 3: three `[fake:flood]` tasks, each turn [`FLOOD_EVENTS`] assistant texts.
const FLOOD_TITLES: [&str; 3] = [
    "Flood A [fake:flood]",
    "Flood B [fake:flood]",
    "Flood C [fake:flood]",
];
/// `FAKE_CLAUDE_FLOOD_EVENTS` of the run (fake-claude's default, set by the backend).
const FLOOD_EVENTS: u32 = 10_000;
/// Rows the transcript may keep in the DOM (spec §9.4, `state::transcript::MAX_ROWS`).
const MAX_DOM_ROWS: usize = 300;
/// While the floods stream: the longest the page's event loop may stay busy (how late a 20 ms
/// timer may fire), and the slowest a tab switch of the open panel may show.
const MAX_STALL_MS: f64 = 1_000.0;
const MAX_INTERACTION_MS: f64 = 1_500.0;
/// Samples of the 20 ms timer taken with the page visible (about 2 s) below which the phase
/// fails as not measured: WebKit fires the timers of a hidden page (window covered, app hidden,
/// display off) once a second.
const MIN_VISIBLE_SAMPLES: u32 = 100;
/// Wait of the three flood turns.
const FLOOD_WAIT: u32 = 150_000;

type R<T = ()> = Result<T, String>;

/// What survives reloads (in `sessionStorage`) and the relaunch (through `debug_e2e_quit`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// Where phase 1 resumes after a reload: 0 start, 1 after step 4's, 2 after step 7's.
    resume: u32,
    /// `step_N` and the other checks: booleans.
    report: Map<String, Value>,
    /// One line per check: what was verified, or why it failed.
    details: Map<String, Value>,
    /// CSP violations of the page loads before this one.
    csp: u32,
    t1: String,
    t2: String,
    t3: String,
    t4: String,
    t5: String,
    t6: String,
    /// T1's CLI session (`--session-id` of its first turn).
    session: String,
    /// Positions of T3, T6 and T2 after step 4 (checked again after the relaunch).
    t3_position: f64,
    t6_position: f64,
    t2_position: f64,
    /// Transcript rows of T1 before step 7's reload.
    entries: usize,
    /// Transcript forwarders just before step 7's reload (T1's panel is open).
    forwarders: u32,
    /// From step 7's click on Stop to the end of the `[fake:slow]` turn.
    stop_ms: f64,
    /// Same for the `[fake:hang_ignore]` turn (the stop escalates to SIGKILL).
    stop_escalated_ms: f64,
}

struct Run {
    setup: E2eSetup,
    st: State,
}

enum Next {
    Reload,
    Quit,
    Report,
}

pub async fn run_if_enabled() {
    let Ok(Some(setup)) = ipc::call::<DebugE2eSetup>(&Empty {}).await else {
        return;
    };
    let st = match setup.phase.as_str() {
        "2" => setup
            .phase1
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        _ => load_state().unwrap_or_default(),
    };
    let mut run = Run { setup, st };
    let next = match run.setup.phase.as_str() {
        "1" => phase1(&mut run).await,
        "2" => phase2(&mut run).await,
        "3" => phase3(&mut run).await,
        "gatekeeper" => gatekeeper(&mut run).await,
        other => Err(format!("unknown phase {other}")),
    };
    match next {
        Ok(Next::Reload) => {
            run.st.csp += csp_violations();
            store_state(&run.st);
            // The native reload unloads this page: its answer may never come.
            let why = match ipc::call::<DebugE2eReload>(&Empty {}).await {
                Err(e) => e.to_string(),
                Ok(()) => {
                    sleep(10_000).await;
                    "the page was not reloaded within 10 s".into()
                }
            };
            let _ = load_state();
            run.fail("reload", why);
            run.report().await;
        }
        Ok(Next::Quit) => {
            run.st.csp += csp_violations();
            let state = serde_json::to_value(&run.st).unwrap_or_default();
            if let Err(e) = ipc::call::<DebugE2eQuit>(&ReportReq { report: state }).await {
                run.fail("quit", e.to_string());
                run.report().await;
            }
        }
        Ok(Next::Report) => run.report().await,
        Err(e) => {
            run.fail("aborted", e);
            run.report().await;
        }
    }
}

impl Run {
    /// Stores a check's outcome; an error ends the phase.
    fn check(&mut self, key: &str, result: R<String>) -> R {
        let (ok, text) = match result {
            Ok(text) => (true, text),
            Err(e) => (false, format!("{e} | DOM: {}", dom_summary())),
        };
        leptos::logging::log!("e2e {key}: {} {text}", if ok { "ok" } else { "FAILED" });
        self.st.report.insert(key.into(), ok.into());
        self.st.details.insert(key.into(), text.clone().into());
        if ok {
            Ok(())
        } else {
            Err(format!("{key}: {text}"))
        }
    }

    fn fail(&mut self, key: &str, why: String) {
        self.st.report.insert(key.into(), false.into());
        self.st.details.insert(key.into(), why.into());
    }

    /// The report on stdout (the backend exits 0 only if it passed): every step (missing =
    /// false), the other checks, the details and the CSP violations of every page load.
    async fn report(&mut self) {
        let mut report = Map::new();
        if matches!(self.setup.phase.as_str(), "1" | "2") {
            for n in 1..=12 {
                let key = format!("step_{n}");
                let ok = self.st.report.get(&key).cloned().unwrap_or(false.into());
                report.insert(key, ok);
            }
        }
        for (k, v) in &self.st.report {
            report.entry(k.clone()).or_insert(v.clone());
        }
        report.insert(
            "csp_violations".into(),
            (self.st.csp + csp_violations()).into(),
        );
        report.insert("details".into(), Value::Object(self.st.details.clone()));
        let report = Value::Object(report);
        if let Err(e) = ipc::call::<DebugE2eReport>(&ReportReq { report }).await {
            leptos::logging::error!("e2e report failed: {e}");
        }
    }
}

// ---- phases -----------------------------------------------------------------------------------

async fn phase1(run: &mut Run) -> R<Next> {
    if run.st.resume == 0 {
        let r = steps_1_2(run).await;
        run.check("step_1", r)?;
        let r = step_3(run).await;
        run.check("step_3", r)?;
        let r = step_4_before_restart(run).await;
        run.check("step_4_before_restart", r)?;
        run.st.resume = 1;
        return Ok(Next::Reload);
    }
    if run.st.resume == 1 {
        let r = step_4_reload(run).await;
        run.check("step_4_reload", r)?;
        let r = step_5(run).await;
        run.check("step_5", r)?;
        let r = step_6(run).await;
        run.check("step_6", r)?;
        let r = step_7(run).await;
        run.check("step_7", r)?;
        sleep(500).await; // the stop's last entries (notices) reach the view
        run.st.entries = entries().len();
        run.st.forwarders = forwarder_count().await?;
        run.st.resume = 2;
        return Ok(Next::Reload);
    }
    let r = reload_resubscribes(run).await;
    run.check("reload_resubscribe_ok", r)?;
    let r = step_8_quit(run).await;
    run.check("step_8_before_quit", r)?;
    let r = only_expected_failures(&EXPECTED_FAILURES).await;
    run.check("command_failures_phase1", r)?;
    Ok(Next::Quit)
}

async fn phase2(run: &mut Run) -> R<Next> {
    let r = step_4_relaunch(run).await;
    run.check("step_4", r)?;
    let r = step_8_relaunch(run).await;
    run.check("step_8", r)?;
    let r = step_9(run).await;
    run.check("step_9", r)?;
    let r = step_10(run).await;
    run.check("step_10", r)?;
    let r = step_11(run).await;
    run.check("step_11", r)?;
    let r = step_12(run).await;
    run.check("step_12", r)?;
    let r = security_confirmations().await;
    run.check("security_confirmations", r)?;
    // The bundle's CSP is in force (so 0 violations means something): a constant `eval` must
    // be blocked. Its own violation event is not counted.
    let enforced = js_sys::eval("1").is_err();
    let r = if enforced {
        Ok("eval blocked by the CSP of the embedded assets".into())
    } else {
        Err("eval allowed: the CSP is not applied".into())
    };
    run.check("csp_enforced", r)?;
    let r = only_expected_failures(&["set_project_security: Invalid"]).await;
    run.check("command_failures_phase2", r)?;
    let r = exit_during_turn(run).await;
    run.check("exit_requested_armed", r)?;
    let r = child_env_scrubbed(run).await;
    run.check("child_env_scrubbed", r)?;
    Ok(Next::Report)
}

/// M6 perf (spec §11.2 M6): after phase 2 on the same data, or alone on fresh data
/// (`scripts/e2e.sh --perf`).
async fn phase3(run: &mut Run) -> R<Next> {
    let r = perf_flood(run).await;
    let measured = match &r {
        Ok((_, true)) => "measured",
        Ok((_, false)) => "not measured (page hidden)",
        Err(_) => "failed",
    };
    run.st
        .report
        .insert("perf_responsiveness".into(), measured.into());
    let r = r.map(|(summary, _)| summary);
    run.check("perf_flood", r)?;
    let r = only_expected_failures(&[]).await;
    run.check("command_failures_phase3", r)?;
    Ok(Next::Report)
}

/// `[fake:flood]` on three attempts at once (Agenti in parallelo = 3, set in the settings),
/// the last one open in the panel. While they stream, a 20 ms timer never fires more than
/// [`MAX_STALL_MS`] late (the page's event loop stays free), switching the open panel to
/// Modifiche and back to Agente shows within [`MAX_INTERACTION_MS`] each, and the transcript
/// never has more than [`MAX_DOM_ROWS`] rows in the DOM. Afterwards the three turns overlapped
/// in time and completed, each with its last text among the newest stored entries and an
/// index past [`FLOOD_EVENTS`] (every text being stored is counted in the DB by
/// `scripts/e2e.sh`), and the open transcript ends on its TurnEnd with at most
/// [`MAX_DOM_ROWS`] rows.
///
/// The two timings need the page visible: WebKit fires a hidden page's timers (screen locked,
/// window covered, app hidden) once a second. Then they are not measured (`false` with the
/// summary: the report says so and `scripts/e2e.sh` warns, `--perf` alone fails) while every
/// other check still applies.
async fn perf_flood(run: &Run) -> R<(String, bool)> {
    perf_board(run).await?;
    set_max_running(3).await?;
    let env = ipc::call::<GetEnv>(&GetEnvReq { force: false })
        .await
        .map_err(|e| e.to_string())?;
    if env.max_running != 3 {
        return Err(format!(
            "max_running {} after the settings",
            env.max_running
        ));
    }
    let mut tasks = Vec::new();
    for title in FLOOD_TITLES {
        tasks.push(create_task("Da fare", title, "").await?);
    }

    // The probe, until `done`: how late a 20 ms timer fires, over the samples taken with the
    // page visible (a hidden page's timers fire once a second: the backend floats the window
    // above the others in this phase, and the testkit window is never suspended,
    // `backgroundThrottling: disabled`), and the DOM rows.
    let visible_now = || {
        Reflect::get(&document(), &"visibilityState".into())
            .ok()
            .and_then(|v| v.as_string())
            .is_some_and(|v| v == "visible")
    };
    let stall = Rc::new(Cell::new(0.0_f64));
    let rows = Rc::new(Cell::new(0_usize));
    let ticks = Rc::new(Cell::new(0_u32));
    let visible = Rc::new(Cell::new(0_u32));
    let done = Rc::new(Cell::new(false));
    {
        let (stall, rows, ticks, visible, done) = (
            stall.clone(),
            rows.clone(),
            ticks.clone(),
            visible.clone(),
            done.clone(),
        );
        leptos::task::spawn_local(async move {
            while !done.get() {
                let (t0, shown) = (js_sys::Date::now(), visible_now());
                sleep(20).await;
                if shown && visible_now() {
                    stall.set(stall.get().max(js_sys::Date::now() - t0 - 20.0));
                    visible.set(visible.get() + 1);
                }
                rows.set(rows.get().max(entries().len()));
                ticks.set(ticks.get() + 1);
            }
        });
    }
    let started = js_sys::Date::now();
    let measured = async {
        for task in &tasks {
            start_attempt(task, "acceptEdits").await?;
        }
        let open = tasks.last().cloned().unwrap_or_default();
        // Rows of the flood are streaming into the open panel.
        until("flood rows in the panel", TURN, || {
            (entries().len() > 20 && badge(&open, "running").is_some()).then_some(())
        })
        .await?;
        let mut switches = Vec::new();
        for label in ["Modifiche", "Agente"] {
            let (t0, shown) = (js_sys::Date::now(), visible_now());
            tab(label).await?;
            let sel = if label == "Agente" {
                "[data-view=task-panel] [data-entry]"
            } else {
                "[data-view=task-panel] [data-view=diff]"
            };
            wait_q(sel).await?;
            // Measured only with the page visible (the wait polls a timer).
            switches.push((shown && visible_now()).then(|| js_sys::Date::now() - t0));
        }
        let mut finished = Vec::new();
        for task in &tasks {
            let task = task.clone();
            let p = until_async(&format!("flood turn of {task}"), FLOOD_WAIT, || {
                let task = task.clone();
                async move {
                    let d = detail(&task).await.ok()?;
                    let last = d.processes.last()?.clone();
                    (last.status != ProcessStatus::Running).then_some(last)
                }
            })
            .await?;
            finished.push(p);
        }
        until("TurnEnd in the open panel", TURN, || {
            q("[data-view=task-panel] [data-turn-end]")
        })
        .await?;
        R::Ok((switches, finished))
    }
    .await;
    let elapsed = js_sys::Date::now() - started;
    sleep(500).await; // the last rows of the open transcript
    done.set(true);
    let (switches, finished) = measured?;
    let (stall, rows, ticks) = (stall.get(), rows.get().max(entries().len()), ticks.get());
    let visible = visible.get();

    let mut problems = Vec::new();
    for p in &finished {
        if p.status != ProcessStatus::Completed {
            problems.push(format!("turn {} ended {:?}", p.id, p.status));
        }
    }
    let first_end = finished
        .iter()
        .filter_map(|p| p.finished_at)
        .min()
        .unwrap_or_default();
    let last_start = finished
        .iter()
        .map(|p| p.started_at)
        .max()
        .unwrap_or_default();
    if last_start >= first_end {
        problems.push(format!(
            "the turns did not overlap (last start {last_start}, first end {first_end})"
        ));
    }
    for task in &tasks {
        let (attempt, _, _) = active_attempt(task).await?;
        let newest = attempt_entries(&attempt).await?;
        let last_text = format!("Evento {}", FLOOD_EVENTS - 1);
        let stored = newest
            .iter()
            .any(|e| matches!(&e.body, EntryBody::AssistantText { text } if *text == last_text));
        let max_idx = newest.iter().map(|e| e.idx).max().unwrap_or_default();
        if !stored || max_idx < FLOOD_EVENTS {
            problems.push(format!(
                "{task}: {last_text:?} stored {stored}, newest idx {max_idx}"
            ));
        }
    }
    let measured = visible >= MIN_VISIBLE_SAMPLES && switches.iter().all(Option::is_some);
    if stall > MAX_STALL_MS {
        problems.push(format!("event loop stalled {stall:.0} ms"));
    }
    if let Some(slow) = switches
        .iter()
        .flatten()
        .find(|ms| **ms > MAX_INTERACTION_MS)
    {
        problems.push(format!("tab switch took {slow:.0} ms"));
    }
    if rows > MAX_DOM_ROWS {
        problems.push(format!("{rows} transcript rows in the DOM"));
    }
    let summary = format!(
        "{}3 concurrent turns × {FLOOD_EVENTS} texts in {:.1} s (overlap {:.1} s); longest \
         event-loop stall {stall:.0} ms over the {visible} samples of a 20 ms timer taken with \
         the page visible ({ticks} in all), tab switches Modifiche/Agente {} ms, at most \
         {rows} transcript rows in the DOM",
        if measured {
            ""
        } else {
            "RESPONSIVENESS NOT MEASURED (the page was hidden: screen locked, window covered or \
             app hidden; WebKit fires a hidden page's timers once a second). "
        },
        elapsed / 1000.0,
        (first_end - last_start) as f64 / 1000.0,
        switches
            .iter()
            .map(|ms| ms.map_or_else(|| "hidden".to_owned(), |ms| format!("{ms:.0}")))
            .collect::<Vec<_>>()
            .join("/")
    );
    if problems.is_empty() {
        Ok((summary, measured))
    } else {
        Err(format!("{problems:?} ({summary})"))
    }
}

/// The board of main: after phase 2 it exists; alone (`--perf`, fresh data, fake-claude logged
/// in by the backend) the run adds it first.
async fn perf_board(run: &Run) -> R {
    until("board", 20_000, || q("[data-view=sidebar]")).await?;
    if main_project().await.is_err() {
        let since = last_toast();
        add_repository(&run.setup.repo).await?;
        toast_after(since, "main added", |t| {
            t.contains("Progetto «main» aggiunto")
        })
        .await?;
    }
    select_main().await
}

/// Agenti in parallelo = `n` from the app settings; the dialog closes on success.
async fn set_max_running(n: u32) -> R {
    let dialog = open_settings("Generali").await?;
    let save = until("settings loaded", UI, || {
        find_in(&dialog, "[data-action=save-settings]").filter(|b| !b.has_attribute("disabled"))
    })
    .await?;
    let select = find_in(&dialog, "#settings-max-running").ok_or("no Agenti in parallelo")?;
    set_select(&select, &n.to_string())?;
    let since = last_toast();
    click(&save);
    toast_after(since, "Impostazioni salvate", |t| {
        t.contains("Impostazioni salvate")
    })
    .await?;
    until("settings closed", UI, || {
        open_dialog("Settings").is_none().then_some(())
    })
    .await
}

/// M6: the native confirmations guard every raise of trust and nothing else. They are answered
/// by the run (`debug_e2e_queue_confirm`: a native dialog cannot be clicked from the page),
/// which still sees each one asked and its text. On main: Attendibile with Annulla leaves it
/// Isolated (the refused command is the phase's one expected failure); with OK it is Trusted
/// and in effect, as the project settings say; the bypass opt-in with OK is allowed; lowering
/// both asks nothing. In the app settings the API key passthrough with OK shows the topbar
/// banner (`scripts/e2e.sh` gives the app a dummy key), turning it off asks nothing and hides
/// it: no agent runs meanwhile, and `child_env_scrubbed` sees no key in any agent.
async fn security_confirmations() -> R<String> {
    select_main().await?;
    let before = main_project().await?;
    if before.config_policy != ConfigPolicy::Isolated || before.allow_bypass {
        return Err(format!("main before: {before:?}"));
    }
    let settings = open_settings("Progetto").await?;
    let policy = find_in(&settings, "#project-policy").ok_or("no policy select")?;
    let bypass = find_in(&settings, "#project-bypass").ok_or("no bypass checkbox")?;
    let apply = find_in(&settings, "[data-action=apply-security]").ok_or("no Applica")?;
    let asked = confirms().await?.len();

    set_select(&policy, "trusted")?;
    queue_confirm(false).await?;
    let since = last_toast();
    click(&apply);
    toast_after(since, "Operazione annullata", |t| t.contains("annullata")).await?;
    let p = main_project().await?;
    if confirms().await?.len() != asked + 1 || p.config_policy != ConfigPolicy::Isolated {
        return Err(format!("after Annulla: {p:?}"));
    }

    queue_confirm(true).await?;
    let since = last_toast();
    click(&apply);
    toast_after(since, "sicurezza aggiornata", |t| {
        t.contains("Sicurezza del progetto")
    })
    .await?;
    let p = main_project().await?;
    if (p.config_policy, p.trusted) != (ConfigPolicy::Trusted, true) {
        return Err(format!("after OK: {p:?}"));
    }
    until("approved configuration noted", UI, || {
        find_in(&settings, "[data-trust=trusted]")
    })
    .await?;
    let texts = confirms().await?;
    let trusted_text = texts.last().cloned().unwrap_or_default();
    if texts.len() != asked + 2
        || !trusted_text.contains(&format!("«{}»", p.repo_path))
        || !trusted_text.contains("hook")
        || !trusted_text.contains("non è una sandbox")
    {
        return Err(format!("Trusted confirmation {texts:?}"));
    }

    click(&bypass);
    queue_confirm(true).await?;
    let since = last_toast();
    click(&apply);
    toast_after(since, "bypass consentito", |t| {
        t.contains("Sicurezza del progetto")
    })
    .await?;
    let p = main_project().await?;
    let texts = confirms().await?;
    let bypass_text = texts.last().cloned().unwrap_or_default();
    if !p.allow_bypass
        || texts.len() != asked + 3
        || !bypass_text.contains("Autonoma")
        || bypass_text.contains("hook")
    {
        return Err(format!("bypass: {p:?}, confirmation {texts:?}"));
    }

    click(&bypass);
    set_select(&policy, "isolated")?;
    let since = last_toast();
    click(&apply);
    toast_after(since, "sicurezza abbassata", |t| {
        t.contains("Sicurezza del progetto")
    })
    .await?;
    let p = main_project().await?;
    if p.config_policy != ConfigPolicy::Isolated
        || p.allow_bypass
        || confirms().await?.len() != asked + 3
    {
        return Err(format!("lowering asked or failed: {p:?}"));
    }

    let banner = || q("[data-banner=api-key]");
    if banner().is_some() {
        return Err("API key banner before the opt-in".into());
    }
    api_key_passthrough(true).await?;
    until("API key banner", UI, banner).await?;
    let texts = confirms().await?;
    let key_text = texts.last().cloned().unwrap_or_default();
    if texts.len() != asked + 4 || !key_text.contains("fatturato") {
        return Err(format!("API key confirmation {texts:?}"));
    }
    api_key_passthrough(false).await?;
    until("no API key banner", UI, || banner().is_none().then_some(())).await?;
    if confirms().await?.len() != asked + 4 {
        return Err("turning the passthrough off asked for a confirmation".into());
    }
    Ok(format!(
        "main: Attendibile + Annulla → still Isolated (set_project_security: Invalid); + OK → \
         Trusted, trusted, noted in the settings; bypass + OK → allowed; both lowered without \
         asking. API key passthrough + OK → topbar banner, off without asking → no banner. \
         {} confirmations asked, each naming what it grants and that the worktree is not a \
         sandbox (or the API billing)",
        asked + 4
    ))
}

/// Opens the settings dialog on its `tab` ("Generali", "Progetto"); returns the dialog.
async fn open_settings(tab: &str) -> R<Element> {
    if open_dialog("Settings").is_none() {
        let button = q_all("nav button")
            .into_iter()
            .find(|b| text(b).contains("Impostazioni"))
            .ok_or("no Impostazioni")?;
        click(&button);
    }
    let dialog = until("settings dialog", UI, || open_dialog("Settings")).await?;
    let trigger = find_all(&dialog, "[data-name=TabsTrigger]")
        .into_iter()
        .find(|t| text(t).trim() == tab)
        .ok_or(format!("no settings tab {tab}"))?;
    if trigger.get_attribute("data-state").as_deref() != Some("Active") {
        click(&trigger);
    }
    until(&format!("settings tab {tab}"), UI, || {
        (trigger.get_attribute("data-state")? == "Active").then_some(())
    })
    .await?;
    let testid = if tab == "Progetto" {
        "project-settings"
    } else {
        "app-settings"
    };
    until(&format!("{testid} loaded"), UI, || {
        find_in(&dialog, &format!("[data-testid={testid}]"))
    })
    .await?;
    Ok(dialog)
}

/// Sets `allow_env_api_key` from the app settings (answering OK to the confirmation it may
/// ask) and saves; the dialog closes on success.
async fn api_key_passthrough(on: bool) -> R {
    let dialog = open_settings("Generali").await?;
    // The form is filled once the settings are loaded (the Salva button is enabled).
    let save = until("settings loaded", UI, || {
        find_in(&dialog, "[data-action=save-settings]").filter(|b| !b.has_attribute("disabled"))
    })
    .await?;
    let checkbox = find_in(&dialog, "#settings-api-key").ok_or("no API key checkbox")?;
    if Reflect::get(&checkbox, &"checked".into())
        .ok()
        .and_then(|v| v.as_bool())
        != Some(on)
    {
        click(&checkbox);
    }
    if on {
        queue_confirm(true).await?;
    }
    let since = last_toast();
    click(&save);
    toast_after(since, "Impostazioni salvate", |t| {
        t.contains("Impostazioni salvate")
    })
    .await?;
    until("settings closed", UI, || {
        open_dialog("Settings").is_none().then_some(())
    })
    .await
}

async fn main_project() -> R<Project> {
    let projects = ipc::call::<ListProjects>(&Empty {})
        .await
        .map_err(|e| e.to_string())?;
    projects
        .into_iter()
        .find(|p| p.name == "main")
        .ok_or_else(|| "no project main".into())
}

async fn queue_confirm(accept: bool) -> R {
    ipc::call::<DebugE2eQueueConfirm>(&E2eConfirmReq { accept })
        .await
        .map_err(|e| e.to_string())
}

async fn confirms() -> R<Vec<String>> {
    ipc::call::<DebugE2eConfirms>(&Empty {})
        .await
        .map_err(|e| e.to_string())
}

/// Every IPC command that failed in this process is one of `expected`, in that order.
async fn only_expected_failures(expected: &[&str]) -> R<String> {
    let failed = ipc::call::<DebugE2eFailures>(&Empty {})
        .await
        .map_err(|e| e.to_string())?;
    if failed
        .iter()
        .map(String::as_str)
        .ne(expected.iter().copied())
    {
        return Err(format!("failed commands {failed:?}, expected {expected:?}"));
    }
    Ok(format!("failed commands: {failed:?} (as provoked)"))
}

/// The report's `app.exit` goes through `ExitRequested` (the other shutdown branch): it
/// runs during a `[fake:hang_ignore]` follow-up of T4, which the script then finds killed
/// with its grandchild and finalized as killed/app_shutdown.
async fn exit_during_turn(run: &mut Run) -> R<String> {
    let t4 = run.st.t4.clone();
    let grandchildren = recorded_grandchildren().await?;
    open_panel(&t4).await?;
    tab("Agente").await?;
    follow_up(&t4, "Attendi ancora [fake:hang_ignore]").await?;
    until_async("hang_ignore grandchild", TURN, || async {
        (recorded_grandchildren().await.ok()?.len() > grandchildren.len()).then_some(())
    })
    .await?;
    until("T4 running", TURN, || badge(&t4, "running")).await?;
    let agents = agents().await?;
    // T4's fake-claude and its grandchild.
    if agents.len() != 2 {
        return Err(format!("agents before the exit {agents:?}"));
    }
    Ok(format!(
        "exiting through app.exit with {agents:?} alive in [fake:hang_ignore]"
    ))
}

/// Every agent of both phases ran without the credentials, nesting and git variables the app
/// itself has (spec §7.2): `scripts/e2e.sh` sets them all, fake-claude records their presence.
async fn child_env_scrubbed(run: &Run) -> R<String> {
    let has = |list: &[String], k: &str| list.iter().any(|a| a == k);
    let (kept, scrubbed) = (&run.setup.app_env, &run.setup.scrubbed_at_start);
    let wrong: Vec<&str> = SCRUBBED
        .into_iter()
        .filter(|k| {
            if KEPT_BY_THE_APP.contains(k) {
                !has(kept, k)
            } else {
                has(kept, k) || !has(scrubbed, k)
            }
        })
        .collect();
    if !wrong.is_empty() {
        return Err(format!(
            "{wrong:?}: not set in the app's environment (scripts/e2e.sh sets them all), or a \
             parent session's variable the app did not remove from itself at startup \
             (app env {kept:?}, removed at startup {scrubbed:?})"
        ));
    }
    let calls = records("call").await?;
    for call in &calls {
        let leaked: Vec<&str> = SCRUBBED
            .into_iter()
            .filter(|k| call["env"][k] != false)
            .collect();
        if !leaked.is_empty() {
            return Err(format!("agent {} got {leaked:?}", call["pid"]));
        }
    }
    Ok(format!(
        "{} agent calls over both phases got none of {SCRUBBED:?}, all set in the app's \
         environment; the app removed {} of them from its own environment at startup",
        calls.len(),
        scrubbed.len()
    ))
}

async fn gatekeeper(run: &mut Run) -> R<Next> {
    let r = async {
        let g = ipc::call::<DebugE2eGatekeeper>(&Empty {})
            .await
            .map_err(|e| e.to_string())?;
        let text = format!(
            "{} opened with `open -a Terminal`, its claude called with {:?} after {} ms; \
             quarantine xattr: {}",
            g.script, g.marker, g.waited_ms, g.quarantined
        );
        if g.marker_ok && !g.quarantined {
            Ok(text)
        } else {
            Err(text)
        }
    }
    .await;
    run.check("gatekeeper_ok", r)?;
    Ok(Next::Report)
}

// ---- steps ------------------------------------------------------------------------------------

/// Step 1 (gate with `FAKE_CLAUDE_AUTH` out → in + Ricontrolla → board) around step 2
/// ("Accedi" writes the login script and would open Terminal: the `open` is recorded).
async fn steps_1_2(run: &mut Run) -> R<String> {
    until("login gate (logged-out)", 20_000, || {
        q("[data-view=onboarding][data-step=logged-out]")
    })
    .await?;
    gate_only()?;
    let r = step_2(run).await;
    run.check("step_2", r)?;
    set_auth(true).await?;
    click(&wait_q("[data-testid=recheck]:not([disabled])").await?);
    until("board after Ricontrolla", UI, || q("[data-view=sidebar]")).await?;
    let account = wait_q("[data-testid=account]").await?;
    let chip = text(&account);
    if !chip.contains("fake@example.com") {
        return Err(format!("account chip: {chip:?}"));
    }
    Ok(format!(
        "gate shown alone (no sidebar, no board) while logged out; after `in` + Ricontrolla \
         the board is shown ({chip})"
    ))
}

/// The login gate is all there is: no sidebar, no board, no task panel behind it.
fn gate_only() -> R {
    for sel in [
        "[data-view=sidebar]",
        "[data-column]",
        "[data-view=task-panel]",
    ] {
        if q(sel).is_some() {
            return Err(format!("{sel} shown with the login gate"));
        }
    }
    Ok(())
}

async fn step_2(run: &Run) -> R<String> {
    click(&wait_q("[data-testid=login]:not([disabled])").await?);
    let dialog = until("login dialog", UI, || open_dialog("Login")).await?;
    let command = find_in(&dialog, "[data-testid=login-command]")
        .map(|c| text(&c))
        .unwrap_or_default();
    if command != "claude auth login" {
        return Err(format!("fallback command {command:?}"));
    }
    let s = ipc::call::<DebugE2eLoginScript>(&Empty {})
        .await
        .map_err(|e| e.to_string())?;
    let fake = &run.setup.fake_claude;
    let expected = format!(
        "#!/bin/sh\n'{fake}' auth login\necho\necho \"Accesso completato? Puoi chiudere questa \
         finestra e tornare ad AI Task Manager.\"\n"
    );
    if s.content != expected {
        return Err(format!("script content {:?}", s.content));
    }
    if s.mode != 0o700 {
        return Err(format!("script mode {:o}", s.mode));
    }
    let open = vec!["-a".to_owned(), "Terminal".into(), s.path.clone()];
    if s.opened.last() != Some(&open) {
        return Err(format!("recorded open calls {:?}", s.opened));
    }
    if s.run_code != Some(0) || !s.run_output.contains("login simulato") {
        return Err(format!("script run: {:?} {:?}", s.run_code, s.run_output));
    }
    let close = button_in(&dialog, "Chiudi").ok_or("no Chiudi in the login dialog")?;
    click(&close);
    until("login dialog closed", UI, || {
        open_dialog("Login").is_none().then_some(())
    })
    .await?;
    Ok(format!(
        "{} (0700) runs `{fake} auth login`; `open -a Terminal` recorded, not run; \
         run with /bin/sh it exits 0",
        s.path
    ))
}

/// Folder, bare and empty repositories rejected with a toast; `.mcp.json` warns; the main
/// repository is added and selected.
async fn step_3(run: &Run) -> R<String> {
    let setup = &run.setup;
    for (path, expected) in [
        (&setup.not_git, "non è un repository git"),
        (&setup.bare, "bare"),
        (&setup.empty, "non ha ancora commit"),
    ] {
        let since = last_toast();
        add_repository(path).await?;
        toast_after(since, &format!("rejection of {path}"), |t| {
            t.starts_with("Invalid:") && t.contains(expected)
        })
        .await?;
        if !project_names().is_empty() {
            return Err(format!("{path} was added: {:?}", project_names()));
        }
    }
    // Not stored either (the backend's list, not only the sidebar), and rejected by the
    // backend itself with `Invalid`.
    let stored = ipc::call::<ListProjects>(&Empty {})
        .await
        .map_err(|e| e.to_string())?;
    if !stored.is_empty() {
        return Err(format!("list_projects after the rejections: {stored:?}"));
    }
    only_expected_failures(&EXPECTED_FAILURES).await?;
    let since = last_toast();
    add_repository(&setup.mcp_repo).await?;
    toast_after(since, "mcp added", |t| {
        t.contains("Progetto «mcp» aggiunto")
    })
    .await?;
    toast_after(since, ".mcp.json warning", |t| {
        t.contains(".mcp.json") && t.contains("Isolato")
    })
    .await?;
    let since = last_toast();
    add_repository(&setup.repo).await?;
    toast_after(since, "main added", |t| {
        t.contains("Progetto «main» aggiunto")
    })
    .await?;
    until("main selected with its board", UI, || {
        let selected = project_button("main")?.get_attribute("aria-current")?;
        (selected == "true" && q("[data-testid=columns]").is_some()).then_some(())
    })
    .await?;
    Ok(format!(
        "not-git, bare and empty rejected (Invalid toast, list_projects empty); .mcp.json \
         warned; projects {:?}",
        project_names()
    ))
}

/// Four tasks (dialog and quick create; T3 and T6 are step 10's), reordered inside "Da fare"
/// and T1 moved to another column and back, all by drag-and-drop: todo = [T3, T6, T2, T1],
/// not the creation order, and [T3, T6, T2] once T1 has left (checked after the restart).
/// Then the reload that re-reads it from the DB.
async fn step_4_before_restart(run: &mut Run) -> R<String> {
    let t1 = create_task("Da fare", T1_TITLE, "Scrivi hello.txt nel worktree.").await?;
    let t2 = quick_create("todo", T2_TITLE).await?;
    let t3 = quick_create("todo", T3_TITLE).await?;
    let t6 = quick_create("todo", T6_TITLE).await?;
    run.st.t1 = t1.clone();
    run.st.t2 = t2.clone();
    run.st.t3 = t3.clone();
    run.st.t6 = t6.clone();
    until_order("todo", &[&t1, &t2, &t3, &t6]).await?;

    // Reorder: T3 to the top, T2 above T1, T6 above T2.
    let top = |task: &str| Some(rect_top(&card(task)?) + 2.0);
    drag(&t3, "todo", top(&t1).ok_or("no T1 card")?).await?;
    until_order("todo", &[&t3, &t1, &t2, &t6]).await?;
    drag(&t2, "todo", top(&t1).ok_or("no T1 card")?).await?;
    until_order("todo", &[&t3, &t2, &t1, &t6]).await?;
    drag(&t6, "todo", top(&t2).ok_or("no T2 card")?).await?;
    until_order("todo", &[&t3, &t6, &t2, &t1]).await?;
    // Between columns: T1 to "In revisione", then back to the end of "Da fare".
    drag(&t1, "inreview", 10_000.0).await?;
    until("T1 in inreview", UI, || {
        (column_of(&t1)? == "inreview").then_some(())
    })
    .await?;
    drag(&t1, "todo", 10_000.0).await?;
    until_order("todo", &[&t3, &t6, &t2, &t1]).await?;
    // The authoritative order, from the backend.
    let expected = vec![t3.clone(), t6.clone(), t2.clone(), t1.clone()];
    let repo = run.setup.repo.clone();
    let board = until_async("backend order", UI, || {
        let (repo, expected) = (repo.clone(), expected.clone());
        async move {
            let board = board(&repo).await.ok()?;
            (ids_in(&board, TaskStatus::Todo) == expected).then_some(board)
        }
    })
    .await?;
    run.st.t3_position = position(&board, &t3);
    run.st.t6_position = position(&board, &t6);
    run.st.t2_position = position(&board, &t2);
    Ok(
        "dialog + 3 quick creates; dragged to todo = [T3, T6, T2, T1] (created T1, T2, T3, T6), \
        every dragover accepted (defaultPrevented) with the dragged id only in the drop's \
        DataTransfer"
            .into(),
    )
}

/// After the page reload: the board is read again from the DB, in the same order.
async fn step_4_reload(run: &mut Run) -> R<String> {
    select_main().await?;
    let st = &run.st;
    let (t1, t2, t3, t6) = (&st.t1, &st.t2, &st.t3, &st.t6);
    until_order("todo", &[t3, t6, t2, t1]).await?;
    let board = board(&run.setup.repo).await?;
    if ids_in(&board, TaskStatus::Todo) != [t3.clone(), t6.clone(), t2.clone(), t1.clone()] {
        return Err("backend order changed across the reload".into());
    }
    Ok(
        "after a native page reload the board re-read from the DB keeps todo = [T3, T6, T2, T1]"
            .into(),
    )
}

/// Step 4's restart for real: a new process on the same DB (T1 has left "Da fare" since and
/// T5 went through it): the DOM and the backend keep the dragged [T3, T6, T2] (created T2,
/// T3, T6) at their positions.
async fn step_4_relaunch(run: &mut Run) -> R<String> {
    until("board after the relaunch", 20_000, || {
        q("[data-view=sidebar]")
    })
    .await?;
    select_main().await?;
    let st = &run.st;
    let (t2, t3, t6) = (&st.t2, &st.t3, &st.t6);
    until_order("todo", &[t3, t6, t2]).await?;
    let board = board(&run.setup.repo).await?;
    let positions = [t3, t6, t2].map(|t| position(&board, t));
    let before = [st.t3_position, st.t6_position, st.t2_position];
    if ids_in(&board, TaskStatus::Todo) != [t3.clone(), t6.clone(), t2.clone()]
        || positions != before
    {
        return Err(format!(
            "todo after the relaunch {:?} at {positions:?}, was at {before:?}",
            ids_in(&board, TaskStatus::Todo),
        ));
    }
    Ok(format!(
        "after the app's restart todo = [T3, T6, T2] (dragged; created T2, T3, T6) in the DOM \
         and in the DB, positions {positions:?} unchanged"
    ))
}

/// "Crea hello [fake:approval]" in Auto-edit: running badge with its spinner, approval card,
/// "Consenti sempre" (the card, the badges and the pending tool entry clear; the answer
/// fake-claude got allows and remembers), the text streamed word by word, TurnEnd, "In
/// revisione".
async fn step_5(run: &mut Run) -> R<String> {
    let t1 = run.st.t1.clone();
    start_attempt(&t1, "acceptEdits").await?;
    let running = until("running badge", UI, || badge(&t1, "running")).await?;
    if find_in(&running, "[role=status]").is_none() {
        return Err("no spinner in the running badge".into());
    }
    let card_ = until("approval card", TURN, || {
        q("[data-view=task-panel] [data-approval]")
    })
    .await?;
    let asked = text(&card_);
    if !asked.contains("Bash") || !asked.contains("echo hello") {
        return Err(format!("approval card {asked:?}"));
    }
    until("approval badge on the card", UI, || badge(&t1, "approval")).await?;
    let attempt = detail(&t1).await?.attempt.ok_or("no attempt")?.id;
    let bash = bash_call(&attempt).await?;
    if !matches!(
        bash,
        ToolStatus::AwaitingApproval {
            can_remember: true,
            ..
        }
    ) {
        return Err(format!("Bash entry before the answer: {bash:?}"));
    }
    let panel = panel().ok_or("no task panel")?;
    let typing = TypingLog::start(&panel)?;
    let before = entries().len();
    click(&find_in(&card_, "[data-action=allow-always]").ok_or("no Consenti sempre")?);
    until("approval answered", UI, || {
        (q("[data-view=task-panel] [data-approval]").is_none()
            && badge(&t1, "approval").is_none()
            && !panel_header().contains("Richiede approvazione"))
        .then_some(())
    })
    .await?;
    let end = until("TurnEnd", TURN, || {
        q_all("[data-view=task-panel] [data-turn-end]")
            .into_iter()
            .next()
    })
    .await?;
    if !text(&end).contains("Completato") {
        return Err(format!("TurnEnd {:?}", text(&end)));
    }
    let streamed = entries().len();
    if streamed < before + 3 {
        return Err(format!("only {streamed} entries after {before}"));
    }
    // Streaming: the typing preview grew word by word before the full text replaced it.
    let (previews, spinner) = typing.stop();
    let growing = previews.windows(2).all(|w| w[0].len() < w[1].len());
    if previews.len() < 2
        || !growing
        || !previews.iter().all(|p| STREAMED.starts_with(p.as_str()))
        || !spinner
    {
        return Err(format!("typing previews {previews:?} (spinner {spinner})"));
    }
    if q("[data-view=task-panel] [data-typing]").is_some()
        || !entries().iter().any(|e| text(e).contains(STREAMED))
    {
        return Err("the streamed text did not become an entry".into());
    }
    let bash = bash_call(&attempt).await?;
    if bash != ToolStatus::Succeeded {
        return Err(format!("Bash entry after Consenti sempre: {bash:?}"));
    }
    wait_idle(&t1, "inreview").await?;
    // The panel refetches its detail on its own, after the board.
    until("panel status In revisione", UI, || {
        panel_header().contains("In revisione").then_some(())
    })
    .await?;
    let d = detail(&t1).await?;
    let turn = d.processes.last().ok_or("no process")?;
    if d.task.status != TaskStatus::InReview || turn.status != ProcessStatus::Completed {
        return Err(format!(
            "DB: task {:?}, turn {:?}",
            d.task.status, turn.status
        ));
    }
    let calls = calls().await?;
    let argv = calls.first().ok_or("fake-claude never called")?;
    if !argv.iter().any(|a| a == "--permission-mode=acceptEdits") {
        return Err(format!("argv {argv:?}"));
    }
    run.st.session = flag(argv, "--session-id=").ok_or("no --session-id")?;
    // What fake-claude received: allow, with the suggested rule remembered for the session.
    let answers = records("control_response").await?;
    let answer = answers.first().ok_or("no control_response recorded")?;
    let decision = &answer["response"]["response"];
    let rule = &decision["updatedPermissions"][0];
    if answer["response"]["subtype"] != "success"
        || decision["behavior"] != "allow"
        || rule["destination"] != "session"
        || rule["rules"][0] != json!({"toolName": "Bash", "ruleContent": "echo hello"})
    {
        return Err(format!("recorded answer {answer}"));
    }
    Ok(format!(
        "spinner in the running badge → approval card → Consenti sempre (card and badges \
         cleared, Bash entry Succeeded, fake-claude got allow + updatedPermissions \
         Bash(echo hello) for the session) → typing {previews:?} → {} entries → TurnEnd \
         Completato → In revisione (column, panel and DB: task inreview, turn completed); \
         session {}",
        streamed - before,
        run.st.session
    ))
}

/// Status of the attempt's Bash tool entry (the one `[fake:approval]` asks for).
async fn bash_call(attempt: &str) -> R<ToolStatus> {
    attempt_entries(attempt)
        .await?
        .into_iter()
        .find_map(|e| match e.body {
            EntryBody::ToolCall { name, status, .. } if name == "Bash" => Some(status),
            _ => None,
        })
        .ok_or_else(|| "no Bash entry".into())
}

/// The Modifiche tab lists hello.txt as added, as `get_diff` does; the file is in the branch,
/// committed by the end-of-turn auto-commit (spec §8.5).
async fn step_6(run: &Run) -> R<String> {
    tab("Modifiche").await?;
    let file = until("hello.txt in the diff", UI, || {
        q("[data-view=diff] [data-file=\"hello.txt\"]")
    })
    .await?;
    let added = find_in(&file, "[title=Aggiunto]").is_some_and(|b| text(&b).trim() == "A");
    if !added || !text(&file).contains("+1") {
        return Err(format!("hello.txt row {:?}", text(&file)));
    }
    let summary = wait_q("[data-diff-summary]").await?;
    let attempt = detail(&run.st.t1).await?.attempt.ok_or("no attempt")?;
    let diff = ipc::call::<GetDiff>(&AttemptIdReq {
        attempt_id: attempt.id.clone(),
    })
    .await
    .map_err(|e| e.to_string())?;
    let files: Vec<_> = diff
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status, f.additions, f.deletions))
        .collect();
    if files != [("hello.txt", FileStatus::Added, 1, 0)] {
        return Err(format!("get_diff files {files:?}"));
    }
    let repo = &run.setup.repo;
    let branch = &attempt.branch;
    let content = git(repo, &["show", &format!("{branch}:hello.txt")]).await?;
    let subject = git(repo, &["log", "-1", "--format=%s", branch]).await?;
    if content.stdout != "hello\n" || !subject.stdout.starts_with("atm: turn 1:") {
        return Err(format!(
            "{branch}: hello.txt {:?}, last commit {:?}",
            content.stdout, subject.stdout
        ));
    }
    Ok(format!(
        "hello.txt added ({}), as get_diff says; committed on {branch} by the auto-commit \
         «{}»",
        text(&summary),
        subject.stdout.trim()
    ))
}

/// Follow-up `[fake:simple]` resumes the session; `[fake:slow]` is stopped within 5 s.
async fn step_7(run: &mut Run) -> R<String> {
    let t1 = run.st.t1.clone();
    tab("Agente").await?;
    let ends = turn_ends();
    let n = follow_up(&t1, "Aggiungi un saluto [fake:simple]").await?;
    until("TurnEnd of the follow-up", TURN, || {
        (turn_ends() > ends).then_some(())
    })
    .await?;
    let done = wait_done(&t1, n).await?;
    if done.status != ProcessStatus::Completed {
        return Err(format!("[fake:simple] follow-up ended {:?}", done.status));
    }
    wait_idle(&t1, "inreview").await?;
    let calls = calls().await?;
    let argv = calls.last().ok_or("no call")?;
    let resume = format!("--resume={}", run.st.session);
    if !argv.contains(&resume) || argv.iter().any(|a| a.starts_with("--session-id=")) {
        return Err(format!("follow-up argv {argv:?}"));
    }
    let settings = flag(argv, "--settings=").unwrap_or_default();
    if !settings.contains("Bash(echo hello)") {
        return Err(format!("allow rule missing from --settings {settings}"));
    }

    follow_up(&t1, "Lavoro lento [fake:slow]").await?;
    until("first slow step", TURN, || {
        entries()
            .iter()
            .any(|e| text(e).contains("Passo 1/"))
            .then_some(())
    })
    .await?;
    let stop = wait_q("[data-view=task-panel] [data-action=stop]").await?;
    let t0 = js_sys::Date::now();
    click(&stop);
    until("stopped", 13_000, || {
        (badge(&t1, "running").is_none() && badge(&t1, "stopped").is_some()).then_some(())
    })
    .await?;
    let seen = js_sys::Date::now() - t0;
    let detail = detail(&t1).await?;
    let last = detail.processes.last().ok_or("no process")?;
    if last.status != ProcessStatus::Killed || last.stop_reason != Some(StopReason::UserStop) {
        return Err(format!(
            "slow turn ended {:?} {:?}",
            last.status, last.stop_reason
        ));
    }
    // The backend's end of the turn (same clock): the UI's own polling may be throttled.
    let elapsed = last.finished_at.map_or(f64::INFINITY, |f| f as f64 - t0);
    run.st.stop_ms = elapsed;
    let agents = agents().await?;
    if elapsed > 5_000.0 || !agents.is_empty() {
        return Err(format!("stop took {elapsed} ms, agents left {agents:?}"));
    }
    wait_idle(&t1, "inreview").await?;
    // M6: the turn's end reads "Interrotto dall'utente", not the CLI's internal diagnostic.
    let end = until("TurnEnd of the stopped turn", UI, || {
        q_all("[data-view=task-panel] [data-turn-end]")
            .into_iter()
            .last()
            .filter(|e| e.get_attribute("data-stopped").as_deref() == Some("user_stop"))
    })
    .await?;
    let shown = panel().map(|p| text(&p)).unwrap_or_default();
    if !text(&end).contains("Interrotto dall'utente") || shown.contains("ede_diagnostic") {
        return Err(format!("stopped TurnEnd {:?}", text(&end)));
    }
    let escalated = stop_escalates(run).await?;
    Ok(format!(
        "follow-up [fake:simple] completed with {resume} and the remembered rule; [fake:slow] \
         killed/user_stop {elapsed:.0} ms after Stop (card updated after {seen:.0} ms), its end \
         «Interrotto dall'utente» without the CLI's diagnostic, no agent left; {escalated}"
    ))
}

/// Stop of a turn that ignores the interrupt, EOF and SIGTERM (`[fake:hang_ignore]`, with a
/// `sleep` grandchild): only the SIGKILL of its process group ends it, after the whole
/// sequence of spec §7.9 and within its 13 s objective; nothing of the group survives.
async fn stop_escalates(run: &mut Run) -> R<String> {
    let t1 = run.st.t1.clone();
    let grandchildren = recorded_grandchildren().await?.len();
    let inits = session_inits();
    let n = follow_up(&t1, "Ignora lo stop [fake:hang_ignore]").await?;
    until_async("hang_ignore grandchild", TURN, || async {
        (recorded_grandchildren().await.ok()?.len() > grandchildren).then_some(())
    })
    .await?;
    // Past `init`: the stop starts with the interrupt.
    until("hang_ignore running", TURN, || {
        (badge(&t1, "running").is_some() && session_inits() > inits).then_some(())
    })
    .await?;
    let alive = agents().await?;
    if alive.len() != 2 {
        return Err(format!("agents before the stop {alive:?}"));
    }
    let stop = wait_q("[data-view=task-panel] [data-action=stop]").await?;
    let t0 = js_sys::Date::now();
    click(&stop);
    let last = until_async("hang_ignore stopped", 20_000, || {
        let t1 = t1.clone();
        async move {
            let d = detail(&t1).await.ok()?;
            let last = d.processes.get(n - 1)?.clone();
            (last.status != ProcessStatus::Running).then_some(last)
        }
    })
    .await?;
    let elapsed = last.finished_at.map_or(f64::INFINITY, |f| f as f64 - t0);
    run.st.stop_escalated_ms = elapsed;
    let left = agents().await?;
    if (last.status, last.stop_reason) != (ProcessStatus::Killed, Some(StopReason::UserStop))
        || !(ESCALATION_MS.0..=ESCALATION_MS.1).contains(&elapsed)
        || !left.is_empty()
    {
        return Err(format!(
            "[fake:hang_ignore] ended {:?}/{:?} {elapsed:.0} ms after Stop, agents left {left:?}",
            last.status, last.stop_reason
        ));
    }
    until("card stopped", UI, || {
        (badge(&t1, "running").is_none() && badge(&t1, "stopped").is_some()).then_some(())
    })
    .await?;
    wait_idle(&t1, "inreview").await?;
    Ok(format!(
        "[fake:hang_ignore] (ignores interrupt, EOF, SIGTERM) killed/user_stop {elapsed:.0} ms \
         after Stop (interrupt 5 s + EOF 3 s + SIGTERM 3 s → SIGKILL of the group), its \
         fake-claude and sleep {alive:?} gone"
    ))
}

/// The page reload (`location.reload()`: the app binds no Cmd+R) dropped the forwarder of the
/// open transcript; reopening the task subscribes again and restores the view.
async fn reload_resubscribes(run: &mut Run) -> R<String> {
    let after_reload = forwarder_count().await?;
    select_main().await?;
    open_panel(&run.st.t1).await?;
    let n = run.st.entries;
    until("transcript restored", UI, || {
        (entries().len() == n).then_some(())
    })
    .await?;
    let reopened = forwarder_count().await?;
    let before = run.st.forwarders;
    if before < 1 || after_reload != 0 || reopened != 1 {
        return Err(format!(
            "forwarders {before} before the reload, {after_reload} after it, {reopened} reopened"
        ));
    }
    Ok(format!(
        "{before} forwarder before the reload, 0 after it, 1 after reopening; {n} rows restored"
    ))
}

/// T5 starts `[fake:hang_ignore]` (it outlives interrupt, EOF and SIGTERM, with a `sleep`
/// grandchild), then T1 follows up with `[fake:hang]`; the quit (Cmd+Q's path) runs while both
/// turns are live.
async fn step_8_quit(run: &mut Run) -> R<String> {
    let t1 = run.st.t1.clone();
    let t5 = create_task("Da fare", T5_TITLE, "").await?;
    run.st.t5 = t5.clone();
    let grandchildren = recorded_grandchildren().await?.len();
    start_attempt(&t5, "acceptEdits").await?;
    until_async("hang_ignore grandchild", TURN, || async {
        (recorded_grandchildren().await.ok()?.len() > grandchildren).then_some(())
    })
    .await?;
    until("T5 running", TURN, || badge(&t5, "running")).await?;

    open_panel(&t1).await?;
    tab("Agente").await?;
    let inits = session_inits();
    follow_up(&t1, "Attendi [fake:hang]").await?;
    until("hang running", TURN, || {
        (badge(&t1, "running").is_some() && session_inits() > inits).then_some(())
    })
    .await?;
    sleep(500).await;
    let agents = agents().await?;
    // T1's and T5's fake-claude and T5's grandchild.
    if agents.len() != 3 {
        return Err(format!("agents before the quit {agents:?}"));
    }
    Ok(format!(
        "pressing Cmd+Q with {agents:?} alive ([fake:hang], [fake:hang_ignore] + its sleep)"
    ))
}

/// After the relaunch: nothing of the run survived the quit (not even the `sleep` of
/// `hang_ignore`, which only the shutdown's SIGKILL of the group ends); both turns were stopped
/// and finalized by the shutdown itself (a crash would leave recovery's failed/app_restart);
/// "Interrotto – Continua" on both cards; Continua resumes T1's session.
async fn step_8_relaunch(run: &mut Run) -> R<String> {
    let left = agents().await?;
    if !left.is_empty() {
        return Err(format!("agents left after the quit: {left:?}"));
    }
    let (t1, t5) = (run.st.t1.clone(), run.st.t5.clone());
    select_main().await?;
    let mut cut = Vec::new();
    for task in [&t1, &t5] {
        let interrupted = until("Interrotto – Continua", UI, || badge(task, "interrupted")).await?;
        let label = text(&interrupted);
        if !label.contains("Interrotto") || !label.contains("Continua") {
            return Err(format!("badge {label:?}"));
        }
        let d = detail(task).await?;
        let last = d.processes.last().ok_or("no process")?;
        let attempt = d.attempt.as_ref().ok_or("no active attempt")?;
        let notices: Vec<String> = attempt_entries(&attempt.id)
            .await?
            .into_iter()
            .filter(|e| e.process_id == last.id)
            .filter_map(|e| match e.body {
                EntryBody::Notice { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        if (last.status, last.stop_reason) != (ProcessStatus::Killed, Some(StopReason::AppShutdown))
            || !notices.iter().any(|n| n == SHUTDOWN_NOTICE)
        {
            return Err(format!(
                "{:?} ended {:?}/{:?} with notices {notices:?}",
                last.prompt, last.status, last.stop_reason
            ));
        }
        cut.push(d);
    }
    let t5_worktree = cut[1]
        .attempt
        .as_ref()
        .ok_or("no T5 attempt")?
        .worktree_path
        .clone();
    let t5_session = calls_in(&t5_worktree)
        .await?
        .first()
        .and_then(|argv| flag(argv, "--session-id="))
        .ok_or("no --session-id for T5")?;
    let mut resumed = Vec::new();
    for (task, session, d) in [(&t1, &run.st.session, &cut[0]), (&t5, &t5_session, &cut[1])] {
        resumed.push(continue_resumes(task, session, d.processes.len() + 1).await?);
    }
    Ok(format!(
        "no agent of the run (fake-claude or grandchild) after the quit; [fake:hang] and \
         [fake:hang_ignore] both killed/app_shutdown with «{SHUTDOWN_NOTICE}»; \
         \"Interrotto – Continua\" on both, each Continua → {resumed:?}, turn completed"
    ))
}

/// Clicks the Continua of `task`'s "Interrotto" badge: the follow-up is `CONTINUE_PROMPT`
/// with `--resume=<session>`, and its turn (the `n`th) completes. Returns the flag.
async fn continue_resumes(task: &str, session: &str, n: usize) -> R<String> {
    let before = calls().await?.len();
    let interrupted = badge(task, "interrupted").ok_or("no Interrotto")?;
    click(&find_in(&interrupted, "button").ok_or("no Continua button")?);
    let argv = new_call(before).await?;
    let resume = format!("--resume={session}");
    if !argv.contains(&resume) || argv.iter().any(|a| a.starts_with("--session-id=")) {
        return Err(format!("Continua argv {argv:?}, expected {resume}"));
    }
    let last = wait_done(task, n).await?;
    wait_idle(task, "inreview").await?;
    if last.prompt != CONTINUE_PROMPT || last.status != ProcessStatus::Completed {
        return Err(format!("Continua turn {:?} {:?}", last.prompt, last.status));
    }
    if badge(task, "interrupted").is_some() {
        return Err("Interrotto still shown after Continua".into());
    }
    Ok(resume)
}

/// Merge with `main` checked out and clean: Fatto, worktree removed, squash commit on main.
async fn step_9(run: &mut Run) -> R<String> {
    let t1 = run.st.t1.clone();
    let attempt = active_attempt(&t1).await?;
    let commit = merge_via_ui(&t1, true).await?;
    if commit.len() < 7 {
        return Err(format!("merge commit {commit:?} not in the toast"));
    }
    until("T1 Fatto + Mergiato", UI, || {
        (column_of(&t1)? == "done" && text(&badge(&t1, "closed")?).contains("Mergiato"))
            .then_some(())
    })
    .await?;
    let repo = run.setup.repo.clone();
    closed_cleanly("Mergiato in main con il commit").await?;
    worktree_gone(&repo, &attempt.2).await?;
    let log = git(&repo, &["log", "-1", "--format=%H%n%s", "main"]).await?;
    let mut lines = log.stdout.lines();
    let (head, subject) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
    if !head.starts_with(&commit) || subject != T1_TITLE {
        return Err(format!("main log {:?} vs merge {commit}", log.stdout));
    }
    let parents = git(&repo, &["log", "-1", "--format=%P", "main"]).await?;
    if parents.stdout.split_whitespace().count() != 1 {
        return Err(format!("squash commit parents {:?}", parents.stdout));
    }
    let files = git(&repo, &["show", "--name-only", "--format=", "main"]).await?;
    let status = git(&repo, &["status", "--porcelain"]).await?;
    let branch = git(&repo, &["branch", "--list", &attempt.1]).await?;
    if !files.stdout.contains("hello.txt") || !status.stdout.is_empty() {
        return Err(format!(
            "squash files {:?}, checkout {:?}",
            files.stdout, status.stdout
        ));
    }
    if branch.stdout.trim().is_empty() {
        return Err(format!("branch {} deleted", attempt.1));
    }
    let merged = closed_in_db(&t1, &attempt.0, AttemptState::Merged, TaskStatus::Done).await?;
    if merged.merge_commit.as_deref() != Some(head) {
        return Err(format!(
            "DB merge_commit {:?} vs main {head}",
            merged.merge_commit
        ));
    }
    Ok(format!(
        "squash {head} \"{subject}\" on main (fast-forward of the checkout), worktree removed \
         (directory and `git worktree list`), branch {} kept; DB: task done, attempt merged \
         with that merge_commit, worktree removed; the panel shows the merged attempt with no \
         failed command",
        attempt.1
    ))
}

/// Two tasks branch from main (T1's hello.txt) and append different lines to hello.txt
/// (`[fake:append]`); T6 is merged first, so T3, the second, conflicts with main →
/// "Conflitti con main" listing hello.txt → "Risolvi con l'agente": the app's conflict prompt,
/// for which fake-claude plays `resolve_merge` (recorded) on the target named in it → merge
/// commit on the branch → merge ok.
async fn step_10(run: &mut Run) -> R<String> {
    let repo = run.setup.repo.clone();
    let (t3, t6) = (run.st.t3.clone(), run.st.t6.clone());
    let mut branches = Vec::new();
    for (task, line) in [(&t6, "Hola su hello"), (&t3, "Conflitto su hello")] {
        start_attempt(task, "acceptEdits").await?;
        wait_turn(task, "inreview").await?;
        let branch = detail(task).await?.attempt.ok_or("no attempt")?.branch;
        let hello = git(&repo, &["show", &format!("{branch}:hello.txt")]).await?;
        if hello.stdout != format!("hello\n{line}\n") {
            return Err(format!("{branch}: hello.txt {:?}", hello.stdout));
        }
        branches.push(branch);
    }
    // T6 first: a clean squash merge into the checked-out main.
    merge_via_ui(&t6, true).await?;
    until("T6 Fatto", UI, || (column_of(&t6)? == "done").then_some(())).await?;
    closed_cleanly("Mergiato in main con il commit").await?;
    let t6_attempt = detail(&t6).await?.closed_attempts;
    let t6_attempt = t6_attempt.last().ok_or("no closed T6 attempt")?;
    closed_in_db(&t6, &t6_attempt.id, AttemptState::Merged, TaskStatus::Done).await?;
    let hello = git(&repo, &["show", "main:hello.txt"]).await?;
    if hello.stdout != "hello\nHola su hello\n" {
        return Err(format!("main:hello.txt after T6 {:?}", hello.stdout));
    }

    open_panel(&t3).await?;
    let attempt = detail(&t3).await?.attempt.ok_or("no attempt")?;
    tab("Modifiche").await?;
    let alert = until("conflict alert", UI, || {
        q_all("[data-view=diff] [data-name=Alert]")
            .into_iter()
            .find(|a| find_in(a, "h4").is_some_and(|t| text(&t) == "Conflitti con main"))
    })
    .await?;
    let listed = find_in(&alert, "p").map(|p| text(&p)).unwrap_or_default();
    if listed != "Il merge andrebbe in conflitto in: hello.txt." {
        return Err(format!("conflict alert lists {listed:?}"));
    }
    let status = ipc::call::<GetBranchStatus>(&AttemptIdReq {
        attempt_id: attempt.id.clone(),
    })
    .await
    .map_err(|e| e.to_string())?;
    if status.conflicts != ["hello.txt"] {
        return Err(format!("branch status conflicts {:?}", status.conflicts));
    }
    // A conflicting branch cannot be merged: the diff header offers the agent instead.
    let merge = wait_q("[data-view=diff] [data-action=merge]").await?;
    if !merge.has_attribute("disabled") || !panel_text().contains("Merge non disponibile") {
        return Err("Merge not blocked by the conflict".into());
    }
    let diff = wait_q("[data-view=diff]").await?;
    let resolve = button_in(&diff, "Risolvi con l'agente").ok_or("no Risolvi con l'agente")?;
    let before = calls().await?.len();
    let n = detail(&t3).await?.processes.len() + 1;
    let since = last_toast();
    click(&resolve);
    toast_after(since, "handed to the agent", |t| {
        t.contains("Conflitti affidati")
    })
    .await?;
    let argv = new_call(before).await?;
    let last = wait_done(&t3, n).await?;
    if last.status != ProcessStatus::Completed {
        return Err(format!(
            "resolve turn {:?} {:?}",
            last.status, last.result_subtype
        ));
    }
    // The follow-up is the app's own prompt (the button adds no `[fake:…]` tag): fake-claude
    // played `resolve_merge` for it (its record says so) on the target the prompt names.
    let pid = records("call")
        .await?
        .get(before)
        .and_then(|c| c["pid"].as_u64())
        .ok_or("no pid for the resolve call")?;
    let scenarios: Vec<String> = records("turn")
        .await?
        .iter()
        .filter(|t| t["pid"].as_u64() == Some(pid))
        .filter_map(|t| t["scenario"].as_str().map(str::to_owned))
        .collect();
    if !last
        .prompt
        .starts_with("This branch conflicts with `main` in: hello.txt.")
        || last.prompt.contains("[fake:")
        || !argv.iter().any(|a| a.starts_with("--resume="))
        || scenarios != ["resolve_merge"]
    {
        return Err(format!(
            "resolve turn {:?} {argv:?}, scenarios {scenarios:?}",
            last.prompt
        ));
    }
    let merged_main = attempt_entries(&attempt.id)
        .await?
        .into_iter()
        .filter(|e| e.process_id == last.id)
        .any(|e| {
            matches!(&e.body, EntryBody::ToolCall { name, input, status, .. }
                if name == "Bash" && input.contains("git merge main")
                    && *status == ToolStatus::Succeeded)
        });
    if !merged_main {
        return Err("no successful `git merge main` in the resolve turn".into());
    }
    wait_idle(&t3, "inreview").await?;
    let branch = attempt.branch.clone();
    let parents = git(&repo, &["log", "-1", "--format=%P", &branch]).await?;
    if parents.stdout.split_whitespace().count() != 2 {
        return Err(format!("no merge commit on {branch}: {:?}", parents.stdout));
    }
    until("conflicts gone", TURN, || {
        (!panel_text().contains("Conflitti con main")).then_some(())
    })
    .await?;
    merge_via_ui(&t3, false).await?;
    until("T3 Fatto", UI, || (column_of(&t3)? == "done").then_some(())).await?;
    closed_cleanly("Mergiato in main con il commit").await?;
    worktree_gone(&repo, &attempt.worktree_path).await?;
    closed_in_db(&t3, &attempt.id, AttemptState::Merged, TaskStatus::Done).await?;
    let hello = git(&repo, &["show", "main:hello.txt"]).await?;
    if hello.stdout != "hello\nConflitto su hello\nhello\nHola su hello\n" {
        return Err(format!("merged hello.txt {:?}", hello.stdout));
    }
    Ok(format!(
        "T6 and T3 ({branches:?}) both appended to hello.txt; T6 squash merged first → T3 \
         shows Conflitti con main listing hello.txt (alert and get_branch_status), Merge \
         blocked → Risolvi con l'agente sends the app's conflict prompt: fake-claude recorded \
         scenario resolve_merge and ran `git merge main`, the target named in the prompt → \
         merge commit on the branch → squash merged, hello.txt = both versions"
    ))
}

/// Discard: worktree removed, branch kept, task back in "Da fare".
async fn step_11(run: &mut Run) -> R<String> {
    let t2 = run.st.t2.clone();
    start_attempt(&t2, "acceptEdits").await?;
    wait_turn(&t2, "inreview").await?;
    let (id, branch, worktree) = active_attempt(&t2).await?;
    click(&wait_q("[data-view=task-panel] [data-action=discard]").await?);
    let dialog = until("discard dialog", UI, || open_dialog("DiscardDialog")).await?;
    let since = last_toast();
    click(&find_in(&dialog, "[data-action=confirm-discard]").ok_or("no Scarta")?);
    toast_after(since, "discarded", |t| t.contains("Tentativo scartato")).await?;
    until("T2 back in Da fare", UI, || {
        (column_of(&t2)? == "todo" && text(&badge(&t2, "closed")?).contains("Scartato"))
            .then_some(())
    })
    .await?;
    worktree_gone(&run.setup.repo, &worktree).await?;
    let listed = git(&run.setup.repo, &["branch", "--list", &branch]).await?;
    if listed.stdout.trim().is_empty() {
        return Err(format!("branch {branch} deleted"));
    }
    closed_in_db(&t2, &id, AttemptState::Discarded, TaskStatus::Todo).await?;
    Ok(format!(
        "worktree removed (directory and `git worktree list`), branch {branch} kept, task in \
         Da fare; DB: attempt discarded, worktree removed, task todo"
    ))
}

/// `[fake:usage_limit]` → pause banner → Riprendi; a `[fake:big]` turn (entries over 8 KiB on
/// the Channel); `[fake:auth_fail]` → the gate again (then logged back in).
async fn step_12(run: &mut Run) -> R<String> {
    let t4 = create_task("Da fare", T4_TITLE, "").await?;
    run.st.t4 = t4.clone();
    start_attempt(&t4, "acceptEdits").await?;
    let banner = until("pause banner", TURN, || q("[data-banner=paused]")).await?;
    if !text(&banner).contains("usage limit") {
        return Err(format!("banner {:?}", text(&banner)));
    }
    wait_idle(&t4, "inreview").await?;
    // The transcript subscribes once the panel's detail shows the attempt.
    until("usage-limit alert in the transcript", UI, || {
        panel_text()
            .contains("Limite d'uso raggiunto")
            .then_some(())
    })
    .await?;
    let limited = detail(&t4).await?;
    let limited = limited.processes.last().ok_or("no process")?;
    if (limited.status, limited.stop_reason)
        != (ProcessStatus::Failed, Some(StopReason::UsageLimit))
        || env_paused().await?.is_none()
    {
        return Err(format!(
            "usage_limit turn {:?}/{:?}, core not paused",
            limited.status, limited.stop_reason
        ));
    }
    click(&button_in(&banner, "Riprendi").ok_or("no Riprendi")?);
    until("banner gone", UI, || {
        q("[data-banner=paused]").is_none().then_some(())
    })
    .await?;
    if let Some(paused) = env_paused().await? {
        return Err(format!("still paused after Riprendi: {paused:?}"));
    }

    let big = channel_big(&t4).await;
    run.check("channel_big_ok", big)?;

    // Logged in until the CLI reports the failure: fake-claude's `auth status` then says
    // logged out, as the real CLI's does once its login is gone, and the core re-reads it.
    follow_up(&t4, "Riprova [fake:auth_fail]").await?;
    until("login gate again", TURN, || {
        q("[data-view=onboarding][data-step=logged-out]")
    })
    .await?;
    gate_only()?;
    let turn = detail(&t4).await?;
    let failed = turn.processes.last().ok_or("no process")?;
    if failed.stop_reason != Some(StopReason::AuthFailure) {
        return Err(format!("auth_fail turn ended {:?}", failed.stop_reason));
    }
    let auth = ipc::call::<GetEnv>(&GetEnvReq { force: false })
        .await
        .map_err(|e| e.to_string())?
        .auth;
    if auth != atm_types::AuthState::LoggedOut {
        return Err(format!("get_env after auth_fail: {auth:?}"));
    }
    set_auth(true).await?;
    click(&wait_q("[data-testid=recheck]:not([disabled])").await?);
    until("board after logging in again", UI, || {
        q("[data-view=sidebar]")
    })
    .await?;
    Ok(
        "[fake:usage_limit] failed/usage_limit pauses the core (get_env) → pause banner → \
        Riprendi clears banner and pause; [fake:auth_fail] (auth_failure, get_env loggedOut) \
        brings back the login gate alone; logged in again → board"
            .into(),
    )
}

/// Messages over 8 KiB on a Channel: the probe's 20 KiB messages, and a `[fake:big]` turn
/// whose tool output entry (8 KiB head + tail) is rendered intact.
async fn channel_big(t4: &str) -> R<String> {
    if !channel_in_order().await {
        return Err("debug_channel_probe: 20 KiB messages not intact".into());
    }
    tab("Agente").await?;
    let ends = turn_ends();
    let n = follow_up(t4, "Leggi il file grande [fake:big]").await?;
    until("TurnEnd of [fake:big]", TURN, || {
        (turn_ends() > ends).then_some(())
    })
    .await?;
    wait_done(t4, n).await?;
    wait_idle(t4, "inreview").await?;
    let row = entries()
        .into_iter()
        .rev()
        .find(|e| text(e).contains("big.txt"))
        .ok_or("no Read big.txt row")?;
    click(&find_in(&row, "[data-name=CollapsibleTrigger]").ok_or("no trigger")?);
    let output = until("tool output", UI, || {
        find_all(&row, "pre").into_iter().nth(1)
    })
    .await?;
    let len = text(&output).len();
    let rows = text(&row);
    if len < 8 * 1024 - 64 || !rows.contains("Output troncato") || !rows.contains("riga 0000003199")
    {
        return Err(format!("tool output {len} bytes"));
    }
    until("the stream went on past the 20 MiB line", UI, || {
        let panel = panel_text();
        (panel.contains("(oltre 16 MiB) ignorata") && panel.contains("Dopo la riga gigante."))
            .then_some(())
    })
    .await?;
    Ok(format!(
        "probe 3×20 KiB intact; [fake:big] output entry of {len} bytes rendered (head + tail), \
         20 MiB line skipped"
    ))
}

// ---- app actions ------------------------------------------------------------------------------

async fn add_repository(path: &str) -> R {
    ipc::call::<DebugE2eQueuePick>(&E2ePathReq { path: path.into() })
        .await
        .map_err(|e| e.to_string())?;
    let sidebar = wait_q("[data-view=sidebar]").await?;
    click(&button_in(&sidebar, "Aggiungi repository").ok_or("no Aggiungi repository")?);
    Ok(())
}

/// TaskDialog from the "+" of the column titled `column`; returns the new card's id.
async fn create_task(column: &str, title: &str, description: &str) -> R<String> {
    let before = card_ids();
    click(&wait_q(&format!("button[aria-label=\"Nuovo task in {column}\"]")).await?);
    let dialog = until("task dialog", UI, || open_dialog("TaskDialog")).await?;
    set_value(&find_in(&dialog, "#task-title").ok_or("no title")?, title)?;
    set_value(
        &find_in(&dialog, "#task-description").ok_or("no description")?,
        description,
    )?;
    click(&button_in(&dialog, "Crea task").ok_or("no Crea task")?);
    until("task dialog closed", UI, || {
        open_dialog("TaskDialog").is_none().then_some(())
    })
    .await?;
    new_card(&before, title).await
}

/// "Aggiungi task" at the bottom of a column, submitted with the form (Enter).
async fn quick_create(status: &str, title: &str) -> R<String> {
    let before = card_ids();
    click(
        &wait_q(&format!(
            "[data-column={status}] [data-testid=quick-create]"
        ))
        .await?,
    );
    let input = wait_q(&format!("[data-column={status}] form input")).await?;
    set_value(&input, title)?;
    let form = input.closest("form").ok().flatten().ok_or("no form")?;
    let submit: Function = Reflect::get(&form, &"requestSubmit".into())
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or("no requestSubmit")?;
    submit
        .call0(&form)
        .map_err(|e| format!("requestSubmit: {e:?}"))?;
    let id = new_card(&before, title).await?;
    // Esc closes the quick-create field.
    key(&input, "Escape")?;
    Ok(id)
}

async fn new_card(before: &[String], title: &str) -> R<String> {
    until(&format!("card {title:?}"), UI, || {
        q_all("[data-column] [data-task-id]")
            .into_iter()
            .find_map(|c| {
                let id = c.get_attribute("data-task-id")?;
                (!before.contains(&id) && text(&c).contains(title)).then_some(id)
            })
    })
    .await
}

/// Opens the task's panel and starts an attempt with the Avvia dialog in `mode`.
async fn start_attempt(task: &str, mode: &str) -> R {
    open_panel(task).await?;
    tab("Agente").await?;
    click(&wait_q("[data-view=task-panel] [data-action=open-start]").await?);
    let dialog = until("start dialog", UI, || {
        open_dialog("StartDialog").filter(|d| find_in(d, "#start-mode").is_some())
    })
    .await?;
    set_select(&find_in(&dialog, "#start-mode").ok_or("no mode")?, mode)?;
    let start = until("Avvia enabled", UI, || {
        find_in(&dialog, "[data-action=start]:not([disabled])")
    })
    .await?;
    let since = last_toast();
    click(&start);
    toast_after(since, "attempt started", |t| {
        t.contains("Tentativo avviato")
    })
    .await?;
    Ok(())
}

/// Types `prompt` in the composer and sends it with ⌘↩, once fake-claude runs it; returns
/// the number of turns of the attempt including this one.
async fn follow_up(task: &str, prompt: &str) -> R<usize> {
    let area = wait_q("[data-view=composer] textarea:not([disabled])").await?;
    set_value(&area, prompt)?;
    let before = calls().await?.len();
    let n = detail(task).await?.processes.len() + 1;
    key_with(&area, "Enter", true)?;
    new_call(before).await?;
    Ok(n)
}

/// argv of fake-claude's call number `before` (0-based), once it has been made.
async fn new_call(before: usize) -> R<Vec<String>> {
    until_async("fake-claude call", TURN, move || async move {
        calls().await.ok()?.get(before).cloned()
    })
    .await
}

/// The attempt's `n`th turn has finished (its process row left `running`).
async fn wait_done(task: &str, n: usize) -> R<ProcessInfo> {
    let task = task.to_owned();
    until_async(&format!("turn {n} finished"), TURN, || {
        let task = task.clone();
        async move {
            let d = detail(&task).await.ok()?;
            let last = d.processes.get(n.checked_sub(1)?)?.clone();
            (last.status != ProcessStatus::Running).then_some(last)
        }
    })
    .await
}

/// Modifiche → Merge → Esegui merge; returns the squash commit (from the toast).
async fn merge_via_ui(task: &str, checked_out: bool) -> R<String> {
    open_panel(task).await?;
    tab("Modifiche").await?;
    let merge = wait_q("[data-view=diff] [data-action=merge]:not([disabled])").await?;
    if checked_out && !panel_text().contains("main è in checkout") {
        return Err("no «main è in checkout» alert".into());
    }
    click(&merge);
    let dialog = until("merge dialog", UI, || open_dialog("MergeDialog")).await?;
    let message = find_in(&dialog, "#merge-message")
        .and_then(|m| Reflect::get(&m, &"value".into()).ok()?.as_string())
        .unwrap_or_default();
    if message.lines().next() != detail(task).await?.task.title.lines().next() {
        return Err(format!("default message {message:?}"));
    }
    let since = last_toast();
    click(&find_in(&dialog, "[data-action=confirm-merge]").ok_or("no Esegui merge")?);
    let toast = toast_after(since, "merge done", |t| t.contains("Merge completato")).await?;
    if checked_out && !toast.contains("fast-forward del checkout") {
        return Err(format!("merge toast {toast:?}"));
    }
    let commit = toast
        .split_once("Merge completato: ")
        .map(|(_, rest)| rest.chars().take_while(char::is_ascii_hexdigit).collect())
        .unwrap_or_default();
    Ok(commit)
}

async fn open_panel(task: &str) -> R {
    let sel = format!("[data-view=task-panel][data-task-id=\"{task}\"]");
    if q(&sel).is_none() {
        click(&until("card", UI, || card(task)).await?);
    }
    until("task panel", UI, || q(&format!("{sel} [data-name=Tabs]"))).await?;
    Ok(())
}

async fn tab(label: &str) -> R {
    let panel = until("task panel", UI, panel).await?;
    let trigger = find_all(&panel, "[data-name=TabsTrigger]")
        .into_iter()
        .find(|t| text(t).trim() == label)
        .ok_or(format!("no tab {label}"))?;
    // Clicking the active tab again would rebuild its view (a stale element for the caller).
    if trigger.get_attribute("data-state").as_deref() == Some("Active") {
        return Ok(());
    }
    click(&trigger);
    until(&format!("tab {label}"), UI, || {
        (trigger.get_attribute("data-state")? == "Active").then_some(())
    })
    .await
}

async fn select_main() -> R {
    let main = until("project main", UI, || project_button("main")).await?;
    if main.get_attribute("aria-current").as_deref() != Some("true") {
        click(&main);
    }
    until("board of main", UI, || {
        let selected = project_button("main")?.get_attribute("aria-current")?;
        (selected == "true" && q("[data-column=todo] [data-card-list]").is_some()).then_some(())
    })
    .await
}

/// The turn has ended (no running badge) and the card is in `status`'s column.
async fn wait_idle(task: &str, status: &str) -> R {
    until(&format!("{task} idle in {status}"), TURN, || {
        (badge(task, "running").is_none() && column_of(task)? == status).then_some(())
    })
    .await
}

/// A turn started by `start_attempt` ran to its end (a TurnEnd row, then idle).
async fn wait_turn(task: &str, status: &str) -> R {
    until("TurnEnd", TURN, || {
        q("[data-view=task-panel] [data-turn-end]")
    })
    .await?;
    wait_idle(task, status).await
}

/// The panel swapped to the closed attempt (`title` in its alert), and no command failed on
/// the way (e.g. a refetch of the diff of the removed worktree).
async fn closed_cleanly(title: &str) -> R {
    until("closed attempt in the panel", UI, || {
        q("[data-view=task-panel] [data-view=closed-attempt]").filter(|c| text(c).contains(title))
    })
    .await?;
    sleep(1_000).await;
    if panel_text().contains("non disponibile") {
        return Err(format!("error in the panel: {:?}", panel_text()));
    }
    only_expected_failures(&[]).await.map(drop)
}

/// The worktree's directory is gone and git no longer lists it.
async fn worktree_gone(repo: &str, worktree: &str) -> R {
    if exists(worktree).await? {
        return Err(format!("worktree {worktree} still there"));
    }
    let listed = git(repo, &["worktree", "list", "--porcelain"]).await?;
    let name = worktree.rsplit('/').next().unwrap_or(worktree);
    if listed.code != 0 || listed.stdout.contains(name) {
        return Err(format!("git worktree list: {:?}", listed.stdout));
    }
    Ok(())
}

// ---- backend reads ----------------------------------------------------------------------------

async fn set_auth(logged_in: bool) -> R {
    ipc::call::<DebugE2eSetAuth>(&E2eAuthReq { logged_in })
        .await
        .map_err(|e| e.to_string())
}

/// `EnvStatus.paused` as the backend reports it (the 60 s cache holds the pause itself).
async fn env_paused() -> R<Option<String>> {
    ipc::call::<GetEnv>(&GetEnvReq { force: false })
        .await
        .map(|env| env.paused)
        .map_err(|e| e.to_string())
}

async fn forwarder_count() -> R<u32> {
    ipc::call::<DebugForwarderCount>(&Empty {})
        .await
        .map_err(|e| e.to_string())
}

async fn agents() -> R<Vec<i32>> {
    ipc::call::<DebugE2eAgents>(&Empty {})
        .await
        .map_err(|e| e.to_string())
}

async fn exists(path: &str) -> R<bool> {
    ipc::call::<DebugE2eExists>(&E2ePathReq { path: path.into() })
        .await
        .map_err(|e| e.to_string())
}

async fn git(repo: &str, args: &[&str]) -> R<E2eGitOut> {
    let req = E2eGitReq {
        repo: repo.into(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    };
    ipc::call::<DebugE2eGit>(&req)
        .await
        .map_err(|e| e.to_string())
}

/// The lines of fake-claude's record of this `kind`, in order.
async fn records(kind: &str) -> R<Vec<Value>> {
    let lines = ipc::call::<DebugE2eRecord>(&Empty {})
        .await
        .map_err(|e| e.to_string())?;
    Ok(lines.into_iter().filter(|l| l["kind"] == kind).collect())
}

/// argv (without argv0) of every fake-claude `-p` call, in order.
async fn calls() -> R<Vec<Vec<String>>> {
    Ok(records("call")
        .await?
        .into_iter()
        .filter_map(|l| serde_json::from_value(l["argv"].clone()).ok())
        .collect())
}

/// argv of the fake-claude `-p` calls that ran in `worktree`, in order.
async fn calls_in(worktree: &str) -> R<Vec<Vec<String>>> {
    let name = worktree.rsplit('/').next().unwrap_or(worktree);
    Ok(records("call")
        .await?
        .into_iter()
        .filter(|l| {
            l["cwd"]
                .as_str()
                .is_some_and(|cwd| cwd == worktree || cwd.ends_with(&format!("/{name}")))
        })
        .filter_map(|l| serde_json::from_value(l["argv"].clone()).ok())
        .collect())
}

/// Pids of the `sleep` grandchildren `[fake:hang_ignore]` started.
async fn recorded_grandchildren() -> R<Vec<u64>> {
    Ok(records("grandchild")
        .await?
        .iter()
        .filter_map(|l| l["pid"].as_u64())
        .collect())
}

/// Every transcript entry of the attempt (a run's attempts have far fewer than 200).
async fn attempt_entries(attempt_id: &str) -> R<Vec<Entry>> {
    let req = GetEntriesReq {
        attempt_id: attempt_id.into(),
        before_idx: u32::MAX,
        limit: 200,
    };
    let page = ipc::call::<GetEntries>(&req)
        .await
        .map_err(|e| e.to_string())?;
    Ok(page.entries)
}

fn flag(argv: &[String], prefix: &str) -> Option<String> {
    argv.iter()
        .find_map(|a| a.strip_prefix(prefix))
        .map(str::to_owned)
}

async fn detail(task: &str) -> R<TaskDetail> {
    ipc::call::<GetTaskDetail>(&IdReq { id: task.into() })
        .await
        .map_err(|e| e.to_string())
}

/// `(id, branch, worktree path)` of the task's active attempt.
async fn active_attempt(task: &str) -> R<(String, String, String)> {
    let attempt = detail(task).await?.attempt.ok_or("no active attempt")?;
    Ok((attempt.id, attempt.branch, attempt.worktree_path))
}

/// What the DB says once the task's attempt `id` is closed: no active attempt, `id` among the
/// closed ones in `state` with its worktree removed, the task in `status`. Returns the attempt.
async fn closed_in_db(
    task: &str,
    id: &str,
    state: AttemptState,
    status: TaskStatus,
) -> R<atm_types::AttemptView> {
    let d = detail(task).await?;
    let closed = d.closed_attempts.iter().find(|a| a.id == id);
    match closed {
        Some(a)
            if d.attempt.is_none()
                && d.task.status == status
                && a.state == state
                && a.worktree_state == WorktreeState::Removed
                && a.closed_at.is_some() =>
        {
            Ok(a.clone())
        }
        _ => Err(format!(
            "task {:?} with active attempt {:?}, closed {closed:?}",
            d.task.status,
            d.attempt.as_ref().map(|a| &a.id)
        )),
    }
}

/// The board of the project of `repo`.
async fn board(repo: &str) -> R<Vec<TaskCard>> {
    let projects = ipc::call::<ListProjects>(&Empty {})
        .await
        .map_err(|e| e.to_string())?;
    let project = projects
        .into_iter()
        .find(|p| p.repo_path == repo)
        .ok_or("project main not found")?;
    ipc::call::<GetBoard>(&ProjectIdReq {
        project_id: project.id,
    })
    .await
    .map_err(|e| e.to_string())
}

fn ids_in(board: &[TaskCard], status: TaskStatus) -> Vec<String> {
    let mut col: Vec<&TaskCard> = board.iter().filter(|c| c.task.status == status).collect();
    col.sort_by(|a, b| a.task.position.total_cmp(&b.task.position));
    col.into_iter().map(|c| c.task.id.clone()).collect()
}

fn position(board: &[TaskCard], task: &str) -> f64 {
    board
        .iter()
        .find(|c| c.task.id == task)
        .map_or(f64::NAN, |c| c.task.position)
}

// ---- DOM --------------------------------------------------------------------------------------

/// What is on screen, for the details of a failed check.
fn dom_summary() -> String {
    let projects: Vec<String> = q_all("[data-testid=projects] button")
        .iter()
        .map(|b| {
            format!(
                "{}{}",
                project_label(b),
                b.get_attribute("aria-current").map_or("", |_| "*")
            )
        })
        .collect();
    let columns: Vec<String> = q_all("[data-column]")
        .iter()
        .map(|c| {
            let ids = find_all(c, "[data-task-id]").len();
            format!(
                "{}:{ids}",
                c.get_attribute("data-column").unwrap_or_default()
            )
        })
        .collect();
    let dialogs: Vec<String> = q_all("[data-state=open][role=dialog]")
        .iter()
        .filter_map(|d| d.get_attribute("data-name"))
        .collect();
    let view = q_all("[data-view]")
        .iter()
        .filter_map(|v| v.get_attribute("data-view"))
        .collect::<Vec<_>>();
    let panel: String = panel_text().chars().take(600).collect();
    format!(
        "views {view:?}; projects {projects:?}; columns {columns:?}; dialogs {dialogs:?}; \
         toasts {:?}; panel {panel:?}",
        toasts()
    )
}

fn q(sel: &str) -> Option<Element> {
    document().query_selector(sel).ok().flatten()
}

fn q_all(sel: &str) -> Vec<Element> {
    elements(document().query_selector_all(sel).ok())
}

fn find_in(root: &Element, sel: &str) -> Option<Element> {
    root.query_selector(sel).ok().flatten()
}

fn find_all(root: &Element, sel: &str) -> Vec<Element> {
    elements(root.query_selector_all(sel).ok())
}

fn elements(list: Option<web_sys::NodeList>) -> Vec<Element> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i)?.dyn_into::<Element>().ok())
        .collect()
}

fn text(el: &Element) -> String {
    el.text_content().unwrap_or_default()
}

fn button_in(root: &Element, label: &str) -> Option<Element> {
    find_all(root, "button")
        .into_iter()
        .find(|b| text(b).contains(label))
}

fn open_dialog(prefix: &str) -> Option<Element> {
    q(&format!("[data-name={prefix}Content][data-state=open]"))
}

fn panel() -> Option<Element> {
    q("[data-view=task-panel]")
}

fn panel_text() -> String {
    panel().map(|p| text(&p)).unwrap_or_default()
}

/// The panel header: title, status, branch, running and approval badges.
fn panel_header() -> String {
    panel()
        .and_then(|p| find_in(&p, "header"))
        .map(|h| text(&h))
        .unwrap_or_default()
}

fn entries() -> Vec<Element> {
    q_all("[data-view=task-panel] [data-entry]")
}

fn turn_ends() -> usize {
    q_all("[data-view=task-panel] [data-turn-end]").len()
}

fn session_inits() -> usize {
    entries()
        .iter()
        .filter(|e| text(e).contains("Sessione avviata"))
        .count()
}

fn card(task: &str) -> Option<Element> {
    q(&format!("[data-column] [data-task-id=\"{task}\"]"))
}

fn card_ids() -> Vec<String> {
    q_all("[data-column] [data-task-id]")
        .iter()
        .filter_map(|c| c.get_attribute("data-task-id"))
        .collect()
}

fn column_ids(status: &str) -> Vec<String> {
    q_all(&format!("[data-column={status}] [data-task-id]"))
        .iter()
        .filter_map(|c| c.get_attribute("data-task-id"))
        .collect()
}

fn column_of(task: &str) -> Option<String> {
    card(task)?
        .closest("[data-column]")
        .ok()
        .flatten()?
        .get_attribute("data-column")
}

fn badge(task: &str, name: &str) -> Option<Element> {
    q(&format!(
        "[data-column] [data-task-id=\"{task}\"] [data-badge={name}]"
    ))
}

/// The project's button in the sidebar (its label span: the icon's SVG has a `<title>`).
fn project_button(name: &str) -> Option<Element> {
    q_all("[data-testid=projects] button")
        .into_iter()
        .find(|b| project_label(b) == name)
}

fn project_label(button: &Element) -> String {
    find_in(button, "span")
        .map(|s| text(&s).trim().to_owned())
        .unwrap_or_default()
}

fn project_names() -> Vec<String> {
    q_all("[data-testid=projects] button")
        .iter()
        .map(project_label)
        .collect()
}

fn rect_top(el: &Element) -> f64 {
    el.get_bounding_client_rect().top()
}

async fn wait_q(sel: &str) -> R<Element> {
    until(sel, UI, || q(sel)).await
}

async fn until_order(status: &str, ids: &[&String]) -> R {
    until(&format!("{status} order {ids:?}"), UI, || {
        let now = column_ids(status);
        (now.len() == ids.len() && now.iter().zip(ids).all(|(a, b)| a == *b)).then_some(())
    })
    .await
}

/// Polls `f` every 50 ms for up to `ms`.
async fn until<T>(what: &str, ms: u32, mut f: impl FnMut() -> Option<T>) -> R<T> {
    let deadline = js_sys::Date::now() + f64::from(ms);
    loop {
        if let Some(v) = f() {
            return Ok(v);
        }
        if js_sys::Date::now() > deadline {
            return Err(format!("timeout after {ms} ms: {what}"));
        }
        sleep(50).await;
    }
}

/// Like [`until`] for a check that needs IPC, every 200 ms.
async fn until_async<T, F: Future<Output = Option<T>>>(
    what: &str,
    ms: u32,
    mut f: impl FnMut() -> F,
) -> R<T> {
    let deadline = js_sys::Date::now() + f64::from(ms);
    loop {
        if let Some(v) = f().await {
            return Ok(v);
        }
        if js_sys::Date::now() > deadline {
            return Err(format!("timeout after {ms} ms: {what}"));
        }
        sleep(200).await;
    }
}

fn click(el: &Element) {
    el.unchecked_ref::<HtmlElement>().click();
}

/// A DOM event built with its constructor (`Event`, `KeyboardEvent`, `DragEvent`), bubbling
/// and cancelable, with `props` in its init dictionary.
fn event(class: &str, kind: &str, props: &[(&str, JsValue)]) -> R<web_sys::Event> {
    let init = Object::new();
    let set = |k: &str, v: &JsValue| Reflect::set(&init, &k.into(), v).map(drop);
    let js = |e: JsValue| format!("{class}: {e:?}");
    set("bubbles", &true.into()).map_err(js)?;
    set("cancelable", &true.into()).map_err(js)?;
    for (k, v) in props {
        set(k, v).map_err(js)?;
    }
    let ctor: Function = Reflect::get(&window(), &class.into())
        .map_err(js)?
        .dyn_into()
        .map_err(js)?;
    let ev = Reflect::construct(&ctor, &Array::of2(&kind.into(), &init)).map_err(js)?;
    Ok(ev.unchecked_into())
}

/// Dispatches `ev`; `false` if a listener cancelled it (`preventDefault`).
fn fire(target: &Element, ev: &web_sys::Event) -> bool {
    target.dispatch_event(ev).unwrap_or(false)
}

/// Sets an input's or textarea's value and fires `input` (Leptos' `bind:value`).
fn set_value(el: &Element, value: &str) -> R {
    Reflect::set(el, &"value".into(), &value.into()).map_err(|e| format!("{e:?}"))?;
    fire(el, &event("Event", "input", &[])?);
    Ok(())
}

fn set_select(el: &Element, value: &str) -> R {
    Reflect::set(el, &"value".into(), &value.into()).map_err(|e| format!("{e:?}"))?;
    fire(el, &event("Event", "change", &[])?);
    Ok(())
}

fn key(el: &Element, key: &str) -> R {
    key_with(el, key, false)
}

fn key_with(el: &Element, key: &str, meta: bool) -> R {
    let ev = event(
        "KeyboardEvent",
        "keydown",
        &[("key", key.into()), ("metaKey", meta.into())],
    )?;
    fire(el, &ev);
    Ok(())
}

/// HTML5 drag of a card onto a column at `client_y`: `dragstart` on the card, `dragenter` and
/// `dragover` on the column's card list, `drop` there, `dragend` on the card. What a native
/// WebKit drag needs is asserted, since synthetic events would go through without it:
/// `dragstart` is not cancelled and puts the id in the `DataTransfer`; `dragover` is cancelled
/// (without that WebKit never fires `drop`) while its `DataTransfer` exposes no data (WebKit's
/// protected mode during a drag); `drop` is cancelled too (no default action on the dropped
/// text). (`effectAllowed` cannot be checked: WebKit ignores it on a constructed
/// `DataTransfer`, the synthetic `dragstart` reads back "none".)
async fn drag(task: &str, status: &str, client_y: f64) -> R {
    let card = card(task).ok_or(format!("no card {task}"))?;
    let list = q(&format!("[data-column={status}] [data-card-list]"))
        .ok_or(format!("no column {status}"))?;
    let new_dt = || web_sys::DataTransfer::new().map_err(|e| format!("DataTransfer: {e:?}"));
    let dt = new_dt()?;
    let protected = new_dt()?;
    let rect = list.get_bounding_client_rect();
    let x = rect.left() + rect.width() / 2.0;
    // Past the column's end: just inside its list, below every card.
    let y = client_y.min(rect.bottom() - 1.0);
    let props = |dt: &web_sys::DataTransfer, y: f64| {
        [
            ("dataTransfer", JsValue::from(dt.clone())),
            ("clientX", x.into()),
            ("clientY", y.into()),
        ]
    };
    let start = rect_top(&card) + 2.0;
    if !fire(&card, &event("DragEvent", "dragstart", &props(&dt, start))?) {
        return Err("dragstart cancelled: WebKit would not start the drag".into());
    }
    let carried = dt.get_data("text/plain").unwrap_or_default();
    if carried != task {
        return Err(format!("dragstart set {carried:?}"));
    }
    // `DragCtx::start` records the dragged card in a zero-delay timeout.
    sleep(30).await;
    fire(
        &list,
        &event("DragEvent", "dragenter", &props(&protected, y))?,
    );
    if fire(
        &list,
        &event("DragEvent", "dragover", &props(&protected, y))?,
    ) {
        return Err(format!(
            "dragover on {status} not cancelled: WebKit would not drop"
        ));
    }
    sleep(30).await;
    if fire(&list, &event("DragEvent", "drop", &props(&dt, y))?) {
        return Err(format!("drop on {status} not cancelled"));
    }
    fire(&card, &event("DragEvent", "dragend", &props(&dt, y))?);
    Ok(())
}

/// Every distinct text of the transcript's typing preview (`[data-typing]`), recorded by a
/// `MutationObserver` on the task panel: unlike timers, it is not throttled in a background
/// window, so no intermediate rendering is missed.
struct TypingLog {
    seen: Rc<RefCell<(Vec<String>, bool)>>,
    observer: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl TypingLog {
    fn start(root: &Element) -> R<TypingLog> {
        let seen: Rc<RefCell<(Vec<String>, bool)>> = Rc::default();
        let sink = seen.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let Some(row) = q("[data-view=task-panel] [data-typing]") else {
                return;
            };
            let preview = find_in(&row, "p").map(|p| text(&p)).unwrap_or_default();
            let mut seen = sink.borrow_mut();
            seen.1 |= find_in(&row, "[role=status]").is_some();
            if !preview.is_empty() && seen.0.last() != Some(&preview) {
                seen.0.push(preview);
            }
        });
        let js = |e: JsValue| format!("MutationObserver: {e:?}");
        let ctor: Function = Reflect::get(&window(), &"MutationObserver".into())
            .map_err(js)?
            .dyn_into()
            .map_err(js)?;
        let observer = Reflect::construct(&ctor, &Array::of1(callback.as_ref())).map_err(js)?;
        let options = Object::new();
        for key in ["childList", "subtree", "characterData"] {
            Reflect::set(&options, &key.into(), &true.into()).map_err(js)?;
        }
        let observe: Function = Reflect::get(&observer, &"observe".into())
            .map_err(js)?
            .dyn_into()
            .map_err(js)?;
        observe.call2(&observer, root, &options).map_err(js)?;
        Ok(TypingLog {
            seen,
            observer,
            _callback: callback,
        })
    }

    /// The previews seen, in order, and whether the typing row showed its spinner.
    fn stop(self) -> (Vec<String>, bool) {
        self.seen.borrow().clone()
    }
}

impl Drop for TypingLog {
    fn drop(&mut self) {
        if let Ok(disconnect) = Reflect::get(&self.observer, &"disconnect".into())
            .and_then(|f| f.dyn_into::<Function>())
        {
            let _ = disconnect.call0(&self.observer);
        }
    }
}

// ---- toasts -----------------------------------------------------------------------------------

fn toasts() -> Vec<(u64, String)> {
    q_all("[data-toast]")
        .iter()
        .filter_map(|t| Some((t.get_attribute("data-toast")?.parse().ok()?, text(t))))
        .collect()
}

fn last_toast() -> u64 {
    toasts()
        .iter()
        .map(|(id, _)| *id)
        .max()
        .unwrap_or(0)
        .max(SEEN.get())
}

thread_local! {
    /// Highest toast id seen: toasts vanish after 5 s, ids only grow.
    static SEEN: Cell<u64> = const { Cell::new(0) };
}

/// A toast newer than `since` whose text satisfies `pred`.
async fn toast_after(since: u64, what: &str, pred: impl Fn(&str) -> bool) -> R<String> {
    let found = until(&format!("toast: {what}"), UI, || {
        toasts()
            .into_iter()
            .find(|(id, t)| *id > since && pred(t))
            .map(|(_, t)| t)
    })
    .await;
    let max = toasts().iter().map(|(id, _)| *id).max().unwrap_or(0);
    SEEN.set(SEEN.get().max(max));
    found.map_err(|e| format!("{e}; toasts {:?}", toasts()))
}

// ---- state across reloads ---------------------------------------------------------------------

fn load_state() -> Option<State> {
    let storage = window().session_storage().ok().flatten()?;
    let saved = storage.get_item(STATE_KEY).ok().flatten()?;
    let _ = storage.remove_item(STATE_KEY);
    serde_json::from_str(&saved).ok()
}

fn store_state(st: &State) {
    if let Some(storage) = window().session_storage().ok().flatten() {
        let _ = storage.set_item(STATE_KEY, &json!(st).to_string());
    }
}
