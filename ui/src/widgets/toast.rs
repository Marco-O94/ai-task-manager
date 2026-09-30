//! Toasts (spec §9.2): a `Vec` in a signal, at most 4 on screen, each removed after 5 s.

use std::time::Duration;

use atm_types::AppError;
use icons::{Check, Info, X};
use leptos::prelude::*;

use crate::widgets::status::FOCUS_RING;

const MAX_TOASTS: usize = 4;
const TOAST_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Info/Success: first used by the views of M2
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub text: String,
}

/// Handle to the toast list; `Copy`, lives in `AppCtx`.
#[derive(Clone, Copy)]
pub struct Toaster {
    toasts: RwSignal<Vec<Toast>>,
    next_id: StoredValue<u64>,
}

impl Default for Toaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Toaster {
    pub fn new() -> Self {
        Self {
            toasts: RwSignal::new(Vec::new()),
            next_id: StoredValue::new(0),
        }
    }

    /// Shows `text`; the oldest toast makes room past the limit.
    pub fn push(self, kind: ToastKind, text: impl Into<String>) {
        let Some(id) = self.next_id.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        let toast = Toast {
            id,
            kind,
            text: text.into(),
        };
        self.toasts.try_update(|list| {
            if list.len() >= MAX_TOASTS {
                list.remove(0);
            }
            list.push(toast);
        });
        set_timeout(move || self.dismiss(id), TOAST_TTL);
    }

    #[allow(dead_code)] // first used by the views of M2
    pub fn info(self, text: impl Into<String>) {
        self.push(ToastKind::Info, text);
    }

    #[allow(dead_code)] // first used by the views of M2
    pub fn success(self, text: impl Into<String>) {
        self.push(ToastKind::Success, text);
    }

    pub fn error(self, text: impl Into<String>) {
        self.push(ToastKind::Error, text);
    }

    /// `"<Code>: <message>"`, e.g. `NotImplemented: get_env: not implemented yet`.
    pub fn app_error(self, err: &AppError) {
        self.error(err.to_string());
    }

    pub fn dismiss(self, id: u64) {
        self.toasts.try_update(|list| list.retain(|t| t.id != id));
    }
}

/// Renders the toasts of `toaster`, bottom right, above dialogs: the kind as an icon in a
/// tinted circle, the text, a close button.
#[component]
pub fn Toasts(toaster: Toaster) -> impl IntoView {
    view! {
        <div
            class="pointer-events-none fixed right-5 bottom-5 z-[200] flex w-80 max-w-[calc(100%-2.5rem)] flex-col gap-2"
            role="status"
            aria-live="polite"
        >
            <For
                each=move || toaster.toasts.get()
                key=|t| t.id
                children=move |t| {
                    let (tone, icon) = match t.kind {
                        ToastKind::Info => ("bg-primary/12 text-primary", view! { <Info /> }.into_any()),
                        ToastKind::Success => {
                            ("bg-status-done/15 text-status-done", view! { <Check /> }.into_any())
                        }
                        ToastKind::Error => {
                            ("bg-status-failed/15 text-status-failed", view! { <X /> }.into_any())
                        }
                    };
                    let id = t.id;
                    view! {
                        <div
                            class="bg-popover text-popover-foreground border-border pointer-events-auto flex items-start gap-3 rounded-lg border p-3 shadow-md"
                            data-toast=id
                        >
                            // The text leads the DOM (the live region, and E2E checks read the
                            // toast's text from its start); the icon is shown first.
                            <p class="min-w-0 flex-1 font-medium break-words">{t.text}</p>
                            <span class=format!(
                                "order-first mt-0.5 grid size-5 shrink-0 place-items-center rounded-full [&_svg]:size-3 {tone}",
                            )>{icon}</span>
                            <button
                                type="button"
                                class=format!(
                                    "text-muted-foreground hover:text-foreground -mr-1 grid size-5 shrink-0 place-items-center rounded-sm [&_svg]:size-3.5 {FOCUS_RING}",
                                )
                                aria-label="Chiudi"
                                on:click=move |_| toaster.dismiss(id)
                            >
                                <X />
                            </button>
                        </div>
                    }
                }
            />
        </div>
    }
}
