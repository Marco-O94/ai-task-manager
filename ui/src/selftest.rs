//! IPC and CSP selftest (spec §11.2 M0, M3-TAURI). Runs only when the debug backend reports
//! `ATM_SELFTEST=1`; in release the probe commands do not exist and this is a no-op.
//! Not compiled with `--features mock` (there is no backend to probe).
//!
//! Two page loads: the first runs the checks, leaves a transcript subscription open, keeps
//! its partial report in `sessionStorage` and reloads; the second checks that the reload
//! dropped that forwarder (`on_page_load` → `drop_subscriptions`) and sends the report.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use atm_types::debug::{
    DebugChannelProbe, DebugForwarderCount, DebugPing, DebugSelftestEnabled, DebugSelftestReport,
    PingReq, ProbeMsg, ReportReq,
};
use atm_types::{
    AppError, Empty, ErrorCode, Id, TranscriptMsg, UnsubscribeTranscript, UnsubscribeTranscriptReq,
};
use leptos::prelude::*;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use web_sys::{SecurityPolicyViolationEvent, Storage};

use crate::ipc;
use crate::ui::dialog::{
    Dialog, DialogBody, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle,
    DialogTrigger,
};

const PROBE_COUNT: usize = 50;
const PROBE_BIG: [u32; 3] = [7, 23, 41];
const PROBE_BIG_LEN: usize = 20 * 1024;
/// `sessionStorage` key carrying the first load's results across the reload.
const STAGE_KEY: &str = "atm-selftest-first-load";
/// No attempt has this id: subscribing to it must yield an empty `Snapshot`.
const UNKNOWN_ATTEMPT: &str = "00000000-0000-4000-8000-000000000000";

/// A check that needs the M3 core: `None` (null in the report) while the core is the M1
/// stub and answers `NotImplemented`. The backend requires it only with `ATM_SELFTEST_FULL=1`.
type CoreCheck = Option<bool>;

thread_local! {
    static CSP_VIOLATIONS: Cell<u32> = const { Cell::new(0) };
}

/// Counts `securitypolicyviolation` events. Call before mounting so the UI's own
/// rendering is covered. The deliberate `eval` probe below is not counted.
pub fn count_csp_violations() {
    let on_violation = Closure::<dyn FnMut(SecurityPolicyViolationEvent)>::new(
        |e: SecurityPolicyViolationEvent| {
            if e.blocked_uri() != "eval" {
                leptos::logging::warn!(
                    "CSP violation: {} blocked {}",
                    e.effective_directive(),
                    e.blocked_uri()
                );
                CSP_VIOLATIONS.with(|c| c.set(c.get() + 1));
            }
        },
    );
    let _ = document().add_event_listener_with_callback(
        "securitypolicyviolation",
        on_violation.as_ref().unchecked_ref(),
    );
    on_violation.forget();
}

pub async fn run_if_enabled() {
    if !ipc::call::<DebugSelftestEnabled>(&Empty {})
        .await
        .unwrap_or(false)
    {
        return;
    }
    let storage = window().session_storage().ok().flatten();
    let saved = storage
        .as_ref()
        .and_then(|s| s.get_item(STAGE_KEY).ok().flatten());
    match (storage, saved) {
        (Some(storage), Some(saved)) => {
            let _ = storage.remove_item(STAGE_KEY);
            after_reload(&saved).await;
        }
        (storage, _) => first_load(storage).await,
    }
}

async fn first_load(storage: Option<Storage>) {
    let ping_ok = ping_ok().await;
    let ping_err_typed = ping_err_typed().await;
    let channel_50_in_order = channel_in_order().await;
    let dialog_ok = dialog_ok().await;
    // Proves the CSP is actually applied: without 'unsafe-eval' a constant eval must throw.
    // Tauri applies the CSP only to embedded assets, not to the `trunk serve` page used by
    // `cargo tauri dev`, so there the check is reported as null (not evaluated).
    let dev_server = window()
        .location()
        .host()
        .is_ok_and(|h| h == "localhost:1420");
    let csp_enforced = (!dev_server).then(|| js_sys::eval("1").is_err());
    let (transcript_subscribe_ok, forwarder_unsub_ok) = subscribe_unsubscribe().await;
    let left_open = leave_subscribed().await;
    sleep(300).await; // let late violation events arrive
    let mut report: Value = json!({
        "ping_ok": ping_ok,
        "ping_err_typed": ping_err_typed,
        "channel_50_in_order": channel_50_in_order,
        "dialog_ok": dialog_ok,
        "csp_enforced": csp_enforced,
        "transcript_subscribe_ok": transcript_subscribe_ok,
        "forwarder_unsub_ok": forwarder_unsub_ok,
        "csp_violations": CSP_VIOLATIONS.with(Cell::get),
    });
    let stage = json!({ "report": report, "left_open": left_open }).to_string();
    let stored = storage.is_some_and(|s| s.set_item(STAGE_KEY, &stage).is_ok());
    if !(stored && window().location().reload().is_ok()) {
        report["reload_ok"] = false.into();
        send_report(report).await;
    }
}

/// Second load: the forwarder left open by [`first_load`] must be gone before anything
/// subscribes again, and subscribing must still work.
async fn after_reload(saved: &str) {
    let saved: Value = serde_json::from_str(saved).unwrap_or_default();
    let mut report = saved["report"].clone();
    let forwarder_reload_ok: CoreCheck = match saved["left_open"].as_bool() {
        Some(true) => {
            let dropped = forwarders_reach(0).await;
            let (subscribed, unsubscribed) = subscribe_unsubscribe().await;
            Some(dropped && subscribed == Some(true) && unsubscribed == Some(true))
        }
        left_open => left_open,
    };
    sleep(300).await; // let late violation events arrive
    let csp_violations = report["csp_violations"]
        .as_u64()
        .map(|first| first + u64::from(CSP_VIOLATIONS.with(Cell::get)));
    report["forwarder_reload_ok"] = forwarder_reload_ok.into();
    report["reload_ok"] = true.into();
    report["csp_violations"] = csp_violations.into();
    send_report(report).await;
}

async fn send_report(report: Value) {
    if let Err(e) = ipc::call::<DebugSelftestReport>(&ReportReq { report }).await {
        leptos::logging::error!("selftest report failed: {e:?}");
    }
}

/// A live subscription to [`UNKNOWN_ATTEMPT`].
struct Subscription {
    id: Id,
    /// The first message was an empty `Snapshot` with `has_more: false`.
    snapshot_ok: bool,
    _handles: (ipc::Channel, Closure<dyn FnMut(JsValue)>),
}

async fn subscribe() -> Result<Subscription, ErrorCode> {
    let first: Rc<RefCell<Option<TranscriptMsg>>> = Rc::default();
    let sink = first.clone();
    let (id, channel, closure) =
        ipc::subscribe_transcript(&UNKNOWN_ATTEMPT.to_owned(), move |msg| {
            sink.borrow_mut().get_or_insert(msg);
        })
        .await
        .map_err(|e| e.code)?;
    for _ in 0..40 {
        if first.borrow().is_some() {
            break;
        }
        sleep(50).await;
    }
    let snapshot_ok = matches!(
        first.borrow().as_ref(),
        Some(TranscriptMsg::Snapshot { entries, has_more: false, .. }) if entries.is_empty()
    );
    Ok(Subscription {
        id,
        snapshot_ok,
        _handles: (channel, closure),
    })
}

/// Subscribe (empty `Snapshot`, 1 forwarder), unsubscribe (0 forwarders):
/// `(transcript_subscribe_ok, forwarder_unsub_ok)`.
async fn subscribe_unsubscribe() -> (CoreCheck, CoreCheck) {
    let sub = match subscribe().await {
        Ok(sub) => sub,
        Err(ErrorCode::NotImplemented) => return (None, None),
        Err(_) => return (Some(false), Some(false)),
    };
    let counted = forwarders_reach(1).await;
    let req = UnsubscribeTranscriptReq {
        subscription_id: sub.id.clone(),
    };
    let unsubscribed = ipc::call::<UnsubscribeTranscript>(&req).await.is_ok();
    let dropped = forwarders_reach(0).await;
    (
        Some(sub.snapshot_ok),
        Some(counted && unsubscribed && dropped),
    )
}

/// Subscribes and leaks the handles until the reload, which must drop the forwarder.
async fn leave_subscribed() -> CoreCheck {
    match subscribe().await {
        Ok(sub) => {
            let ok = sub.snapshot_ok && forwarders_reach(1).await;
            std::mem::forget(sub);
            Some(ok)
        }
        Err(ErrorCode::NotImplemented) => None,
        Err(_) => Some(false),
    }
}

/// Polls `debug_forwarder_count` for up to 1 s: a forwarder may deregister asynchronously.
async fn forwarders_reach(n: u32) -> bool {
    for _ in 0..20 {
        if ipc::call::<DebugForwarderCount>(&Empty {})
            .await
            .is_ok_and(|c| c == n)
        {
            return true;
        }
        sleep(50).await;
    }
    false
}

async fn ping_ok() -> bool {
    ipc::call::<DebugPing>(&PingReq { fail: false })
        .await
        .is_ok_and(|s| s == "pong")
}

async fn ping_err_typed() -> bool {
    matches!(
        ipc::call::<DebugPing>(&PingReq { fail: true }).await,
        Err(AppError { code: ErrorCode::Invalid, message }) if !message.is_empty()
    )
}

async fn channel_in_order() -> bool {
    let received: Rc<RefCell<Vec<Option<ProbeMsg>>>> = Rc::default();
    let sink = received.clone();
    let result = ipc::call_with_channel::<DebugChannelProbe, ProbeMsg>(&Empty {}, move |m| {
        sink.borrow_mut().push(m.ok());
    })
    .await;
    // Keep channel and closure alive until every message has arrived.
    let Ok((sent, _channel, _closure)) = result else {
        return false;
    };
    for _ in 0..100 {
        if received.borrow().len() >= PROBE_COUNT {
            break;
        }
        sleep(50).await;
    }
    let received = received.borrow();
    sent as usize == PROBE_COUNT
        && received.len() == PROBE_COUNT
        && received
            .iter()
            .enumerate()
            .all(|(idx, m)| m.as_ref().is_some_and(|m| probe_intact(idx, m)))
}

/// Mounts a ported dialog in its own container: the app shows none at startup.
fn mount_dialog_fixture() -> Option<web_sys::Element> {
    let host = document().create_element("div").ok()?;
    host.set_attribute("data-selftest", "dialog").ok()?;
    document().body()?.append_child(&host).ok()?;
    leptos::mount::mount_to(host.clone().unchecked_into(), || {
        let open = RwSignal::new(false);
        view! {
            <Dialog open>
                <DialogTrigger>"Selftest"</DialogTrigger>
                <DialogContent>
                    <DialogBody>
                        <DialogHeader>
                            <DialogTitle>"Selftest"</DialogTitle>
                        </DialogHeader>
                        <DialogFooter>
                            <DialogClose>"Chiudi"</DialogClose>
                        </DialogFooter>
                    </DialogBody>
                </DialogContent>
            </Dialog>
        }
    })
    .forget();
    Some(host)
}

/// Drives the ported dialog fixture: trigger opens it and locks scroll, Esc and a backdrop
/// click close it.
async fn dialog_ok() -> bool {
    let doc = document();
    let Some(host) = mount_dialog_fixture() else {
        return false;
    };
    let find = |sel: &str| host.query_selector(sel).ok().flatten();
    let (Some(trigger), Some(content), Some(backdrop)) = (
        find("[data-dialog-trigger]"),
        find("[data-name=DialogContent]"),
        find("[data-name=DialogBackdrop]"),
    ) else {
        return false;
    };
    let click = |el: &web_sys::Element| el.unchecked_ref::<web_sys::HtmlElement>().click();
    let is_open = || content.get_attribute("data-state").as_deref() == Some("open");
    let scroll_locked = || {
        doc.body()
            .and_then(|b| b.style().get_property_value("position").ok())
            .is_some_and(|p| p == "fixed")
    };

    click(&trigger);
    sleep(50).await;
    let opened = is_open() && scroll_locked();

    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    let Ok(esc) = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
    else {
        return false;
    };
    let _ = window().dispatch_event(&esc);
    sleep(50).await;
    let esc_closed = !is_open();

    click(&trigger);
    sleep(50).await;
    click(&backdrop);
    sleep(300).await; // unlock runs after the close animation
    opened && esc_closed && !is_open() && !scroll_locked()
}

fn probe_intact(idx: usize, m: &ProbeMsg) -> bool {
    if m.i as usize != idx {
        return false;
    }
    if PROBE_BIG.contains(&m.i) {
        let fill = char::from(b'a' + (m.i % 26) as u8);
        m.data.len() == PROBE_BIG_LEN && m.data.chars().all(|c| c == fill)
    } else {
        m.data.is_empty()
    }
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
