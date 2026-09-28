//! In-browser backend for `trunk serve --features mock` (spec D13), with the signatures of
//! `ipc::tauri`. This file routes commands by `NAME` and emulates events and channels; the
//! data lives in `board.rs` (env, settings, projects, board: M2-UI-BOARD) and `attempt.rs`
//! (task detail, attempts, transcript, diff, merge: M2-UI-TASK).
//!
//! Hooks for those files: [`emit`] (like `app.emit`), [`send_transcript`] (like
//! `Channel::send`), [`sleep`], [`now_ms`], [`new_id`]. `subscribe_transcript` and
//! `unsubscribe_transcript` are handled here and forwarded to `attempt::on_subscribe` /
//! `attempt::on_unsubscribe`. Between the two files: `board::{task, set_task_status,
//! update_env}` and `attempt::{decorate, forget_task}`.

mod attempt;
mod board;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use atm_types::*;
use js_sys::{Function, Object, Reflect};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use wasm_bindgen::prelude::*;

use super::{from_js, to_js};

/// Stand-in for `window.__TAURI__.core.Channel`: messages go to the returned closure.
pub struct Channel;

/// Simulated round trip, so loading states are visible.
const LATENCY_MS: i32 = 120;

/// Commands served by `board.rs`; every other §6.3 command goes to `attempt.rs`.
const BOARD_COMMANDS: &[&str] = &[
    GetEnv::NAME,
    OpenLoginTerminal::NAME,
    ResumeAgents::NAME,
    GetSettings::NAME,
    UpdateSettings::NAME,
    ListProjects::NAME,
    PickRepoFolder::NAME,
    AddProject::NAME,
    UpdateProject::NAME,
    SetProjectSecurity::NAME,
    RemoveProject::NAME,
    ListBranches::NAME,
    GetBoard::NAME,
    CreateTask::NAME,
    UpdateTask::NAME,
    MoveTask::NAME,
    DeleteTask::NAME,
    OpenUrl::NAME, // onboarding: install docs link
];

thread_local! {
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    /// `(listener id, event, handler)` registered by [`listen`].
    static LISTENERS: RefCell<Vec<(u32, String, Function)>> = const { RefCell::new(Vec::new()) };
    /// Transcript subscriptions: id → the handler returned by [`subscribe_transcript`].
    static CHANNELS: RefCell<HashMap<Id, Function>> = RefCell::new(HashMap::new());
}

fn next_id() -> u32 {
    NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

pub async fn call<C: Command>(req: &C::Req) -> Result<C::Res, AppError> {
    let req = serde_json::to_value(req)?;
    sleep(LATENCY_MS).await;
    let res = match C::NAME {
        SubscribeTranscript::NAME => Err(AppError::invalid("use ipc::subscribe_transcript")),
        UnsubscribeTranscript::NAME => {
            let req: UnsubscribeTranscriptReq = serde_json::from_value(req)?;
            CHANNELS.with(|c| c.borrow_mut().remove(&req.subscription_id));
            attempt::on_unsubscribe(&req.subscription_id);
            Ok(Value::Null)
        }
        name if BOARD_COMMANDS.contains(&name) => board::handle(name, req).await,
        name if COMMAND_NAMES.contains(&name) => attempt::handle(name, req).await,
        name => Err(AppError::not_implemented(name)),
    }?;
    Ok(serde_json::from_value(res)?)
}

/// Same contract as `ipc::tauri::listen`; events come from [`emit`].
pub async fn listen<T: DeserializeOwned + 'static>(
    event: &str,
    mut f: impl FnMut(T) + 'static,
) -> Result<(Closure<dyn FnMut(JsValue)>, Function), AppError> {
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
        let payload = Reflect::get(&ev, &"payload".into()).unwrap_or(JsValue::NULL);
        match from_js(&payload) {
            Ok(v) => f(v),
            Err(e) => leptos::logging::error!("event payload: {}", e.message),
        }
    });
    let id = next_id();
    let handler = closure.as_ref().unchecked_ref::<Function>().clone();
    LISTENERS.with(|l| l.borrow_mut().push((id, event.to_owned(), handler)));
    let unlisten = Closure::once_into_js(move || {
        LISTENERS.with(|l| l.borrow_mut().retain(|(i, ..)| *i != id));
    });
    Ok((closure, unlisten.unchecked_into()))
}

/// Same contract as `ipc::tauri::subscribe_transcript`: the handler is registered before
/// `attempt::on_subscribe` runs, so its first `Snapshot` is never lost.
#[allow(dead_code)] // first used by the transcript view (M2-UI-TASK)
pub async fn subscribe_transcript(
    attempt_id: &Id,
    mut f: impl FnMut(TranscriptMsg) + 'static,
) -> Result<(Id, Channel, Closure<dyn FnMut(JsValue)>), AppError> {
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| match from_js(&v) {
        Ok(msg) => f(msg),
        Err(e) => leptos::logging::error!("transcript message: {}", e.message),
    });
    sleep(LATENCY_MS).await;
    let sub_id = format!("mock-sub-{}", next_id());
    let handler = closure.as_ref().unchecked_ref::<Function>().clone();
    CHANNELS.with(|c| c.borrow_mut().insert(sub_id.clone(), handler));
    attempt::on_subscribe(&sub_id, attempt_id);
    Ok((sub_id, Channel, closure))
}

/// Delivers `payload` to every `listen(event)` handler, like `app.emit` (e.g. `changed`).
#[allow(dead_code)] // hook for board.rs / attempt.rs
pub(super) fn emit<T: Serialize>(event: &str, payload: &T) {
    let Ok(payload) = to_js(payload) else {
        return;
    };
    let ev = Object::new();
    let _ = Reflect::set(&ev, &"event".into(), &event.into());
    let _ = Reflect::set(&ev, &"payload".into(), &payload);
    // Collected first: a handler may listen or unlisten while it runs.
    let handlers: Vec<Function> = LISTENERS.with(|l| {
        l.borrow()
            .iter()
            .filter(|(_, e, _)| e == event)
            .map(|(_, _, h)| h.clone())
            .collect()
    });
    for h in handlers {
        let _ = h.call1(&JsValue::NULL, &ev);
    }
}

/// Sends one message to a transcript subscription, like `Channel::send`. `false` once the
/// subscription is gone (unsubscribed or its closure dropped): stop replaying.
#[allow(dead_code)] // hook for attempt.rs
pub(super) fn send_transcript(sub_id: &str, msg: &TranscriptMsg) -> bool {
    let Some(handler) = CHANNELS.with(|c| c.borrow().get(sub_id).cloned()) else {
        return false;
    };
    to_js(msg).is_ok_and(|v| handler.call1(&JsValue::NULL, &v).is_ok())
}

pub(super) async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[allow(dead_code)] // hook for board.rs / attempt.rs
pub(super) fn now_ms() -> Millis {
    js_sys::Date::now() as Millis
}

/// Random UUID-v4-shaped id.
#[allow(dead_code)] // hook for board.rs / attempt.rs
pub(super) fn new_id() -> Id {
    let hex = |n: usize| -> String {
        (0..n)
            .map(|_| char::from_digit((js_sys::Math::random() * 16.0) as u32, 16).unwrap_or('0'))
            .collect()
    };
    format!("{}-{}-4{}-a{}-{}", hex(8), hex(4), hex(3), hex(3), hex(12))
}
