//! Follow-up composer (spec §7.9, §9.2): textarea + send (⌘↩), disabled while a turn runs.
//! Pinned under the transcript, with the focus ring on its box.
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
            class="shrink-0 border-t p-3"
            data-view="composer"
            on:submit=move |ev| {
                ev.prevent_default();
                send();
            }
        >
            // The ring is on the whole box, not on the borderless textarea.
            <div class="bg-card border-input focus-within:ring-ring rounded-lg border shadow-xs focus-within:ring-2">
                <label for="follow-up" class="sr-only">
                    "Messaggio all'agente"
                </label>
                <Textarea
                    bind_value=text
                    id="follow-up"
                    name="follow-up"
                    rows=2u32
                    class="block max-h-48 min-h-14 resize-none rounded-none border-0 bg-transparent px-3 pt-2.5 pb-1 text-[13px] shadow-none focus-visible:ring-0 md:text-[13px] dark:bg-transparent"
                    placeholder="Scrivi all'agente…"
                    attr:disabled=disabled
                    on:keydown=move |ev: KeyboardEvent| {
                        if ev.key() == "Enter" && (ev.meta_key() || ev.ctrl_key()) {
                            ev.prevent_default();
                            send();
                        }
                    }
                />
                <div class="flex items-center gap-2 px-3 pb-2">
                    <span class="text-muted-foreground min-w-0 text-[11px]">
                        {move || {
                            if running.get() {
                                "L'agente sta lavorando: potrai rispondere alla fine del turno."
                            } else {
                                "L'agente riceve il messaggio come nuovo turno."
                            }
                        }}
                    </span>
                    <span class="ml-auto flex shrink-0 items-center gap-2">
                        <KbdGroup>
                            <Kbd>"⌘"</Kbd>
                            <Kbd>"↩"</Kbd>
                        </KbdGroup>
                        <Button
                            size=ButtonSize::IconSm
                            class="size-7 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
                            attr:r#type="submit"
                            attr:aria-label="Invia"
                            attr:title="Invia (⌘↩)"
                            attr:disabled=move || disabled() || text.with(|t| t.trim().is_empty())
                        >
                            <Send />
                        </Button>
                    </span>
                </div>
            </div>
        </form>
    }
}
