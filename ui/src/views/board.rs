//! Kanban board of the selected project (spec §9.2, §9.3): five columns (Annullati
//! collapsed), drag-and-drop, quick create at the bottom of a column. Refetches `get_board`
//! when `AppCtx::project` or `AppCtx::board_version` changes. Owner: M2-UI-BOARD.

use std::time::Duration;

use atm_types::{
    AttemptState, CONTINUE_PROMPT, CreateTask, CreateTaskReq, GetBoard, Id, MoveTask, MoveTaskReq,
    ProcessStatus, ProjectIdReq, SendFollowUp, SendFollowUpReq, StopReason, TaskCard, TaskStatus,
    WorktreeState,
};
use icons::{ChevronLeft, ChevronRight, FolderPlus, GitBranch, Pencil, Plus};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::state::board::{apply_move, column, drop_before};
use crate::ui::badge::{Badge, BadgeSize, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::card::{Card, CardContent, CardSize};
use crate::ui::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::input::Input;
use crate::ui::scroll_area::ScrollArea;
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;
use crate::views::sidebar::{add_repository, projects_loaded};
use crate::views::start_dialog::StartDialog;
use crate::views::task_dialog::{TaskDialog, TaskDialogMode};
use crate::widgets::dnd::DragCtx;

/// Column heading shown on the board.
pub fn column_title(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Todo => "Da fare",
        TaskStatus::InProgress => "In corso",
        TaskStatus::InReview => "In revisione",
        TaskStatus::Done => "Fatto",
        TaskStatus::Cancelled => "Annullati",
    }
}

/// State shared by the columns and cards of the board.
#[derive(Clone, Copy)]
struct BoardState {
    ctx: AppCtx,
    /// Latest `get_board`, plus the optimistic edits made since.
    cards: RwSignal<Vec<TaskCard>>,
    /// `get_board` answered for the selected project.
    loaded: RwSignal<bool>,
    /// Bumped by every local edit: an older `get_board` reply is stale and dropped.
    generation: StoredValue<u64>,
    drag: DragCtx,
    start_for: RwSignal<Option<Id>>,
    task_dialog: RwSignal<Option<TaskDialogMode>>,
}

impl BoardState {
    fn fetch(self, project_id: Id) {
        let generation = self.generation.get_value();
        spawn_local(async move {
            let res = ipc::call::<GetBoard>(&ProjectIdReq { project_id }).await;
            if self.generation.try_get_value() != Some(generation) {
                return;
            }
            match res {
                Ok(cards) => {
                    self.cards.try_set(cards);
                }
                Err(e) => self.ctx.toasts.app_error(&e),
            }
            self.loaded.try_set(true);
        });
    }

    /// The authoritative state again, e.g. after a rejected optimistic edit.
    fn refetch(self) {
        self.ctx.board_version.try_update(|v| *v += 1);
    }

    fn edit(self, f: impl FnOnce(&mut Vec<TaskCard>)) {
        self.generation.try_update_value(|g| *g += 1);
        self.cards.try_update(f);
    }

    /// Drop of `id` at insertion `index` of `status` (spec §9.3): optimistic reorder, then
    /// `move_task`; on error a toast and a refetch. A task without an active attempt dropped
    /// on "In corso" opens the Start dialog instead of moving.
    fn drop(self, id: Id, status: TaskStatus, index: usize) {
        let (col, card) = self.cards.with_untracked(|cards| {
            (
                column(cards, status),
                cards.iter().find(|c| c.task.id == id).cloned(),
            )
        });
        let Some(card) = card else {
            return;
        };
        let Some(before_id) = drop_before(&col, index, &id) else {
            return;
        };
        if status == TaskStatus::InProgress
            && card.task.status != TaskStatus::InProgress
            && card.attempt_state != Some(AttemptState::Active)
        {
            self.start_for.set(Some(id));
            return;
        }
        self.edit(|cards| apply_move(cards, &id, status, before_id.as_deref()));
        spawn_local(async move {
            let req = MoveTaskReq {
                id,
                status,
                before_id,
            };
            if let Err(e) = ipc::call::<MoveTask>(&req).await {
                self.ctx.toasts.app_error(&e);
                self.refetch();
            }
        });
    }

    fn create(self, status: TaskStatus, title: String) {
        let Some(project_id) = self.ctx.project.get_untracked() else {
            return;
        };
        spawn_local(async move {
            let req = CreateTaskReq {
                project_id,
                title,
                description: String::new(),
                status: Some(status),
            };
            match ipc::call::<CreateTask>(&req).await {
                Ok(card) => self.edit(|cards| {
                    if !cards.iter().any(|c| c.task.id == card.task.id) {
                        cards.push(card);
                    }
                }),
                Err(e) => self.ctx.toasts.app_error(&e),
            }
        });
    }
}

#[component]
pub fn Board() -> impl IntoView {
    let ctx = use_app();
    // A drop on "In corso" of a task without attempt opens the Start dialog instead of moving.
    let start_for = RwSignal::new(None::<Id>);
    let task_dialog = RwSignal::new(None::<TaskDialogMode>);
    let board = BoardState {
        ctx,
        cards: RwSignal::new(Vec::new()),
        loaded: RwSignal::new(false),
        generation: StoredValue::new(0),
        drag: DragCtx::new(),
        start_for,
        task_dialog,
    };
    let projects_ready = projects_loaded(ctx);
    let shown = StoredValue::new(None::<Id>);
    Effect::new(move |_| {
        ctx.board_version.track();
        let project = ctx.project.get();
        if project != shown.get_value() {
            shown.set_value(project.clone());
            board.loaded.set(false);
            board.edit(Vec::clear);
        }
        if let Some(project) = project {
            board.fetch(project);
        }
    });

    view! {
        <section
            class="flex h-full flex-col"
            data-view="board"
            data-start-for=move || start_for.get()
        >
            <Show
                when=move || ctx.project.with(Option::is_some)
                fallback=move || projects_ready.get().then(|| view! { <NoProject /> })
            >
                <div class="flex min-h-0 flex-1 gap-3 overflow-x-auto p-4" data-testid="columns">
                    {TaskStatus::ALL
                        .iter()
                        .map(|&status| view! { <Column board status /> })
                        .collect_view()}
                </div>
            </Show>
            <StartDialog task_id=start_for />
            <TaskDialog mode=task_dialog />
        </section>
    }
}

#[component]
fn NoProject() -> impl IntoView {
    let ctx = use_app();
    view! {
        <div class="flex flex-1 items-center justify-center p-8">
            <Empty>
                <EmptyHeader>
                    <EmptyTitle>"Nessun progetto"</EmptyTitle>
                    <EmptyDescription>"Aggiungi un repository git per iniziare."</EmptyDescription>
                </EmptyHeader>
                <EmptyContent>
                    <Button on:click=move |_| add_repository(ctx)>
                        <FolderPlus />
                        "Aggiungi repository"
                    </Button>
                </EmptyContent>
            </Empty>
        </div>
    }
}

#[component]
fn Column(board: BoardState, status: TaskStatus) -> impl IntoView {
    let drag = board.drag;
    let cards = Memo::new(move |_| board.cards.with(|c| column(c, status)));
    let collapsed = RwSignal::new(status == TaskStatus::Cancelled);
    // Where the placeholder bar goes: before a card (`Some(Some(id))`) or at the end.
    let placeholder = Memo::new(move |_| {
        let (over, index) = drag.drop.get()?;
        if over != status {
            return None;
        }
        let dragged = drag.dragging.get()?;
        cards.with(|col| drop_before(col, index, &dragged))
    });
    let root = NodeRef::<html::Div>::new();
    let list = NodeRef::<html::Div>::new();
    let count = move || cards.with(Vec::len);

    let expanded = move || {
        view! {
            <header class="flex items-center gap-2 px-3 pt-3 pb-2">
                <h2 class="text-sm font-semibold">{column_title(status)}</h2>
                <Badge variant=BadgeVariant::Muted size=BadgeSize::Sm>
                    {count}
                </Badge>
                <div class="flex-1" />
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::IconXs
                    attr:title="Nuovo task"
                    attr:aria-label=format!("Nuovo task in {}", column_title(status))
                    on:click=move |_| board.task_dialog.set(Some(TaskDialogMode::Create(status)))
                >
                    <Plus />
                </Button>
                {(status == TaskStatus::Cancelled)
                    .then(|| {
                        view! {
                            <Button
                                variant=ButtonVariant::Ghost
                                size=ButtonSize::IconXs
                                attr:title="Comprimi"
                                attr:aria-label="Comprimi Annullati"
                                on:click=move |_| collapsed.set(true)
                            >
                                <ChevronRight />
                            </Button>
                        }
                    })}
            </header>
            <ScrollArea class="min-h-0 flex-1">
                <div node_ref=list class="flex min-h-12 flex-col gap-2 px-2 pt-1 pb-2" data-card-list="">
                    <Show when=move || board.loaded.get() fallback=|| view! { <ColumnSkeleton /> }>
                        <For each=move || cards.get() key=|c| c.task.id.clone() let:card>
                            <TaskCardView board card placeholder />
                        </For>
                        <PlaceholderBar show=Signal::derive(move || placeholder.get() == Some(None)) />
                        <Show when=move || count() == 0 && placeholder.with(Option::is_none)>
                            <p class="text-muted-foreground py-6 text-center text-xs">"Nessun task"</p>
                        </Show>
                    </Show>
                </div>
            </ScrollArea>
            <QuickCreate board status />
        }
    };
    let folded = move || {
        view! {
            <button
                class="hover:bg-accent flex h-full w-full flex-col items-center gap-3 rounded-xl py-3"
                title="Espandi Annullati"
                on:click=move |_| collapsed.set(false)
            >
                <ChevronLeft class="size-4" />
                <span class="text-sm font-semibold [writing-mode:vertical-rl]">{column_title(status)}</span>
                <Badge variant=BadgeVariant::Muted size=BadgeSize::Sm>
                    {count}
                </Badge>
            </button>
        }
    };

    view! {
        <div
            node_ref=root
            class="bg-muted/60 flex shrink-0 flex-col rounded-xl border transition-colors"
            class=("w-72", move || !collapsed.get())
            class=("w-11", move || collapsed.get())
            class=("border-primary", move || placeholder.with(Option::is_some))
            data-column=status.as_str()
            data-collapsed=move || collapsed.get().to_string()
            on:dragover=move |ev| {
                match list.get_untracked().filter(|_| !collapsed.get_untracked()) {
                    Some(list) => drag.over(&ev, status, &list),
                    None => drag.over_end(&ev, status),
                }
            }
            on:dragleave=move |ev| {
                if let Some(root) = root.get_untracked() {
                    drag.leave(&ev, &root);
                }
            }
            on:drop=move |ev| {
                if let Some((id, status, index)) = drag.finish(&ev) {
                    board.drop(id, status, index);
                }
            }
        >
            {move || if collapsed.get() { folded().into_any() } else { expanded().into_any() }}
        </div>
    }
}

#[component]
fn PlaceholderBar(show: Signal<bool>) -> impl IntoView {
    view! { <div class="bg-primary h-0.5 shrink-0 rounded-full" class:invisible=move || !show.get() /> }
}

#[component]
fn ColumnSkeleton() -> impl IntoView {
    view! {
        <Skeleton class="h-16 w-full" />
        <Skeleton class="h-12 w-full" />
    }
}

#[component]
fn TaskCardView(
    board: BoardState,
    card: TaskCard,
    placeholder: Memo<Option<Option<Id>>>,
) -> impl IntoView {
    let ctx = board.ctx;
    let id = card.task.id.clone();
    // `<For>` is keyed by id: read the latest version of this card from the board.
    let card = {
        let id = id.clone();
        Memo::new(move |_| {
            board
                .cards
                .with(|cards| cards.iter().find(|c| c.task.id == id).cloned())
                .unwrap_or_else(|| card.clone())
        })
    };
    let bar_here = {
        let id = id.clone();
        Signal::derive(move || {
            placeholder.with(|p| p.as_ref().is_some_and(|b| b.as_ref() == Some(&id)))
        })
    };
    let dragging = {
        let id = id.clone();
        move || board.drag.dragging.with(|d| d.as_ref() == Some(&id))
    };
    let selected = {
        let id = id.clone();
        move || ctx.open_task.with(|t| t.as_ref() == Some(&id))
    };
    let open = {
        let id = id.clone();
        move || ctx.open_task.set(Some(id.clone()))
    };
    let edit = move || {
        board.task_dialog.set(Some(TaskDialogMode::Edit(
            card.with_untracked(|c| c.task.clone()),
        )));
    };

    view! {
        <div class="flex flex-col gap-2">
            <PlaceholderBar show=bar_here />
            <div
                class="group rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring"
                class=("ring-2", selected.clone())
                class=("ring-primary", selected)
                class=("opacity-40", dragging)
                data-task-id=id.clone()
                draggable="true"
                tabindex="0"
                role="button"
                on:dragstart={
                    let id = id.clone();
                    move |ev| board.drag.start(&ev, id.clone())
                }
                on:dragend=move |_| board.drag.end()
                on:click={
                    let open = open.clone();
                    move |_| open()
                }
                on:dblclick=move |_| edit()
                on:keydown=move |ev| {
                    if ev.key() == "Enter" && ev.target() == ev.current_target() {
                        open();
                    }
                }
            >
                <Card size=CardSize::Sm class="hover:border-foreground/20 cursor-grab gap-2 py-3 shadow-xs active:cursor-grabbing">
                    <CardContent class="flex flex-col gap-2 px-3">
                        <div class="flex items-start gap-2">
                            <p class="flex-1 text-sm leading-snug font-medium break-words">
                                {move || card.with(|c| c.task.title.clone())}
                            </p>
                            <button
                                class="text-muted-foreground hover:text-foreground -mt-0.5 rounded p-0.5 opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                                title="Modifica"
                                aria-label="Modifica il task"
                                on:click=move |ev| {
                                    ev.stop_propagation();
                                    edit();
                                }
                            >
                                <Pencil class="size-3.5" />
                            </button>
                        </div>
                        {move || {
                            let description = card.with(|c| c.task.description.clone());
                            (!description.trim().is_empty())
                                .then(|| {
                                    view! {
                                        <p class="text-muted-foreground line-clamp-2 text-xs break-words">
                                            {description}
                                        </p>
                                    }
                                })
                        }}
                        {move || card_badges(ctx, &card.get())}
                    </CardContent>
                </Card>
            </div>
        </div>
    }
}

/// Badges of a card (spec §9.2): running, pending approvals, failed, interrupted (with
/// "Continua", spec §7.9), missing worktree, closed attempt; then the branch.
fn card_badges(ctx: AppCtx, card: &TaskCard) -> impl IntoView + use<> {
    let active = card.attempt_state == Some(AttemptState::Active);
    let idle = active && !card.running;
    let interrupted = idle && interrupted_by_app(card.last_stop_reason);
    let failed = idle && !interrupted && card.last_status == Some(ProcessStatus::Failed);
    let stopped = idle && !interrupted && card.last_status == Some(ProcessStatus::Killed);
    let missing = card.worktree_state == Some(WorktreeState::Missing);
    let closed = match card.attempt_state {
        Some(AttemptState::Merged) => Some((BadgeVariant::Success, "Mergiato")),
        Some(AttemptState::Discarded) => Some((BadgeVariant::Muted, "Scartato")),
        _ => None,
    };
    let approvals = card.pending_approvals;
    let attempt_id = card.attempt_id.clone().filter(|_| interrupted);
    let any = card.running || approvals > 0 || failed || stopped || interrupted || missing;
    let any = any || closed.is_some();

    let badges = any.then(|| {
        view! {
            <div class="flex flex-wrap items-center gap-1.5" data-testid="badges">
                {card
                    .running
                    .then(|| {
                        view! {
                            <Badge variant=BadgeVariant::Info class="gap-1" attr:data-badge="running">
                                <Spinner class="size-3" />
                                "In esecuzione"
                            </Badge>
                        }
                    })}
                {(approvals > 0)
                    .then(|| {
                        let text = if approvals == 1 {
                            "Richiede approvazione".to_owned()
                        } else {
                            format!("Richiede approvazione ({approvals})")
                        };
                        view! {
                            <Badge variant=BadgeVariant::Warning attr:data-badge="approval">
                                {text}
                            </Badge>
                        }
                    })}
                {failed
                    .then(|| {
                        view! {
                            <Badge variant=BadgeVariant::Destructive attr:data-badge="failed">
                                "Fallito"
                            </Badge>
                        }
                    })}
                {stopped
                    .then(|| {
                        view! {
                            <Badge variant=BadgeVariant::Muted attr:data-badge="stopped">
                                "Fermato"
                            </Badge>
                        }
                    })}
                {attempt_id
                    .map(|attempt_id| {
                        view! {
                            <Badge variant=BadgeVariant::Warning class="gap-1 pr-0.5" attr:data-badge="interrupted">
                                "Interrotto –"
                                <button
                                    class="rounded px-1 underline-offset-2 hover:underline"
                                    on:click=move |ev| {
                                        ev.stop_propagation();
                                        continue_attempt(ctx, attempt_id.clone());
                                    }
                                >
                                    "Continua"
                                </button>
                            </Badge>
                        }
                    })}
                {missing
                    .then(|| {
                        view! {
                            <Badge variant=BadgeVariant::Warning attr:data-badge="missing">
                                "Worktree mancante"
                            </Badge>
                        }
                    })}
                {closed
                    .map(|(variant, text)| {
                        view! {
                            <Badge variant=variant attr:data-badge="closed">
                                {text}
                            </Badge>
                        }
                    })}
            </div>
        }
    });
    let branch = card.branch.clone().map(|branch| {
        view! {
            <p class="text-muted-foreground flex min-w-0 items-center gap-1 font-mono text-[11px]">
                <GitBranch class="size-3 shrink-0" />
                <span class="truncate">{branch}</span>
            </p>
        }
    });
    (badges, branch)
}

/// A turn cut by the app's lifecycle rather than by the user or the agent: stopped by the
/// quit (`app_shutdown`: Cmd+Q finalizes the running turns) or found running by the recovery
/// after a crash (`app_restart`). Both offer "Interrotto – Continua" (spec §7.9, §12.2 step 8).
pub fn interrupted_by_app(reason: Option<StopReason>) -> bool {
    matches!(
        reason,
        Some(StopReason::AppShutdown | StopReason::AppRestart)
    )
}

/// "Continua" on a turn interrupted by the app closing: follow-up with `CONTINUE_PROMPT`,
/// which resumes the session (spec §7.9).
fn continue_attempt(ctx: AppCtx, attempt_id: Id) {
    spawn_local(async move {
        let req = SendFollowUpReq {
            attempt_id,
            prompt: CONTINUE_PROMPT.into(),
            permission_mode: None,
            fresh_session: false,
        };
        if let Err(e) = ipc::call::<SendFollowUp>(&req).await {
            ctx.toasts.app_error(&e);
        }
    });
}

/// "Aggiungi task" at the bottom of a column: Enter creates, Esc or an empty blur cancels.
#[component]
fn QuickCreate(board: BoardState, status: TaskStatus) -> impl IntoView {
    let editing = RwSignal::new(false);
    let title = RwSignal::new(String::new());
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            let _ = el.focus();
        }
    });
    let close = move || {
        title.set(String::new());
        editing.set(false);
    };
    view! {
        <div class="px-2 pb-2">
            <Show
                when=move || editing.get()
                fallback=move || {
                    view! {
                        <Button
                            variant=ButtonVariant::Ghost
                            size=ButtonSize::Sm
                            class="text-muted-foreground w-full justify-start"
                            attr:data-testid="quick-create"
                            on:click=move |_| editing.set(true)
                        >
                            <Plus />
                            "Aggiungi task"
                        </Button>
                    }
                }
            >
                <form
                    on:submit=move |ev| {
                        ev.prevent_default();
                        let text = title.get_untracked().trim().to_owned();
                        if !text.is_empty() {
                            board.create(status, text);
                        }
                        title.set(String::new());
                    }
                    on:keydown=move |ev| {
                        if ev.key() == "Escape" {
                            close();
                        }
                    }
                    on:focusout=move |_| {
                        // Deferred: focus moves inside the form first (e.g. on submit).
                        set_timeout(
                            move || {
                                if title.try_with_untracked(|t| t.trim().is_empty()) == Some(true) {
                                    editing.try_set(false);
                                }
                            },
                            Duration::from_millis(100),
                        );
                    }
                >
                    <Input
                        bind_value=title
                        node_ref=input
                        placeholder="Titolo, poi Invio"
                        autocomplete="off"
                        class="bg-background h-8"
                    />
                </form>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::interrupted_by_app;
    use atm_types::StopReason;

    #[test]
    fn quit_and_crash_recovery_offer_continua_user_stop_does_not() {
        assert!(interrupted_by_app(Some(StopReason::AppShutdown)));
        assert!(interrupted_by_app(Some(StopReason::AppRestart)));
        assert!(!interrupted_by_app(Some(StopReason::UserStop)));
        assert!(!interrupted_by_app(Some(StopReason::Crash)));
        assert!(!interrupted_by_app(None));
    }
}
