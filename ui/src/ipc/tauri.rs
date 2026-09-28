//! Bindings to the global Tauri API (`withGlobalTauri`), spec §6.6.
//!
//! Payloads cross the boundary as JSON text (`serde_json` <-> `JSON.parse`/`JSON.stringify`),
//! so the wire format is exactly serde's on both sides (D11).

use js_sys::{JSON, Object, Reflect};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::*;

use super::AppError;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn tauri_listen(
        event: &str,
        handler: &Closure<dyn FnMut(JsValue)>,
    ) -> Result<JsValue, JsValue>;

    /// `window.__TAURI__.core.Channel`: a callback the backend streams messages into.
    pub type Channel;

    #[wasm_bindgen(constructor, js_namespace = ["window", "__TAURI__", "core"])]
    pub fn new() -> Channel;

    #[wasm_bindgen(method, setter)]
    pub fn set_onmessage(this: &Channel, f: &Closure<dyn FnMut(JsValue)>);
}

fn to_js<T: Serialize + ?Sized>(v: &T) -> Result<JsValue, AppError> {
    let text = serde_json::to_string(v).map_err(|e| AppError::internal(e.to_string()))?;
    JSON::parse(&text).map_err(|e| AppError::internal(format!("JSON.parse: {e:?}")))
}

fn from_js<T: DeserializeOwned>(v: &JsValue) -> Result<T, AppError> {
    // `JSON.stringify(undefined)` is not a string: commands returning `()` resolve to it.
    let text = if v.is_undefined() {
        "null".to_owned()
    } else {
        JSON::stringify(v)
            .ok()
            .and_then(|s| s.as_string())
            .ok_or_else(|| AppError::internal("JSON.stringify failed"))?
    };
    serde_json::from_str(&text).map_err(|e| AppError::internal(format!("decode: {e}")))
}

/// A rejected invoke carries the serialized `AppError`, or a plain string for errors
/// raised by Tauri itself (unknown command, bad arguments).
fn decode_error(err: &JsValue) -> AppError {
    from_js(err).unwrap_or_else(|_| {
        AppError::internal(err.as_string().unwrap_or_else(|| format!("{err:?}")))
    })
}

fn args_object(req: &JsValue) -> Result<Object, AppError> {
    let args = Object::new();
    Reflect::set(&args, &"req".into(), req).map_err(|e| AppError::internal(format!("{e:?}")))?;
    Ok(args)
}

/// Invokes `cmd` with `{req}` and decodes the reply or the typed error.
pub async fn call<Req, Res>(cmd: &str, req: &Req) -> Result<Res, AppError>
where
    Req: Serialize + ?Sized,
    Res: DeserializeOwned,
{
    let args = args_object(&to_js(req)?)?;
    match tauri_invoke(cmd, args.into()).await {
        Ok(v) => from_js(&v),
        Err(e) => Err(decode_error(&e)),
    }
}

/// Invokes a streaming command with `{req, onEvent}`. The channel's `onmessage` is set
/// before the invoke, so no message can be missed. Keep the returned `Channel` and
/// `Closure` alive for as long as messages should be received.
pub async fn call_with_channel<Req, Res, Msg>(
    cmd: &str,
    req: &Req,
    mut on_msg: impl FnMut(Result<Msg, AppError>) + 'static,
) -> Result<(Res, Channel, Closure<dyn FnMut(JsValue)>), AppError>
where
    Req: Serialize + ?Sized,
    Res: DeserializeOwned,
    Msg: DeserializeOwned + 'static,
{
    let channel = Channel::new();
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| on_msg(from_js(&v)));
    channel.set_onmessage(&closure);

    let args = args_object(&to_js(req)?)?;
    Reflect::set(&args, &"onEvent".into(), &channel)
        .map_err(|e| AppError::internal(format!("{e:?}")))?;
    match tauri_invoke(cmd, args.into()).await {
        Ok(v) => Ok((from_js(&v)?, channel, closure)),
        Err(e) => Err(decode_error(&e)),
    }
}

/// Listens to a global event; `f` receives the event's `payload`. Keep the returned
/// `Closure` alive and call the returned unlisten function to stop.
#[allow(dead_code)] // first used in M1 (`changed`, `env_changed`)
pub async fn listen<T: DeserializeOwned + 'static>(
    event: &str,
    mut f: impl FnMut(T) + 'static,
) -> Result<(Closure<dyn FnMut(JsValue)>, js_sys::Function), AppError> {
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
        let payload = Reflect::get(&ev, &"payload".into()).unwrap_or(JsValue::NULL);
        match from_js(&payload) {
            Ok(v) => f(v),
            Err(e) => leptos::logging::error!("event payload: {}", e.message),
        }
    });
    let unlisten = tauri_listen(event, &closure)
        .await
        .map_err(|e| decode_error(&e))?;
    Ok((closure, unlisten.unchecked_into()))
}
