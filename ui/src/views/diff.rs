//! Modifiche tab (spec §8.6, §9.2): branch status alerts, per-file diff viewer, Aggiorna,
//! Merge, "Risolvi con l'agente", Elimina branch. Owner: M2-UI-TASK.
#![allow(dead_code)] // M1 stub: remove when used by the task panel

use atm_types::{Id, Task};
use leptos::prelude::*;

/// Fetches `get_diff` and `get_branch_status` on open, on `changed` of the task while
/// visible, and on "Aggiorna". `task` feeds the merge dialog's default message.
#[component]
pub fn DiffView(attempt_id: Id, #[prop(into)] task: Signal<Task>) -> impl IntoView {
    let _ = (attempt_id, task);
}
