//! Follow-up composer (spec §7.9, §9.2): textarea + send (⌘↩), disabled while a turn runs.
//! Owner: M2-UI-TASK.
#![allow(dead_code)] // M1 stub: remove when used by the task panel

use atm_types::Id;
use leptos::prelude::*;

#[component]
pub fn Composer(attempt_id: Id, #[prop(into)] running: Signal<bool>) -> impl IntoView {
    let _ = (attempt_id, running);
}
