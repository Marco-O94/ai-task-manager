//! Task panel (right split, 55 %): header (Avvia, Stop, Scarta, Apri in…, "Sposta in…") and
//! the Agente / Modifiche tabs (spec §9.2). Refetches `get_task_detail` when
//! `AppCtx::detail_version` changes. Owner: M2-UI-TASK.

use atm_types::Id;
use leptos::prelude::*;

use crate::app::use_app;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};

/// Mounted per task: `app.rs` re-creates it when `AppCtx::open_task` changes.
#[component]
pub fn TaskPanel(task_id: Id) -> impl IntoView {
    let ctx = use_app();
    view! {
        <section class="flex h-full flex-col" data-view="task-panel" data-task-id=task_id>
            <header class="flex items-center justify-end border-b p-2">
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::Sm
                    on:click=move |_| ctx.open_task.set(None)
                >
                    "Chiudi"
                </Button>
            </header>
        </section>
    }
}
