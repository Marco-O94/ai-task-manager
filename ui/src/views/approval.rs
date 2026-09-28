//! Approval card (spec §7.8, §9.2): tool, full input, reason; "Consenti", "Consenti sempre
//! (attempt)" only if `can_remember`, "Nega", "Nega e ferma", message field. Owner: M2-UI-TASK.

use atm_types::{
    ApprovalDecision, Entry, EntryBody, Id, RespondApproval, RespondApprovalReq, ToolStatus,
};
use icons::ShieldQuestion;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::use_app;
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::input::Input;

/// Sent with "Nega" when the message field is empty.
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
                        attr:disabled=move || busy.get()
                        attr:data-action="allow-always"
                        on:click=move |_| {
                            respond(id.get_value(), ApprovalDecision::Allow { remember: true })
                        }
                    >
                        "Consenti sempre (attempt)"
                    </Button>
                }
            });
            Some(view! {
                <div
                    class="border-warning bg-warning-light/40 dark:bg-warning-dark/20 flex flex-col gap-3 rounded-xl border p-4"
                    data-approval=approval_id.clone()
                >
                    <div class="flex items-start gap-2">
                        <ShieldQuestion class="mt-0.5 size-4 shrink-0" />
                        <div class="min-w-0">
                            <p class="text-sm font-medium">{format!("{name} chiede l'approvazione")}</p>
                            <p class="text-muted-foreground font-mono text-xs [overflow-wrap:anywhere]">
                                {summary.clone()}
                            </p>
                        </div>
                    </div>
                    {reason.clone().map(|r| view! { <p class="text-sm">"Motivo: " {r}</p> })}
                    <pre class="bg-background max-h-80 overflow-auto rounded-md border p-2 font-mono text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
                        {input}
                    </pre>
                    <Input
                        bind_value=message
                        name="deny-message"
                        placeholder="Messaggio per l'agente se neghi (facoltativo)"
                        class="bg-background"
                    />
                    <div class="flex flex-wrap gap-2">
                        <Button
                            size=ButtonSize::Sm
                            attr:disabled=move || busy.get()
                            attr:data-action="allow"
                            on:click=move |_| {
                                respond(id.get_value(), ApprovalDecision::Allow { remember: false })
                            }
                        >
                            "Consenti"
                        </Button>
                        {remember}
                        <Button
                            variant=ButtonVariant::Outline
                            size=ButtonSize::Sm
                            attr:disabled=move || busy.get()
                            attr:data-action="deny"
                            on:click=move |_| deny(id.get_value(), false)
                        >
                            "Nega"
                        </Button>
                        <Button
                            variant=ButtonVariant::Destructive
                            size=ButtonSize::Sm
                            attr:disabled=move || busy.get()
                            attr:data-action="deny-stop"
                            on:click=move |_| deny(id.get_value(), true)
                        >
                            "Nega e ferma"
                        </Button>
                    </div>
                </div>
            })
        })
        .flatten()
    }
}
