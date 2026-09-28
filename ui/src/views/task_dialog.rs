//! Create/edit task dialog (spec §9.2). Owner: M2-UI-BOARD.
#![allow(dead_code)] // M1 stub: remove when implemented

use atm_types::{Task, TaskStatus};
use leptos::prelude::*;

/// What the dialog is doing: create in a column, or edit an existing task.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskDialogMode {
    Create(TaskStatus),
    Edit(Task),
}

/// Open while `mode` is `Some`; closing or saving sets it to `None`.
#[component]
pub fn TaskDialog(mode: RwSignal<Option<TaskDialogMode>>) -> impl IntoView {
    let _ = mode;
}
