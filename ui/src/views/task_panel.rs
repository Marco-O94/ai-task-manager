//! Task panel (right split, 55 %): header on three rows (column and date with the actions:
//! Avvia, Merge, Stop, Continua, Scarta, Apri in…, "Sposta in…"; the title; the agent badge,
//! the branch and the attachments) plus the attempt's label/value line, and the Agente /
//! Modifiche tabs (spec §9.2). Refetches `get_task_detail` when `AppCtx::detail_version`
//! changes. Owner: M2-UI-TASK, attachments and model line UI-TASKS.

use atm_types::{
    AppError, AttemptIdReq, AttemptView, CONTINUE_PROMPT, GetTaskDetail, Id, IdReq, MoveTask,
    MoveTaskReq, OpenAttempt, OpenAttemptReq, OpenTarget, SendFollowUp, SendFollowUpReq,
    StopAttempt, Task, TaskCard, TaskDetail, TaskStatus, WorktreeState,
};
use icons::{
    Code, FolderOpen, GitBranch, GitMerge, Paperclip, Play, RotateCw, Square, SquareTerminal,
    Trash2, X,
};
use leptos::ev::KeyboardEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;
use wasm_bindgen::JsCast;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::alert::{Alert, AlertDescription, AlertTitle};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::select_native::SelectNative;
use crate::ui::skeleton::Skeleton;
use crate::ui::tabs::{Tabs, TabsContent, TabsList, TabsTrigger, TabsVariant};
use crate::ui::tooltip::{Tooltip, TooltipContent, TooltipPosition};
use crate::views::board::{column_title, interrupted_by_app, updated_text, updated_title};
use crate::views::composer::Composer;
use crate::views::diff::{ClosedAttempt, DiffView};
use crate::views::merge_dialog::DiscardDialog;
use crate::views::overview::short_commit;
use crate::views::start_dialog::StartDialog;
use crate::views::task_dialog::format_size;
use crate::views::transcript::{Transcript, mode_label};
use crate::widgets::status::{AgentBadge, FOCUS_RING, Status, StatusDot, agent_state};

const TAB_AGENT: &str = "agent";
const TAB_CHANGES: &str = "changes";

/// The header's compact buttons (h-7), with the design's focus ring.
const SMALL: &str = "h-7 px-2.5 text-xs has-[>svg]:px-2.5 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";
/// Line tab: the underline sits on the tab list's bottom border, in the primary colour.
const LINE_TAB: &str = "h-full flex-none rounded-none border-0 px-0 text-[13px] after:bg-primary group-data-[orientation=Horizontal]/tabs:after:bottom-[-1px] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";

/// Mounted per task: `app.rs` re-creates it when `AppCtx::open_task` changes.
#[component]
pub fn TaskPanel(task_id: Id) -> impl IntoView {
    let ctx = use_app();
    let id = StoredValue::new(task_id.clone());
    // Never reset to `None` once loaded: a failed refetch keeps the last detail.
    let detail = RwSignal::new(None::<TaskDetail>);
    let error = RwSignal::new(None::<AppError>);
    let retry = RwSignal::new(0u32);
    let fetches = StoredValue::new(0u64);

    Effect::new(move |_| {
        ctx.detail_version.track();
        retry.track();
        let seq = fetches
            .try_update_value(|n| {
                *n += 1;
                *n
            })
            .unwrap_or_default();
        let req = IdReq { id: id.get_value() };
        spawn_local(async move {
            let res = ipc::call::<GetTaskDetail>(&req).await;
            if fetches.try_get_value() != Some(seq) {
                return;
            }
            match res {
                Ok(d) => {
                    detail.try_set(Some(d));
                    error.try_set(None);
                }
                Err(e) => {
                    error.try_set(Some(e));
                }
            }
        });
    });

    let loaded = Memo::new(move |_| detail.with(Option::is_some));
    let close = move |_| ctx.open_task.set(None);

    view! {
        <section class="flex h-full flex-col" data-view="task-panel" data-task-id=task_id>
            {move || {
                if loaded.get() {
                    detail
                        .with_untracked(|d| d.as_ref().map(|d| d.task.clone()))
                        .map(|initial| view! { <PanelBody detail initial /> }.into_any())
                } else {
                    Some(
                        view! {
                            <header class="flex items-center gap-2 border-b px-5 py-3.5">
                                <Skeleton class="h-6 flex-1" />
                                <Button
                                    variant=ButtonVariant::Ghost
                                    size=ButtonSize::IconSm
                                    class=tw_merge!("text-muted-foreground size-7", FOCUS_RING)
                                    attr:aria-label="Chiudi"
                                    on:click=close
                                >
                                    <X />
                                </Button>
                            </header>
                        }
                            .into_any(),
                    )
                }
            }}
            {move || {
                error
                    .get()
                    .map(|e| {
                        view! {
                            <Alert class="border-destructive/50 text-destructive m-3 w-auto">
                                <AlertTitle>"Dettaglio del task non disponibile"</AlertTitle>
                                <AlertDescription>{e.to_string()}</AlertDescription>
                                <Button
                                    variant=ButtonVariant::Outline
                                    size=ButtonSize::Sm
                                    class="mt-2"
                                    on:click=move |_| retry.update(|n| *n += 1)
                                >
                                    "Riprova"
                                </Button>
                            </Alert>
                        }
                    })
            }}
        </section>
    }
}

#[component]
fn PanelBody(detail: RwSignal<Option<TaskDetail>>, initial: Task) -> impl IntoView {
    let ctx = use_app();
    let task = Memo::new(move |_| {
        detail
            .with(|d| d.as_ref().map(|d| d.task.clone()))
            .unwrap_or_else(|| initial.clone())
    });
    let active = Memo::new(move |_| detail.with(|d| d.as_ref().and_then(|d| d.attempt.clone())));
    let closed = Memo::new(move |_| {
        detail.with(|d| d.as_ref().and_then(|d| d.closed_attempts.last().cloned()))
    });
    let last_process =
        Memo::new(move |_| detail.with(|d| d.as_ref().and_then(|d| d.processes.last().cloned())));
    let attachments = Memo::new(move |_| {
        detail.with(|d| {
            d.as_ref()
                .map(|d| d.attachments.clone())
                .unwrap_or_default()
        })
    });
    let meta =
        Memo::new(move |_| active.with(|a| a.as_ref().map(attempt_meta).unwrap_or_default()));
    let agent = Memo::new(move |_| detail.with(|d| d.as_ref().and_then(panel_state)));
    let pending =
        Memo::new(move |_| active.with(|a| a.as_ref().is_some_and(|a| a.pending_approvals > 0)));
    let active_id = Memo::new(move |_| active.with(|a| a.as_ref().map(|a| a.id.clone())));
    let active_ref =
        Memo::new(move |_| active.with(|a| a.as_ref().map(|a| (a.id.clone(), a.branch.clone()))));
    // The transcript of the active attempt, else of the last closed one (read only).
    let shown_id = Memo::new(move |_| {
        active_id
            .get()
            .or_else(|| closed.with(|c| c.as_ref().map(|c| c.id.clone())))
    });
    let running = Signal::derive(move || active.with(|a| a.as_ref().is_some_and(|a| a.running)));
    let interrupted = Memo::new(move |_| {
        active.with(|a| a.as_ref().is_some_and(|a| !a.running))
            && last_process.with(|p| {
                p.as_ref()
                    .is_some_and(|p| interrupted_by_app(p.stop_reason))
            })
    });
    let tab = RwSignal::new(TAB_AGENT);
    // One primary action per state: Avvia (no attempt), Continua (interrupted), else Merge in
    // review while the agent is idle; it opens Modifiche, where the merge is (and hides there).
    let can_merge = Memo::new(move |_| {
        task.with(|t| t.status == TaskStatus::InReview)
            && active.with(Option::is_some)
            && !running.get()
            && !interrupted.get()
            && tab.get() != TAB_CHANGES
    });
    let start_for = RwSignal::new(None::<Id>);
    let discard_open = RwSignal::new(false);
    let task_id = move || task.with_untracked(|t| t.id.clone());
    let open_start = move |_| start_for.set(Some(task_id()));

    let agent_view = move || match shown_id.get() {
        Some(attempt_id) => view! { <Transcript attempt_id /> }.into_any(),
        None => view! {
            <div class="flex flex-1 items-center justify-center p-6">
                <Empty>
                    <EmptyHeader>
                        <EmptyTitle>"Nessun tentativo"</EmptyTitle>
                        <EmptyDescription>
                            "Avvia l'agente: lavorerà in un worktree e in un branch dedicati."
                        </EmptyDescription>
                    </EmptyHeader>
                    <EmptyContent>
                        <Button on:click=open_start>
                            <Play />
                            "Avvia"
                        </Button>
                    </EmptyContent>
                </Empty>
            </div>
        }
        .into_any(),
    };
    // Mounted only while the tab is visible: the diff is fetched on open (spec §9.2).
    let changes = move || {
        (tab.get() == TAB_CHANGES).then(|| match active_id.get() {
            Some(attempt_id) => {
                let id = attempt_id.clone();
                // Not memoized: every refetch of the detail notifies it.
                let active = Signal::derive(move || {
                    detail.with(|d| {
                        d.as_ref()
                            .and_then(|d| d.attempt.as_ref())
                            .is_some_and(|a| a.id == id)
                    })
                });
                view! { <DiffView attempt_id task active /> }.into_any()
            }
            None => match closed.get() {
                Some(attempt) => view! { <ClosedAttempt attempt /> }.into_any(),
                None => view! {
                    <div class="p-6">
                        <Empty>
                            <EmptyHeader>
                                <EmptyTitle>"Nessuna modifica"</EmptyTitle>
                                <EmptyDescription>
                                    "Le modifiche compaiono qui dopo il primo turno dell'agente."
                                </EmptyDescription>
                            </EmptyHeader>
                        </Empty>
                    </div>
                }
                .into_any(),
            },
        })
    };

    view! {
        <header class="shrink-0 border-b px-5 pt-3.5 pb-3">
            // The close button stays top right however the actions wrap; the date gives way before
            // they do.
            <div class="flex items-start gap-2">
                <div class="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1">
                    <p class="text-muted-foreground flex min-w-0 flex-[1_1_10rem] items-center gap-2 text-[11px] whitespace-nowrap">
                        {move || view! { <StatusDot status=Status::of_column(task.with(|t| t.status)) /> }}
                        <span>{move || column_title(task.with(|t| t.status))}</span>
                        <span aria-hidden="true">"·"</span>
                        <span class="truncate" title=move || task.with(|t| updated_title(t.created_at))>
                            {move || format!("creato {}", updated_text(task.with(|t| t.created_at)))}
                        </span>
                    </p>
                    <div class="ml-auto flex flex-wrap items-center justify-end gap-1">
                        <Show when=move || active.with(Option::is_none)>
                            <Button size=ButtonSize::Sm class=SMALL attr:data-action="open-start" on:click=open_start>
                                <Play />
                                "Avvia"
                            </Button>
                        </Show>
                        <Show when=move || can_merge.get()>
                            <Button
                                size=ButtonSize::Sm
                                class=SMALL
                                attr:data-action="open-merge"
                                attr:title="Apri Modifiche per il merge"
                                on:click=move |_| show_tab(tab, TAB_CHANGES, true)
                            >
                                <GitMerge />
                                "Merge"
                            </Button>
                        </Show>
                        <Show when=move || interrupted.get()>
                            <Button
                                size=ButtonSize::Sm
                                class=SMALL
                                on:click=move |_| attempt_command(ctx, active, Command::Continue)
                            >
                                <RotateCw />
                                "Continua"
                            </Button>
                        </Show>
                        <Show when=move || running.get()>
                            <Button
                                variant=ButtonVariant::Outline
                                size=ButtonSize::Sm
                                class=SMALL
                                attr:data-action="stop"
                                on:click=move |_| attempt_command(ctx, active, Command::Stop)
                            >
                                <Square />
                                "Stop"
                            </Button>
                        </Show>
                        <Show when=move || active.with(Option::is_some)>
                            <Button
                                variant=ButtonVariant::Ghost
                                size=ButtonSize::Sm
                                class=tw_merge!(SMALL, "text-destructive hover:bg-destructive/10 hover:text-destructive")
                                attr:data-action="discard"
                                on:click=move |_| discard_open.set(true)
                            >
                                <Trash2 />
                                "Scarta"
                            </Button>
                        </Show>
                        <Show when=move || {
                            active.with(|a| a.as_ref().is_some_and(|a| a.worktree_state == WorktreeState::Present))
                        }>
                            <div class="flex items-center">
                                {[
                                    (OpenTarget::Finder, "Apri in Finder"),
                                    (OpenTarget::Terminal, "Apri nel Terminale"),
                                    (OpenTarget::Editor, "Apri nell'editor"),
                                ]
                                    .into_iter()
                                    .map(|(target, label)| {
                                        let icon = match target {
                                            OpenTarget::Finder => view! { <FolderOpen /> }.into_any(),
                                            OpenTarget::Terminal => view! { <SquareTerminal /> }.into_any(),
                                            OpenTarget::Editor => view! { <Code /> }.into_any(),
                                        };
                                        view! {
                                            <Tooltip class="my-0">
                                                <Button
                                                    variant=ButtonVariant::Ghost
                                                    size=ButtonSize::IconSm
                                                    class=tw_merge!("text-muted-foreground size-7 [&_svg:not([class*='size-'])]:size-3.5", FOCUS_RING)
                                                    attr:aria-label=label
                                                    on:click=move |_| attempt_command(ctx, active, Command::Open(target))
                                                >
                                                    {icon}
                                                </Button>
                                                <TooltipContent position=TooltipPosition::Bottom class="shadow-md">
                                                    {label}
                                                </TooltipContent>
                                            </Tooltip>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        </Show>
                        <label for="task-move" class="sr-only">
                            "Sposta in"
                        </label>
                        <div class="w-28" title="Sposta in">
                            <MoveSelect task active start_for />
                        </div>
                    </div>
                </div>
                <span class="bg-border mx-1 mt-1.5 h-4 w-px shrink-0" aria-hidden="true"></span>
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::IconSm
                    class=tw_merge!("text-muted-foreground size-7 shrink-0", FOCUS_RING)
                    attr:aria-label="Chiudi"
                    on:click=move |_| ctx.open_task.set(None)
                >
                    <X />
                </Button>
            </div>
            <h2 class="mt-1.5 text-[17px] leading-6 font-semibold tracking-tight text-pretty [overflow-wrap:anywhere]">
                {move || task.with(|t| t.title.clone())}
            </h2>
            <div class="mt-2 flex flex-wrap items-center gap-2">
                {move || agent.get().map(|(label, status)| view! { <AgentBadge label status /> })}
                {move || active.get().map(|a| branch_chip(&a))}
                <Show when=move || attachments.with(|a| !a.is_empty())>
                    <Show when=move || agent.with(Option::is_some) || active.with(Option::is_some)>
                        <span class="bg-border h-3 w-px" aria-hidden="true"></span>
                    </Show>
                    <ul class="flex flex-wrap gap-1.5" aria-label="Allegati">
                        <For each=move || attachments.get() key=|a| a.id.clone() let:a>
                            <li
                                class="bg-muted inline-flex h-5 max-w-60 items-center gap-1 rounded-md px-1.5 text-[11px]"
                                title=a.path.clone()
                                data-attachment=a.name.clone()
                            >
                                <Paperclip class="text-muted-foreground size-3 shrink-0" />
                                <span class="truncate">{a.name.clone()}</span>
                                <span class="text-muted-foreground shrink-0">{format_size(a.size)}</span>
                            </li>
                        </For>
                    </ul>
                </Show>
            </div>
            <Show when=move || meta.with(|m| !m.is_empty())>
                // The sr-only colon keeps "Sub-agent: …" readable as one phrase (and its text).
                <dl class="mt-2.5 flex flex-wrap gap-x-4 gap-y-1 text-xs" data-testid="attempt-meta">
                    <For each=move || meta.get() key=|(label, value)| (*label, value.clone()) let:pair>
                        <div class="flex min-w-0 gap-1.5">
                            <dt class="text-muted-foreground shrink-0">
                                {pair.0}
                                <span class="sr-only">": "</span>
                            </dt>
                            <dd class="min-w-0 truncate font-mono">{pair.1}</dd>
                        </div>
                    </For>
                </dl>
            </Show>
        </header>
        <Tabs default_value=TAB_AGENT class="min-h-0 flex-1 gap-0">
            <TabsList
                variant=TabsVariant::Line
                class="w-full shrink-0 justify-start gap-5 border-b px-5 group-data-[orientation=Horizontal]/tabs:h-9"
                attr:role="tablist"
                attr:aria-label="Task"
                on:keydown=move |ev: KeyboardEvent| {
                    let next = match ev.key().as_str() {
                        "ArrowLeft" | "ArrowRight" if tab.get_untracked() == TAB_AGENT => TAB_CHANGES,
                        "ArrowLeft" | "ArrowRight" | "Home" => TAB_AGENT,
                        "End" => TAB_CHANGES,
                        _ => return,
                    };
                    ev.prevent_default();
                    show_tab(tab, next, true);
                }
            >
                <TabsTrigger
                    value=TAB_AGENT
                    class=LINE_TAB
                    attr:id="task-tab-agent"
                    attr:role="tab"
                    attr:aria-controls="task-tabpanel-agent"
                    attr:aria-selected=move || (tab.get() == TAB_AGENT).to_string()
                    attr:tabindex=move || if tab.get() == TAB_AGENT { "0" } else { "-1" }
                    attr:data-tab=TAB_AGENT
                    attr:aria-describedby=move || pending.get().then_some("task-tab-agent-waiting")
                    on:click=move |_| tab.set(TAB_AGENT)
                >
                    "Agente"
                    // No text: the E2E finds the tab by its exact label; the description says it.
                    <Show when=move || pending.get()>
                        <StatusDot status=Status::Waiting />
                    </Show>
                </TabsTrigger>
                <TabsTrigger
                    value=TAB_CHANGES
                    class=LINE_TAB
                    attr:id="task-tab-changes"
                    attr:role="tab"
                    attr:aria-controls="task-tabpanel-changes"
                    attr:aria-selected=move || (tab.get() == TAB_CHANGES).to_string()
                    attr:tabindex=move || if tab.get() == TAB_CHANGES { "0" } else { "-1" }
                    attr:data-tab=TAB_CHANGES
                    on:click=move |_| tab.set(TAB_CHANGES)
                >
                    "Modifiche"
                </TabsTrigger>
            </TabsList>
            <Show when=move || pending.get()>
                <span id="task-tab-agent-waiting" class="sr-only">
                    "In attesa di approvazione"
                </span>
            </Show>
            <TabsContent
                value=TAB_AGENT
                class="flex min-h-0 flex-col group-data-[orientation=Horizontal]/tabs:mt-0"
                attr:id="task-tabpanel-agent"
                attr:role="tabpanel"
                attr:aria-labelledby="task-tab-agent"
            >
                {agent_view}
                {move || active_id.get().map(|attempt_id| view! { <Composer attempt_id running /> })}
            </TabsContent>
            <TabsContent
                value=TAB_CHANGES
                class="min-h-0 overflow-y-auto group-data-[orientation=Horizontal]/tabs:mt-0"
                attr:id="task-tabpanel-changes"
                attr:role="tabpanel"
                attr:aria-labelledby="task-tab-changes"
            >
                {changes}
            </TabsContent>
        </Tabs>
        <StartDialog task_id=start_for />
        {move || {
            active_ref
                .get()
                .map(|(attempt_id, branch)| {
                    view! { <DiscardDialog open=discard_open attempt_id branch /> }
                })
        }}
    }
}

fn branch_chip(attempt: &AttemptView) -> impl IntoView + use<> {
    view! {
        <span
            class="text-muted-foreground inline-flex min-w-0 items-center gap-1 font-mono text-[11px]"
            title=format!("{} → {}", attempt.branch, attempt.target_branch)
        >
            <GitBranch class="size-3 shrink-0" />
            <span class="truncate">{attempt.branch.clone()}</span>
        </span>
    }
}

/// The agent badge of the board card (`status::agent_state`), from the detail: the active
/// attempt, else the last closed one; the latest turn only counts for the active one.
fn panel_state(d: &TaskDetail) -> Option<(&'static str, Status)> {
    let attempt = d.attempt.as_ref().or(d.closed_attempts.last())?;
    let last = d.attempt.as_ref().and(d.processes.last());
    agent_state(&TaskCard {
        task: d.task.clone(),
        attempt_id: Some(attempt.id.clone()),
        attempt_state: Some(attempt.state),
        branch: Some(attempt.branch.clone()),
        running: attempt.running,
        pending_approvals: attempt.pending_approvals,
        last_status: last.map(|p| p.status),
        last_stop_reason: last.and_then(|p| p.stop_reason),
        worktree_state: Some(attempt.worktree_state),
    })
}

/// Selects a tab of the vendored `Tabs` (its state is private) by clicking its trigger;
/// `focus` for the arrow keys. The active tab is not clicked again: its view would rebuild.
fn show_tab(tab: RwSignal<&'static str>, value: &str, focus: bool) {
    if tab.get_untracked() == value {
        return;
    }
    let trigger = document()
        .query_selector(&format!("[data-view=task-panel] [data-tab={value}]"))
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(trigger) = trigger {
        trigger.click();
        if focus {
            let _ = trigger.focus();
        }
    }
}

/// "Sposta in…": the keyboard/menu alternative to drag-and-drop (spec §9.3). "In corso"
/// without an active attempt opens the Start dialog, as a drop there does.
#[component]
fn MoveSelect(
    task: Memo<Task>,
    active: Memo<Option<AttemptView>>,
    start_for: RwSignal<Option<Id>>,
) -> impl IntoView {
    let ctx = use_app();
    let value = RwSignal::new(String::new());
    // Follows the task; also reverts the select until a move is confirmed by `changed`.
    Effect::new(move |_| value.set(task.with(|t| t.status.as_str().to_owned())));
    let on_change = Callback::new(move |ev: leptos::ev::Event| {
        let (id, current) = task.with_untracked(|t| (t.id.clone(), t.status));
        value.set(current.as_str().to_owned());
        let Ok(status) = event_target_value(&ev).parse::<TaskStatus>() else {
            return;
        };
        if status == current {
            return;
        }
        if status == TaskStatus::InProgress && active.with_untracked(Option::is_none) {
            start_for.set(Some(id));
            return;
        }
        let req = MoveTaskReq {
            id,
            status,
            before_id: None,
        };
        spawn_local(async move {
            if let Err(e) = ipc::call::<MoveTask>(&req).await {
                ctx.toasts.app_error(&e);
            }
        });
    });
    view! {
        <SelectNative
            id="task-move"
            class="h-7 rounded-md ps-2.5 pe-7 text-xs shadow-xs"
            value=value.read_only()
            on_change
        >
            {TaskStatus::ALL
                .iter()
                .map(|s| {
                    let s = *s;
                    view! {
                        <option value=s.as_str() prop:selected=move || value.with(|v| v == s.as_str())>
                            {column_title(s)}
                        </option>
                    }
                })
                .collect_view()}
        </SelectNative>
    }
}

/// Label/value pairs of the active attempt: "Modello" and "Sub-agent" (e.g. "sonnet, max 3
/// (usati 1)") only when it sets them, then its mode and where its branch starts.
fn attempt_meta(a: &AttemptView) -> Vec<(&'static str, String)> {
    let mut pairs = meta_pairs(
        a.model.as_deref(),
        a.subagent_model.as_deref(),
        a.max_subagents,
        a.subagents_used,
    );
    pairs.push(("Modalità", mode_label(a.permission_mode.as_str())));
    let commit = short_commit(&a.base_commit);
    pairs.push(("Da", format!("{} @ {commit}", a.target_branch)));
    pairs
}

fn meta_pairs(
    model: Option<&str>,
    subagent_model: Option<&str>,
    max_subagents: Option<u8>,
    used: u32,
) -> Vec<(&'static str, String)> {
    let subagents = match (max_subagents, subagent_model) {
        (Some(0), _) => Some("nessuno".to_owned()),
        (Some(max), Some(m)) => Some(format!("{m}, max {max} (usati {used})")),
        (Some(max), None) => Some(format!("max {max} (usati {used})")),
        (None, Some(m)) => Some(m.to_owned()),
        (None, None) => None,
    };
    [
        model.map(|m| ("Modello", m.to_owned())),
        subagents.map(|s| ("Sub-agent", s)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[derive(Clone, Copy)]
enum Command {
    Stop,
    /// "Continua" after the app closed mid-turn (spec §7.9).
    Continue,
    Open(OpenTarget),
}

fn attempt_command(ctx: AppCtx, active: Memo<Option<AttemptView>>, command: Command) {
    let Some(attempt_id) = active.with_untracked(|a| a.as_ref().map(|a| a.id.clone())) else {
        return;
    };
    spawn_local(async move {
        let res = match command {
            Command::Stop => ipc::call::<StopAttempt>(&AttemptIdReq { attempt_id }).await,
            Command::Continue => {
                let req = SendFollowUpReq {
                    attempt_id,
                    prompt: CONTINUE_PROMPT.into(),
                    permission_mode: None,
                    fresh_session: false,
                };
                ipc::call::<SendFollowUp>(&req).await.map(drop)
            }
            Command::Open(target) => {
                ipc::call::<OpenAttempt>(&OpenAttemptReq { attempt_id, target }).await
            }
        };
        if let Err(e) = res {
            ctx.toasts.app_error(&e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::meta_pairs;

    fn pairs(v: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        v.iter().map(|(k, v)| (*k, (*v).to_owned())).collect()
    }

    #[test]
    fn the_meta_pairs_omit_what_the_attempt_leaves_to_the_defaults() {
        assert_eq!(
            meta_pairs(Some("opus"), Some("sonnet"), Some(3), 1),
            pairs(&[
                ("Modello", "opus"),
                ("Sub-agent", "sonnet, max 3 (usati 1)")
            ])
        );
        assert_eq!(
            meta_pairs(None, None, Some(2), 0),
            pairs(&[("Sub-agent", "max 2 (usati 0)")])
        );
        assert_eq!(
            meta_pairs(Some("fable"), Some("haiku"), None, 0),
            pairs(&[("Modello", "fable"), ("Sub-agent", "haiku")])
        );
        // No sub-agents at all: their model does not matter.
        assert_eq!(
            meta_pairs(Some("opus"), Some("haiku"), Some(0), 0),
            pairs(&[("Modello", "opus"), ("Sub-agent", "nessuno")])
        );
        assert_eq!(
            meta_pairs(Some("sonnet"), None, None, 0),
            pairs(&[("Modello", "sonnet")])
        );
        assert!(meta_pairs(None, None, None, 0).is_empty());
    }
}
