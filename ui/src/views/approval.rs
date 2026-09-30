//! Approval card (spec §7.8, §9.2): tool, full input, reason; "Approva", "Approva sempre"
//! (for the attempt) only if `can_remember`, "Rifiuta", "Rifiuta e ferma", message field.
//! Owner: M2-UI-TASK.

use atm_types::{
    ApprovalDecision, Entry, EntryBody, Id, RespondApproval, RespondApprovalReq, ToolStatus,
};
use icons::ShieldQuestion;
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;

use crate::app::use_app;
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::input::Input;

/// Compact buttons of the card, with the design's focus ring (`status::FOCUS_RING`).
const BUTTON: &str = "h-7 px-2.5 text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";

/// Sent with "Rifiuta" when the message field is empty.
const DEFAULT_DENY_MESSAGE: &str = "No.";

/// `entry` is a `ToolCall` in `AwaitingApproval`; answers with `respond_approval`.
#[component]
pub fn ApprovalCard(attempt_id: Id, #[prop(into)] entry: Signal<Entry>) -> impl IntoView {
    let ctx = use_app();
    let attempt_id = StoredValue::new(attempt_id);
    let message = RwSignal::new(String::new());
    // Answer sent: the buttons stay disabled until the upsert replaces the card.
    let busy = RwSignal::new(false);

    let respond = move |approval_id: Id, decision: ApprovalDecision| {
        busy.set(true);
        let req = RespondApprovalReq {
            attempt_id: attempt_id.get_value(),
            approval_id,
            decision,
        };
        spawn_local(async move {
            if let Err(e) = ipc::call::<RespondApproval>(&req).await {
                ctx.toasts.app_error(&e);
                busy.try_set(false);
            }
        });
    };
    let deny = move |approval_id: Id, interrupt: bool| {
        let text = message.get_untracked();
        let text = text.trim();
        let message = if text.is_empty() {
            DEFAULT_DENY_MESSAGE
        } else {
            text
        };
        respond(
            approval_id,
            ApprovalDecision::Deny {
                message: message.to_owned(),
                interrupt,
            },
        );
    };

    move || {
        entry.try_with(|e| {
            let EntryBody::ToolCall {
                name,
                summary,
                input,
                status:
                    ToolStatus::AwaitingApproval {
                        approval_id,
                        can_remember,
                        reason,
                    },
                ..
            } = &e.body
            else {
                return None;
            };
            let input = serde_json::from_str::<serde_json::Value>(input)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or_else(|_| input.clone());
            let id = StoredValue::new(approval_id.clone());
            let remember = can_remember.then(|| {
                view! {
                    <Button
                        variant=ButtonVariant::Outline
                        size=ButtonSize::Sm
                        class=BUTTON
                        attr:title="Approva le richieste uguali per il resto del tentativo"
                        attr:disabled=move || busy.get()
                        attr:data-action="allow-always"
                        on:click=move |_| {
                            respond(id.get_value(), ApprovalDecision::Allow { remember: true })
                        }
                    >
                        "Approva sempre"
                    </Button>
                }
            });
            // The only coloured block of the transcript (design: amber border, soft fill).
            Some(view! {
                <section
                    aria-label="Richiesta di permesso"
                    class="border-status-waiting/40 bg-status-waiting/8 overflow-hidden rounded-lg border"
                    data-approval=approval_id.clone()
                >
                    <div class="flex items-center gap-2 px-3 pt-3">
                        <ShieldQuestion class="text-status-waiting size-4 shrink-0" />
                        <h3 class="text-[13px] font-semibold">"Richiesta di permesso"</h3>
                        <span class="bg-background rounded-sm px-1.5 font-mono text-[11px]">{name.clone()}</span>
                    </div>
                    <p class="text-muted-foreground px-3 pt-1.5 font-mono text-[11px] [overflow-wrap:anywhere]">
                        {summary.clone()}
                    </p>
                    <pre class="bg-background mx-3 mt-2.5 max-h-80 overflow-auto rounded-md border px-3 py-2 font-mono text-[12px] leading-[18px] whitespace-pre-wrap [overflow-wrap:anywhere]">
                        {input}
                    </pre>
                    {reason
                        .clone()
                        .map(|r| {
                            view! { <p class="text-muted-foreground px-3 pt-2 text-xs">"Motivo: " {r}</p> }
                        })}
                    <div class="px-3 pt-3">
                        <Input
                            bind_value=message
                            name="deny-message"
                            placeholder="Messaggio per l'agente se rifiuti (facoltativo)"
                            class="bg-background h-8 text-[13px] md:text-[13px]"
                        />
                    </div>
                    <div class="flex flex-wrap items-center gap-2 p-3">
                        <Button
                            size=ButtonSize::Sm
                            class=BUTTON
                            attr:disabled=move || busy.get()
                            attr:data-action="allow"
                            on:click=move |_| {
                                respond(id.get_value(), ApprovalDecision::Allow { remember: false })
                            }
                        >
                            "Approva"
                        </Button>
                        {remember}
                        <Button
                            variant=ButtonVariant::Ghost
                            size=ButtonSize::Sm
                            class=tw_merge!(BUTTON, "text-muted-foreground ml-auto")
                            attr:disabled=move || busy.get()
                            attr:data-action="deny"
                            on:click=move |_| deny(id.get_value(), false)
                        >
                            "Rifiuta"
                        </Button>
                        <Button
                            variant=ButtonVariant::Ghost
                            size=ButtonSize::Sm
                            class=tw_merge!(BUTTON, "text-destructive hover:bg-destructive/10 hover:text-destructive")
                            attr:disabled=move || busy.get()
                            attr:data-action="deny-stop"
                            on:click=move |_| deny(id.get_value(), true)
                        >
                            "Rifiuta e ferma"
                        </Button>
                    </div>
                </section>
            })
        })
        .flatten()
    }
}
