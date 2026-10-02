mod app;
// The selftest and E2E drivers exist only with `--features testkit` (debug bundles), never in the
// release WASM (spec §11.2 M6), and not in the mock (there is no backend to drive).
#[cfg(all(feature = "testkit", not(feature = "mock")))]
mod e2e;
#[rustfmt::skip] // vendored Rust/UI code, kept byte-identical to upstream
mod hooks;
mod ipc;
#[cfg(all(feature = "testkit", not(feature = "mock")))]
mod selftest;
#[rustfmt::skip] // vendored Rust/UI code, kept byte-identical to upstream
mod ui;

// Declared inline so that no `mod.rs` sits outside the file ownership of spec §11.2.
mod state {
    pub mod board;
    pub mod transcript;
}
mod views {
    pub mod approval;
    pub mod board;
    pub mod composer;
    pub mod diff;
    pub mod merge_dialog;
    pub mod onboarding;
    pub mod overview;
    pub mod planner;
    pub mod settings;
    pub mod sidebar;
    pub mod start_dialog;
    pub mod task_dialog;
    pub mod task_panel;
    pub mod transcript;
    pub mod update;
}
mod widgets {
    pub mod context_menu;
    pub mod dnd;
    pub mod status;
    pub mod toast;
}

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    #[cfg(all(feature = "testkit", not(feature = "mock")))]
    selftest::count_csp_violations();
    apply_color_scheme();
    leptos::mount::mount_to_body(app::App);
    #[cfg(all(feature = "testkit", not(feature = "mock")))]
    {
        leptos::task::spawn_local(selftest::run_if_enabled());
        leptos::task::spawn_local(e2e::run_if_enabled());
    }
}

/// Sets `dark` on `<html>` from the OS preference (manual toggle deferred, spec §9.2).
fn apply_color_scheme() {
    let dark = window()
        .match_media("(prefers-color-scheme: dark)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches());
    if let Some(root) = document().document_element() {
        let _ = root.class_list().toggle_with_force("dark", dark);
    }
}
