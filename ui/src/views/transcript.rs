//! Agente tab: live transcript list (spec §9.4) with subagent nesting, typing preview,
//! "Carica precedenti", autoscroll and "Vai all'ultimo". Owner: M2-UI-TASK.
#![allow(dead_code)] // M1 stub: remove when used by the task panel

use atm_types::Id;
use leptos::prelude::*;

/// Subscribes on mount (`ipc::subscribe_transcript`), unsubscribes on cleanup.
#[component]
pub fn Transcript(attempt_id: Id) -> impl IntoView {
    let _ = attempt_id;
}
