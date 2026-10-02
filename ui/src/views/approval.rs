//! Approval card (spec §7.8, §9.2): tool, full input, reason; "Approva", "Approva sempre"
//! (for the attempt) only if `can_remember`, "Rifiuta", "Rifiuta e ferma", message field. A
//! board tool (`mcp__atm__*`) reads as a sentence, its raw input folded. Owner: M2-UI-TASK.

use atm_types::{
    ApprovalDecision, Entry, EntryBody, Id, RespondApproval, RespondApprovalReq, TaskCard,
    TaskStatus, ToolStatus,
};
use icons::ShieldQuestion;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use tw_merge::tw_merge;

use crate::app::use_app;
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::input::Input;
use crate::views::board::column_title;

/// Compact buttons of the card, with the design's focus ring (`status::FOCUS_RING`).
const BUTTON: &str = "h-7 px-2.5 text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";

/// The tool's input, pretty-printed.
const INPUT: &str = "bg-background max-h-80 overflow-auto rounded-md border px-3 py-2 font-mono text-[12px] leading-[18px] whitespace-pre-wrap [overflow-wrap:anywhere]";

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
            let label = ctx.cards.with_untracked(|cards| board_tool_label(name, input, cards));
            let input = serde_json::from_str::<serde_json::Value>(input)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or_else(|_| input.clone());
            let input = match label {
                Some(label) => view! {
                    <p class="px-3 pt-1.5 text-[13px] font-medium [overflow-wrap:anywhere]" data-board-tool="">
                        {label}
                    </p>
                    <details class="group mx-3 mt-2">
                        <summary class="text-muted-foreground hover:text-foreground cursor-pointer text-xs">
                            "Input"
                        </summary>
                        <pre class=tw_merge!(INPUT, "mt-1.5")>{input}</pre>
                    </details>
                }
                .into_any(),
                None => view! {
                    <p class="text-muted-foreground px-3 pt-1.5 font-mono text-[11px] [overflow-wrap:anywhere]">
                        {summary.clone()}
                    </p>
                    <pre class=tw_merge!(INPUT, "mx-3 mt-2.5")>{input}</pre>
                }
                .into_any(),
            };
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
                    {input}
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

/// A board tool call (`mcp__atm__<tool>`, spec §7.8) as an Italian sentence, e.g. «Sposta «T»
/// in Fatto»; tasks by their title in `cards`, else by id. `None` for any other tool, or an
/// input that is not a JSON object.
pub fn board_tool_label(name: &str, input: &str, cards: &[TaskCard]) -> Option<String> {
    let tool = name.strip_prefix("mcp__atm__")?;
    let input: Value = serde_json::from_str(input).ok()?;
    let input = input.as_object()?;
    let text = |key: &str| input.get(key).and_then(Value::as_str);
    let task = || {
        let id = text("id").unwrap_or("?");
        let title = cards
            .iter()
            .find(|c| c.task.id == id)
            .map_or(id, |c| c.task.title.as_str());
        format!("«{title}»")
    };
    let column = |status: &str| {
        status
            .parse::<TaskStatus>()
            .map_or_else(|_| status.to_owned(), |s| column_title(s).to_owned())
    };
    Some(match tool {
        "list_tasks" => match text("status") {
            Some(status) => format!("Elenca i task in {}", column(status)),
            None => "Elenca i task".into(),
        },
        "get_task" => format!("Leggi {}", task()),
        "create_task" => {
            let title = text("title").unwrap_or_default();
            match text("parent_id") {
                Some(_) => format!("Crea il sotto task «{title}»"),
                None => format!("Crea il task «{title}»"),
            }
        }
        "update_task" => format!("Modifica {}", task()),
        "move_task" => format!("Sposta {} in {}", task(), column(text("status")?)),
        "start_task" => format!("Avvia l'agente su {}", task()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::board_tool_label;
    use atm_types::{Task, TaskCard, TaskStatus};

    fn card(id: &str, title: &str) -> TaskCard {
        TaskCard {
            task: Task {
                id: id.into(),
                project_id: "p".into(),
                title: title.into(),
                description: String::new(),
                status: TaskStatus::Todo,
                position: 1.0,
                created_at: 0,
                updated_at: 0,
                parent_id: None,
                auto: false,
                after_id: None,
                kind: Default::default(),
                launch: false,
            },
            attempt_id: None,
            attempt_state: None,
            branch: None,
            running: false,
            pending_approvals: 0,
            last_status: None,
            last_stop_reason: None,
            worktree_state: None,
            subtasks_done: 0,
            subtasks_total: 0,
            verifying: false,
            verify_state: None,
            verify_fixes: 0,
        }
    }

    #[test]
    fn board_tools_read_as_sentences() {
        let cards = [card("t1", "Scrivi i test")];
        let label = |name: &str, input: &str| board_tool_label(name, input, &cards);
        let moved = label("mcp__atm__move_task", r#"{"id":"t1","status":"done"}"#);
        assert_eq!(moved.as_deref(), Some("Sposta «Scrivi i test» in Fatto"));
        let unknown = label("mcp__atm__move_task", r#"{"id":"t9","status":"inreview"}"#);
        assert_eq!(unknown.as_deref(), Some("Sposta «t9» in In revisione"));
        let started = label("mcp__atm__start_task", r#"{"id":"t1","model":"haiku"}"#);
        assert_eq!(
            started.as_deref(),
            Some("Avvia l'agente su «Scrivi i test»")
        );
        let edited = label("mcp__atm__update_task", r#"{"id":"t1","title":"Nuovo"}"#);
        assert_eq!(edited.as_deref(), Some("Modifica «Scrivi i test»"));
        let created = label("mcp__atm__create_task", r#"{"title":"Docs"}"#);
        assert_eq!(created.as_deref(), Some("Crea il task «Docs»"));
        let child = label(
            "mcp__atm__create_task",
            r#"{"title":"Docs","parent_id":"self"}"#,
        );
        assert_eq!(child.as_deref(), Some("Crea il sotto task «Docs»"));
        let listed = label("mcp__atm__list_tasks", r#"{"status":"todo"}"#);
        assert_eq!(listed.as_deref(), Some("Elenca i task in Da fare"));
        assert_eq!(label("Bash", r#"{"command":"ls"}"#), None);
        assert_eq!(label("mcp__other__move_task", r#"{"id":"t1"}"#), None);
        assert_eq!(label("mcp__atm__move_task", "not json"), None);
        assert_eq!(label("mcp__atm__move_task", r#"{"id":"t1"}"#), None);
    }
}
