//! IPC and CSP selftest (spec §11.2 M0). Runs only when the debug backend reports
//! `ATM_SELFTEST=1`; in release the probe commands do not exist and this is a no-op.
//! Not compiled with `--features mock` (there is no backend to probe). Owner after M1:
//! M3-TAURI (subscribe, unsubscribe and reload with `debug_forwarder_count`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use atm_types::debug::{
    DebugChannelProbe, DebugPing, DebugSelftestEnabled, DebugSelftestReport, PingReq, ProbeMsg,
    ReportReq,
};
use atm_types::{AppError, Empty, ErrorCode};
use leptos::prelude::*;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use web_sys::SecurityPolicyViolationEvent;

use crate::ipc;
use crate::ui::dialog::{
    Dialog, DialogBody, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle,
    DialogTrigger,
};

const PROBE_COUNT: usize = 50;
const PROBE_BIG: [u32; 3] = [7, 23, 41];
const PROBE_BIG_LEN: usize = 20 * 1024;

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
    sleep(300).await; // let late violation events arrive
    let report: Value = json!({
        "ping_ok": ping_ok,
        "ping_err_typed": ping_err_typed,
        "channel_50_in_order": channel_50_in_order,
        "dialog_ok": dialog_ok,
        "csp_enforced": csp_enforced,
        "csp_violations": CSP_VIOLATIONS.with(Cell::get),
    });
    if let Err(e) = ipc::call::<DebugSelftestReport>(&ReportReq { report }).await {
        leptos::logging::error!("selftest report failed: {e:?}");
    }
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
