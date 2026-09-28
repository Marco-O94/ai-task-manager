//! "Avvia" dialog (spec §9.2): target branch, model, effort, permission mode (Autonomo only
//! with `allow_bypass`, with the bypass callout) → `start_attempt`. Owner: M2-UI-TASK.

use atm_types::Id;
use leptos::prelude::*;

/// Open while `task_id` is `Some`; closing or starting sets it to `None`. Used by the board
/// (drop on "In corso") and by the task panel.
#[component]
pub fn StartDialog(task_id: RwSignal<Option<Id>>) -> impl IntoView {
    let _ = task_id;
}
