//! Approval card (spec §7.8, §9.2): tool, full input, reason; "Consenti", "Consenti sempre
//! (attempt)" only if `can_remember`, "Nega", "Nega e ferma", message field. Owner: M2-UI-TASK.
#![allow(dead_code)] // M1 stub: remove when used by the transcript

use atm_types::{Entry, Id};
use leptos::prelude::*;

/// `entry` is a `ToolCall` in `AwaitingApproval`; answers with `respond_approval`.
#[component]
pub fn ApprovalCard(attempt_id: Id, #[prop(into)] entry: Signal<Entry>) -> impl IntoView {
    let _ = (attempt_id, entry);
}
