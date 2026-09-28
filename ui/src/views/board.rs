//! Kanban board of the selected project (spec §9.2, §9.3): five columns (Annullati
//! collapsed), drag-and-drop, quick create at the bottom of a column. Refetches `get_board`
//! when `AppCtx::project` or `AppCtx::board_version` changes. Owner: M2-UI-BOARD.

use atm_types::Id;
use leptos::prelude::*;

use crate::ui::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::views::start_dialog::StartDialog;
use crate::views::task_dialog::{TaskDialog, TaskDialogMode};

#[component]
pub fn Board() -> impl IntoView {
    // A drop on "In corso" of a task without attempt opens the Start dialog instead of moving.
    let start_for = RwSignal::new(None::<Id>);
    let task_dialog = RwSignal::new(None::<TaskDialogMode>);
    view! {
        <section class="flex h-full items-center justify-center p-8" data-view="board">
            <Empty>
                <EmptyHeader>
                    <EmptyTitle>"Nessun progetto"</EmptyTitle>
                    <EmptyDescription>"Aggiungi un repository per iniziare."</EmptyDescription>
                </EmptyHeader>
            </Empty>
            <StartDialog task_id=start_for />
            <TaskDialog mode=task_dialog />
        </section>
    }
}
