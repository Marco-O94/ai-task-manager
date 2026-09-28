//! Task panel (right split, 55 %): header (Avvia, Stop, Scarta, Apri in…, "Sposta in…") and
//! the Agente / Modifiche tabs (spec §9.2). Refetches `get_task_detail` when
//! `AppCtx::detail_version` changes. Owner: M2-UI-TASK.

use atm_types::{
    AppError, AttemptIdReq, AttemptView, CONTINUE_PROMPT, GetTaskDetail, Id, IdReq, MoveTask,
    MoveTaskReq, OpenAttempt, OpenAttemptReq, OpenTarget, ProcessStatus, SendFollowUp,
    SendFollowUpReq, StopAttempt, StopReason, Task, TaskDetail, TaskStatus, WorktreeState,
};
use icons::{Code, FolderOpen, GitBranch, Play, RotateCw, Square, SquareTerminal, Trash2, X};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::alert::{Alert, AlertDescription, AlertTitle};
use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::select_native::SelectNative;
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;
use crate::ui::tabs::{Tabs, TabsContent, TabsList, TabsTrigger, TabsVariant};
use crate::ui::tooltip::{Tooltip, TooltipContent, TooltipPosition};
use crate::views::composer::Composer;
use crate::views::diff::{ClosedAttempt, DiffView};
use crate::views::merge_dialog::DiscardDialog;
use crate::views::start_dialog::StartDialog;
use crate::views::transcript::{ON_DESTRUCTIVE, Transcript};

const TAB_AGENT: &str = "agent";
const TAB_CHANGES: &str = "changes";

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
                            <header class="flex items-center gap-2 border-b p-3">
                                <Skeleton class="h-6 flex-1" />
                                <Button
                                    variant=ButtonVariant::Ghost
                                    size=ButtonSize::IconSm
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
                    .is_some_and(|p| p.stop_reason == Some(StopReason::AppRestart))
            })
    });
    let tab = RwSignal::new(TAB_AGENT);
    let start_for = RwSignal::new(None::<Id>);
    let discard_open = RwSignal::new(false);
    let task_id = move || task.with_untracked(|t| t.id.clone());
    let open_start = move |_| start_for.set(Some(task_id()));

    let agent = move || match shown_id.get() {
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
            Some(attempt_id) => view! { <DiffView attempt_id task /> }.into_any(),
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
        <header class="flex flex-col gap-2 border-b p-3">
            <div class="flex items-start gap-2">
                <div class="min-w-0 flex-1">
                    <h2 class="truncate text-base font-semibold">{move || task.with(|t| t.title.clone())}</h2>
                    <div class="mt-1 flex flex-wrap items-center gap-1.5">
                        <Badge variant=BadgeVariant::Outline>
                            {move || status_label(task.with(|t| t.status))}
                        </Badge>
                        {move || active.get().map(|a| branch_chip(&a))}
                        <Show when=move || running.get()>
                            <Badge variant=BadgeVariant::Info class="gap-1">
                                <Spinner class="size-3" />
                                "In esecuzione"
                            </Badge>
                        </Show>
                        {move || {
                            let n = active.with(|a| a.as_ref().map_or(0, |a| a.pending_approvals));
                            (n > 0)
                                .then(|| {
                                    view! {
                                        <Badge variant=BadgeVariant::Warning>
                                            {format!("Richiede approvazione ({n})")}
                                        </Badge>
                                    }
                                })
                        }}
                        <Show when=move || {
                            !running.get()
                                && last_process
                                    .with(|p| p.as_ref().is_some_and(|p| p.status == ProcessStatus::Failed))
                        }>
                            <Badge variant=BadgeVariant::Destructive class=ON_DESTRUCTIVE>
                                "Fallito"
                            </Badge>
                        </Show>
                        <Show when=move || interrupted.get()>
                            <Badge variant=BadgeVariant::Warning>"Interrotto"</Badge>
                        </Show>
                    </div>
                </div>
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::IconSm
                    attr:aria-label="Chiudi"
                    on:click=move |_| ctx.open_task.set(None)
                >
                    <X />
                </Button>
            </div>
            <div class="flex flex-wrap items-center gap-2">
                <Show when=move || active.with(Option::is_none)>
                    <Button size=ButtonSize::Sm attr:data-action="open-start" on:click=open_start>
                        <Play />
                        "Avvia"
                    </Button>
                </Show>
                <Show when=move || running.get()>
                    <Button
                        variant=ButtonVariant::Outline
                        size=ButtonSize::Sm
                        attr:data-action="stop"
                        on:click=move |_| attempt_command(ctx, active, Command::Stop)
                    >
                        <Square />
                        "Stop"
                    </Button>
                </Show>
                <Show when=move || interrupted.get()>
                    <Button
                        size=ButtonSize::Sm
                        on:click=move |_| attempt_command(ctx, active, Command::Continue)
                    >
                        <RotateCw />
                        "Continua"
                    </Button>
                </Show>
                <Show when=move || active.with(Option::is_some)>
                    <Button
                        variant=ButtonVariant::Outline
                        size=ButtonSize::Sm
                        class="text-destructive"
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
                                            attr:aria-label=label
                                            on:click=move |_| attempt_command(ctx, active, Command::Open(target))
                                        >
                                            {icon}
                                        </Button>
                                        <TooltipContent position=TooltipPosition::Bottom>{label}</TooltipContent>
                                    </Tooltip>
                                }
                            })
                            .collect_view()}
                    </div>
                </Show>
                <div class="ml-auto flex items-center gap-2">
                    <label for="task-move" class="text-muted-foreground text-xs">
                        "Sposta in"
                    </label>
                    <div class="w-36">
                        <MoveSelect task active start_for />
                    </div>
                </div>
            </div>
        </header>
        <Tabs default_value=TAB_AGENT class="min-h-0 flex-1 gap-0">
            <TabsList variant=TabsVariant::Line class="w-full justify-start gap-2 border-b px-3">
                <TabsTrigger value=TAB_AGENT class="flex-none px-2" on:click=move |_| tab.set(TAB_AGENT)>
                    "Agente"
                </TabsTrigger>
                <TabsTrigger
                    value=TAB_CHANGES
                    class="flex-none px-2"
                    on:click=move |_| tab.set(TAB_CHANGES)
                >
                    "Modifiche"
                </TabsTrigger>
            </TabsList>
            <TabsContent
                value=TAB_AGENT
                class="flex min-h-0 flex-col group-data-[orientation=Horizontal]/tabs:mt-0"
            >
                {agent}
                {move || active_id.get().map(|attempt_id| view! { <Composer attempt_id running /> })}
            </TabsContent>
            <TabsContent
                value=TAB_CHANGES
                class="min-h-0 overflow-y-auto group-data-[orientation=Horizontal]/tabs:mt-0"
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
        <span class="text-muted-foreground inline-flex min-w-0 items-center gap-1 font-mono text-xs">
            <GitBranch class="size-3 shrink-0" />
            <span class="truncate">{attempt.branch.clone()}</span>
            <span class="shrink-0">{format!("→ {}", attempt.target_branch)}</span>
        </span>
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
        <SelectNative id="task-move" value=value.read_only() on_change>
            {TaskStatus::ALL
                .iter()
                .map(|s| {
                    let s = *s;
                    view! {
                        <option value=s.as_str() prop:selected=move || value.with(|v| v == s.as_str())>
                            {status_label(s)}
                        </option>
                    }
                })
                .collect_view()}
        </SelectNative>
    }
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Todo => "Da fare",
        TaskStatus::InProgress => "In corso",
        TaskStatus::InReview => "In revisione",
        TaskStatus::Done => "Completati",
        TaskStatus::Cancelled => "Annullati",
    }
}

#[derive(Clone, Copy)]
enum Command {
    Stop,
    /// "Continua" after an app restart (spec §7.9).
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
