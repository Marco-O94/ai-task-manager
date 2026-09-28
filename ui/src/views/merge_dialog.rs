//! Squash-merge dialog (spec §8.7): editable message, outcome (Merged, NothingToMerge,
//! Conflicts, TargetCheckoutDirty). Owner: M2-UI-TASK.
#![allow(dead_code)] // M1 stub: remove when used by the diff view

use atm_types::{Id, Task};
use leptos::prelude::*;

/// The message starts as `atm_types::merge_message(title, description, attempt_id)`.
#[component]
pub fn MergeDialog(
    open: RwSignal<bool>,
    attempt_id: Id,
    #[prop(into)] task: Signal<Task>,
) -> impl IntoView {
    let _ = (open, attempt_id, task);
}
