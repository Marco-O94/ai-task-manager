//! Follow-up composer (spec §7.9, §9.2): textarea + send (⌘↩), disabled while a turn runs.
//! Owner: M2-UI-TASK.

use atm_types::{Id, SendFollowUp, SendFollowUpReq};
use icons::Send;
use leptos::ev::KeyboardEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::use_app;
use crate::ipc;
use crate::ui::button::{Button, ButtonSize};
use crate::ui::kbd::{Kbd, KbdGroup};
use crate::ui::textarea::Textarea;

#[component]
pub fn Composer(attempt_id: Id, #[prop(into)] running: Signal<bool>) -> impl IntoView {
    let ctx = use_app();
    let attempt_id = StoredValue::new(attempt_id);
    let text = RwSignal::new(String::new());
    let sending = RwSignal::new(false);
    let disabled = move || running.get() || sending.get();

    let send = move || {
        let prompt = text.get_untracked().trim().to_owned();
        if prompt.is_empty() || running.get_untracked() || sending.get_untracked() {
            return;
        }
        sending.set(true);
        let req = SendFollowUpReq {
            attempt_id: attempt_id.get_value(),
            prompt,
            permission_mode: None,
            fresh_session: false,
        };
        spawn_local(async move {
            match ipc::call::<SendFollowUp>(&req).await {
                Ok(_) => {
                    text.try_set(String::new());
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
            sending.try_set(false);
        });
    };

    view! {
        <form
            class="flex shrink-0 flex-col gap-2 border-t p-3"
            data-view="composer"
            on:submit=move |ev| {
                ev.prevent_default();
                send();
            }
        >
            <Textarea
                bind_value=text
                name="follow-up"
                rows=3u32
                class="max-h-48 resize-none"
                placeholder="Scrivi un messaggio di follow-up…"
                attr:disabled=disabled
                on:keydown=move |ev: KeyboardEvent| {
                    if ev.key() == "Enter" && (ev.meta_key() || ev.ctrl_key()) {
                        ev.prevent_default();
                        send();
                    }
                }
            />
            <div class="flex items-center justify-between gap-2">
                <span class="text-muted-foreground flex items-center gap-1.5 text-xs">
                    {move || {
                        if running.get() {
                            "L'agente sta lavorando: potrai rispondere alla fine del turno.".into_any()
                        } else {
                            view! {
                                <KbdGroup>
                                    <Kbd>"⌘"</Kbd>
                                    <Kbd>"↩"</Kbd>
                                </KbdGroup>
                                " per inviare"
                            }
                                .into_any()
                        }
                    }}
                </span>
                <Button
                    size=ButtonSize::Sm
                    attr:r#type="submit"
                    attr:disabled=move || disabled() || text.with(|t| t.trim().is_empty())
                >
                    <Send />
                    "Invia"
                </Button>
            </div>
        </form>
    }
}
