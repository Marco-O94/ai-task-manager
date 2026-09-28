//! Sidebar (projects, "Aggiungi repository", settings) and topbar (account chip, pause banner
//! with "Riprendi", running counter), spec §9.2. Owner: M2-UI-BOARD.

use leptos::prelude::*;

use crate::views::settings::SettingsDialog;

/// Left column, 240 px.
#[component]
pub fn Sidebar() -> impl IntoView {
    let settings_open = RwSignal::new(false);
    view! {
        <nav class="flex w-60 shrink-0 flex-col border-r" data-view="sidebar">
            <div class="p-4 font-semibold">"AI Task Manager"</div>
            <SettingsDialog open=settings_open />
        </nav>
    }
}

/// Bar above the board and the task panel.
#[component]
pub fn Topbar() -> impl IntoView {
    view! { <header class="flex h-12 shrink-0 items-center border-b px-4" data-view="topbar"></header> }
}
