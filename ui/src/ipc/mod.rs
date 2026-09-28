//! IPC facade (spec §6.6). The same functions run over Tauri or, with `--features mock`, over
//! the in-browser mock (spec D13):
//!
//! - `call::<C: Command>(&C::Req) -> Result<C::Res, AppError>`
//! - `listen::<T>(event, f)` → `(Closure, unlisten)`: `f` gets the event payload
//! - `subscribe_transcript(attempt_id, f)` → `(subscription_id, Channel, Closure)`
//!
//! Keep the returned `Channel`/`Closure`/unlisten alive (`StoredValue::new_local`) for as long
//! as messages are wanted; `on_cleanup` calls `unsubscribe_transcript` (spec §6.6).

#[cfg(feature = "mock")]
mod mock;
#[cfg(not(feature = "mock"))]
mod tauri;

// `Channel` and `subscribe_transcript` are first used by the transcript view (M2-UI-TASK).
#[cfg(feature = "mock")]
#[allow(unused_imports)]
pub use mock::{Channel, call, listen, subscribe_transcript};
#[cfg(not(feature = "mock"))]
#[allow(unused_imports)]
pub use tauri::{Channel, call, call_with_channel, listen, subscribe_transcript};

use atm_types::AppError;
use js_sys::JSON;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsValue;

/// Payloads cross the boundary as JSON text (`serde_json` ⇄ `JSON.parse`/`JSON.stringify`), so
/// the wire format is exactly serde's on both sides (spec D11).
fn to_js<T: Serialize + ?Sized>(v: &T) -> Result<JsValue, AppError> {
    let text = serde_json::to_string(v)?;
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
