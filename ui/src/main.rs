mod app;
#[rustfmt::skip] // vendored Rust/UI code, kept byte-identical to upstream
mod hooks;
mod ipc;
mod selftest;
#[rustfmt::skip] // vendored Rust/UI code, kept byte-identical to upstream
mod ui;

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    selftest::count_csp_violations();
    apply_color_scheme();
    leptos::mount::mount_to_body(app::App);
    leptos::task::spawn_local(selftest::run_if_enabled());
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
