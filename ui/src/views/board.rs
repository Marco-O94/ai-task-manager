//! Board of the selected project (spec §9.2, §9.3): a toolbar with the Kanban | Lista toggle
//! (spec F4), then either the five columns (Annullati collapsed, drag-and-drop, quick create
//! at the bottom of a column) or the list (`board/list.rs`), both over the same cards.
//! Refetches `get_board` when `AppCtx::project` or `AppCtx::board_version` changes.
//! Owner: M2-UI-BOARD, toolbar and list UI-TASKS.

mod list;

pub(crate) use list::{updated_text, updated_title};

use std::time::Duration;

use atm_types::{
    AttemptState, CONTINUE_PROMPT, CreateTask, CreateTaskReq, DEFAULT_AUTOPILOT_MAX_FIXES,
    GetBoard, Id, MoveTask, MoveTaskReq, ProjectIdReq, SendFollowUp, SendFollowUpReq, StopReason,
    TaskCard, TaskStatus, WorktreeState,
};
use icons::{
    Bot, ChevronLeft, ChevronRight, FolderPlus, GitBranch, List, Pencil, Plus, SquareKanban,
    TriangleAlert,
};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::state::board::{apply_move, column, drop_before, title_of};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::input::Input;
use crate::ui::scroll_area::ScrollArea;
use crate::ui::skeleton::Skeleton;
use crate::views::sidebar::add_repository;
use crate::views::start_dialog::StartDialog;
use crate::views::task_dialog::{TaskDialog, TaskDialogMode};
use crate::widgets::dnd::{DragCtx, DropPlaceholder};
use crate::widgets::status::{
    AgentBadge, FOCUS_RING, PILL_TABLIST, Status, StatusDot, agent_state, pill_tab, queued,
    verify_state,
};
use list::TaskList;

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

/// How the board shows the tasks (spec F4). Not persisted: every mount starts on Kanban.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskView {
    Kanban,
    List,
}

impl TaskView {
    /// `data-task-view` of the toggle's buttons.
    fn as_str(self) -> &'static str {
        match self {
            Self::Kanban => "kanban",
            Self::List => "list",
        }
    }
}

/// State shared by the columns, the cards and the list of the board.
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
                parent_id: None,
                auto: false,
                after_id: None,
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

/// Mounted by `Layout` only while a project is selected; without one, `Layout` renders
/// [`NoProject`] itself.
#[component]
pub fn Board() -> impl IntoView {
    let ctx = use_app();
    // A drop on "In corso" of a task without attempt opens the Start dialog instead of moving.
    let start_for = RwSignal::new(None::<Id>);
    let task_dialog = ctx.task_dialog;
    let board = BoardState {
        ctx,
        cards: ctx.cards,
        loaded: RwSignal::new(false),
        generation: StoredValue::new(0),
        drag: DragCtx::new(),
        start_for,
        task_dialog,
    };
    // The cards and the dialog live in `AppCtx` for the task panel; they go with the board.
    on_cleanup(move || {
        ctx.cards.try_set(Vec::new());
        ctx.task_dialog.try_set(None);
    });
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
    let task_view = RwSignal::new(TaskView::Kanban);
    // Behind memoized `Show`s: each view mounts fresh on a switch, never rebuilt in place, and
    // the hidden one is not in the DOM (the E2E measures the columns' cards).
    let list = Memo::new(move |_| task_view.get() == TaskView::List);

    view! {
        <section
            class="flex h-full flex-col"
            data-view="board"
            data-start-for=move || start_for.get()
        >
            <Toolbar board task_view />
            <Show when=move || !list.get()>
                <div
                    class="flex min-h-0 flex-1 gap-3 overflow-x-auto px-5 pb-5"
                    data-testid="columns"
                >
                    {TaskStatus::ALL
                        .iter()
                        .map(|&status| view! { <Column board status /> })
                        .collect_view()}
                </div>
            </Show>
            <Show when=move || list.get()>
                <TaskList board />
            </Show>
            <StartDialog task_id=start_for />
            <TaskDialog mode=task_dialog />
        </section>
    }
}

/// Kanban | Lista toggle (`[data-task-view]`, `aria-pressed`) as pill tabs, and "Nuovo task"
/// on the right. Neither button captures an id.
#[component]
fn Toolbar(board: BoardState, task_view: RwSignal<TaskView>) -> impl IntoView {
    view! {
        <div class="flex h-12 shrink-0 items-center gap-3 px-5" data-testid="board-toolbar">
            <div class=PILL_TABLIST role="group" aria-label="Vista dei task">
                {view_toggle(task_view, TaskView::Kanban, "Kanban", view! { <SquareKanban /> })}
                {view_toggle(task_view, TaskView::List, "Lista", view! { <List /> })}
            </div>
            <Button
                variant=ButtonVariant::Outline
                size=ButtonSize::Sm
                class="bg-card dark:bg-card dark:border-border dark:hover:bg-accent ml-auto text-[13px]"
                attr:data-action="new-task"
                on:click=move |_| board.task_dialog.set(Some(TaskDialogMode::create(TaskStatus::Todo)))
            >
                <Plus />
                "Nuovo task"
            </Button>
        </div>
    }
}

fn view_toggle(
    current: RwSignal<TaskView>,
    value: TaskView,
    label: &'static str,
    icon: impl IntoView + 'static,
) -> impl IntoView {
    let active = move || current.get() == value;
    view! {
        <button
            type="button"
            class=move || tw_merge!(pill_tab(active()), "px-2.5 [&_svg]:size-3.5")
            data-task-view=value.as_str()
            aria-pressed=move || active().to_string()
            on:click=move |_| {
                if current.get_untracked() != value {
                    current.set(value);
                }
            }
        >
            {icon}
            {label}
        </button>
    }
}

#[component]
pub fn NoProject() -> impl IntoView {
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
    // Where the drop placeholder goes: before a card (`Some(Some(id))`) or at the end.
    let placeholder = Memo::new(move |_| {
        let (over, index) = drag.drop.get()?;
        if over != status {
            return None;
        }
        let dragged = drag.dragging.get()?;
        cards.with(|col| drop_before(col, index, &dragged))
    });
    let root = NodeRef::<html::Section>::new();
    let list = NodeRef::<html::Div>::new();
    let count = move || cards.with(Vec::len);
    let title = column_title(status);
    let colour = Status::of_column(status);

    let expanded = move || {
        view! {
            <header class="flex h-10 shrink-0 items-center gap-2 px-3">
                <StatusDot status=colour />
                <h2 class="text-[13px] font-semibold">{title}</h2>
                <span class="text-muted-foreground font-mono text-[11px] tabular-nums">{count}</span>
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::IconXs
                    class="text-muted-foreground ml-auto size-7"
                    attr:title="Nuovo task"
                    attr:aria-label=format!("Nuovo task in {title}")
                    on:click=move |_| board.task_dialog.set(Some(TaskDialogMode::create(status)))
                >
                    <Plus />
                </Button>
                {(status == TaskStatus::Cancelled)
                    .then(|| {
                        view! {
                            <Button
                                variant=ButtonVariant::Ghost
                                size=ButtonSize::IconXs
                                class="text-muted-foreground size-7"
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
                <div node_ref=list class="flex min-h-12 flex-col gap-2 px-2 pb-2" data-card-list="">
                    <Show when=move || board.loaded.get() fallback=|| view! { <ColumnSkeleton /> }>
                        <For each=move || cards.get() key=|c| c.task.id.clone() let:card>
                            <TaskCardView board card placeholder />
                        </For>
                        <DropPlaceholder show=Signal::derive(move || placeholder.get() == Some(None)) />
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
                class=format!(
                    "hover:bg-accent flex h-full w-full flex-col items-center gap-3 rounded-xl py-3 {FOCUS_RING}",
                )
                title="Espandi Annullati"
                on:click=move |_| collapsed.set(false)
            >
                <ChevronLeft class="text-muted-foreground size-4" />
                <StatusDot status=colour />
                <span class="text-[13px] font-semibold [writing-mode:vertical-rl]">{title}</span>
                <span class="text-muted-foreground font-mono text-[11px] tabular-nums">{count}</span>
            </button>
        }
    };

    view! {
        <section
            node_ref=root
            class="bg-muted/50 flex shrink-0 flex-col rounded-xl transition-shadow"
            class=("w-[264px]", move || !collapsed.get())
            class=("w-11", move || collapsed.get())
            class=("ring-1", move || placeholder.with(Option::is_some))
            class=("ring-primary/30", move || placeholder.with(Option::is_some))
            aria-label=title
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
        </section>
    }
}

#[component]
fn ColumnSkeleton() -> impl IntoView {
    view! {
        <Skeleton class="h-16 w-full rounded-lg" />
        <Skeleton class="h-12 w-full rounded-lg" />
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
    let slot_here = {
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
        Memo::new(move |_| ctx.open_task.with(|t| t.as_ref() == Some(&id)))
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
    // Closed tasks recede: Fatto muted, Annullati also struck through.
    let status = Memo::new(move |_| card.with(|c| c.task.status));
    let closed = move || matches!(status.get(), TaskStatus::Done | TaskStatus::Cancelled);
    let parent_id = Memo::new(move |_| card.with(|c| c.task.parent_id.clone()));
    let progress = Memo::new(move |_| card.with(|c| (c.subtasks_done, c.subtasks_total)));

    view! {
        <div class="flex flex-col gap-2">
            <DropPlaceholder show=slot_here />
            <div
                class=format!(
                    "group bg-card cursor-grab rounded-lg border p-3 shadow-xs transition-colors active:cursor-grabbing {FOCUS_RING}",
                )
                class=("border-border", move || !selected.get())
                class=("hover:border-foreground/20", move || !selected.get())
                class=("border-primary", move || selected.get())
                class=("ring-1", move || selected.get())
                class=("ring-primary", move || selected.get())
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
                {move || {
                    parent_id.get().map(|parent_id| view! { <ParentLine cards=board.cards parent_id /> })
                }}
                <div class="flex items-start gap-2">
                    <p
                        class="line-clamp-2 flex-1 text-[13px] leading-[18px] font-medium text-pretty break-words"
                        class=("text-muted-foreground", closed)
                        class=("line-through", move || status.get() == TaskStatus::Cancelled)
                    >
                        {move || card.with(|c| c.task.title.clone())}
                    </p>
                    <button
                        class="text-muted-foreground hover:text-foreground -mt-0.5 rounded-sm p-0.5 opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
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
                    let (done, total) = progress.get();
                    let subtasks = (total > 0).then(|| view! { <SubtaskProgress done total /> });
                    card.with(|c| {
                        let badges = card_badges(ctx, c);
                        let branch = branch_line(c);
                        (badges.is_some() || branch.is_some() || subtasks.is_some())
                            .then(|| {
                                view! {
                                    <div class="mt-2.5 flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
                                        {subtasks}
                                        {badges}
                                        {branch}
                                    </div>
                                }
                            })
                    })
                }}
            </div>
        </div>
    }
}

/// "↳ done/total" with a thin bar (not when `compact`): the sub-tasks of a parent task (card,
/// list, panel).
#[component]
pub(crate) fn SubtaskProgress(
    done: u32,
    total: u32,
    #[prop(optional)] compact: bool,
) -> impl IntoView {
    let percent = (done * 100).checked_div(total).unwrap_or(0);
    view! {
        <span
            class="text-muted-foreground inline-flex items-center gap-1.5 font-mono text-[11px] tabular-nums"
            title=format!("Sotto task fatti: {done} su {total}")
            data-subtask-progress=format!("{done}/{total}")
        >
            {format!("↳ {done}/{total}")}
            {(!compact)
                .then(|| {
                    view! {
                        <span class="bg-muted h-1 w-8 overflow-hidden rounded-full" aria-hidden="true">
                            <span class="bg-status-done block h-full rounded-full" style:width=format!("{percent}%") />
                        </span>
                    }
                })}
        </span>
    }
}

/// "↳ <parent title>" of a sub-task's card, from the board's cards (the id until they have it).
#[component]
fn ParentLine(cards: RwSignal<Vec<TaskCard>>, parent_id: Id) -> impl IntoView {
    let title = Memo::new(move |_| {
        cards
            .with(|c| title_of(c, &parent_id))
            .unwrap_or_else(|| parent_id.clone())
    });
    view! {
        <p class="text-muted-foreground mb-1 truncate font-mono text-[11px]" title=title>
            {move || format!("↳ {}", title.get())}
        </p>
    }
}

/// Badges of a card (spec §9.2): the autopilot's icon, «In coda» (with why as its tooltip),
/// the agent's state (see [`agent_state`]), "Interrotto" with "Continua" (spec §7.9), the
/// verification, and a missing worktree. `None` without any: the list view shows a dash
/// instead. `display: contents`, so they flow in the caller's row. The project's autopilot
/// and «Tentativi di correzione», and the dependency's title, are read untracked: a card shows
/// a change of them at its own next change (a known limit, spec §13.7).
fn card_badges(ctx: AppCtx, card: &TaskCard) -> Option<impl IntoView + use<>> {
    let state = agent_state(card);
    let missing = card.worktree_state == Some(WorktreeState::Missing);
    // Read untracked: the card's own changes re-render its badges.
    let project = ctx.projects.with_untracked(|ps| {
        ps.iter()
            .find(|p| p.id == card.task.project_id)
            .map(|p| (p.autopilot, p.autopilot_max_fixes))
    });
    let (autopilot, max_fixes) = project.unwrap_or((false, DEFAULT_AUTOPILOT_MAX_FIXES));
    let verify = verify_state(card, max_fixes);
    let queued = queued(card).then(|| queue_reason(ctx, card, autopilot));
    let auto = card.task.auto;
    if state.is_none() && !missing && verify.is_none() && queued.is_none() && !auto {
        return None;
    }
    let agent = state.map(|(label, status)| {
        let name = badge_name(card, status);
        let attempt_id = card.attempt_id.clone().filter(|_| name == "interrupted");
        view! {
            <span class="inline-flex items-center gap-1" data-badge=name>
                // The live region replaces the old running spinner's; a pending approval
                // replaces the running badge, so it is one too.
                <AgentBadge
                    label
                    status
                    attr:role=matches!(name, "running" | "approval").then_some("status")
                />
                {attempt_id
                    .map(|attempt_id| {
                        view! {
                            <button
                                class=format!(
                                    "text-primary rounded-sm text-[11px] font-medium underline-offset-2 hover:underline {FOCUS_RING}",
                                )
                                on:click=move |ev| {
                                    ev.stop_propagation();
                                    continue_attempt(ctx, attempt_id.clone());
                                }
                            >
                                "Continua"
                            </button>
                        }
                    })}
            </span>
        }
    });
    Some(view! {
        <div class="contents" data-testid="badges">
            {auto
                .then(|| {
                    view! {
                        <span
                            class="text-primary inline-flex items-center"
                            title="Affidato all'autopilota"
                            data-auto=""
                        >
                            <Bot class="size-3.5" />
                            <span class="sr-only">"Affidato all'autopilota"</span>
                        </span>
                    }
                })}
            {queued
                .map(|reason| {
                    view! {
                        <span class="inline-flex" title=reason data-queued="">
                            <AgentBadge label="In coda" status=Status::Todo />
                        </span>
                    }
                })}
            {agent}
            {verify
                .map(|(label, status, name)| {
                    view! {
                        <span class="inline-flex" data-verify=name>
                            <AgentBadge label status spin=name == "running" />
                        </span>
                    }
                })}
            {missing
                .then(|| {
                    view! {
                        <span
                            class=format!(
                                "inline-flex h-5 items-center gap-1 rounded-md px-1.5 text-[11px] font-medium whitespace-nowrap {}",
                                Status::Failed.tint(),
                            )
                            data-badge="missing"
                        >
                            <TriangleAlert class="size-3" />
                            "Worktree mancante"
                        </span>
                    }
                })}
        </div>
    })
}

/// Tooltip of "In coda": why the queued task has not started yet.
fn queue_reason(ctx: AppCtx, card: &TaskCard, autopilot: bool) -> String {
    let after = card.task.after_id.as_ref().and_then(|id| {
        ctx.cards.with_untracked(|cards| {
            cards
                .iter()
                .find(|c| c.task.id == *id && c.task.status != TaskStatus::Done)
                .map(|c| c.task.title.clone())
        })
    });
    match after {
        Some(title) => format!("Parte dopo «{title}»"),
        None if !autopilot => "Autopilota del progetto spento".into(),
        None => "Attende un agente libero".into(),
    }
}

/// `data-badge` of the agent's badge, which the E2E drives: `running` only while the
/// registry runs the process, `closed` for a merged or discarded attempt.
fn badge_name(card: &TaskCard, status: Status) -> &'static str {
    if card.pending_approvals > 0 {
        return "approval";
    }
    if card.running {
        return "running";
    }
    if card.attempt_state != Some(AttemptState::Active) {
        return "closed";
    }
    match status {
        Status::Todo => "interrupted",
        Status::Failed => "failed",
        Status::Cancelled => "stopped",
        Status::Review => "ready",
        // `last_status` still Running: the process is ending.
        _ => "ending",
    }
}

/// The branch of the card's attempt, if it has one.
fn branch_line(card: &TaskCard) -> Option<impl IntoView + use<>> {
    card.branch.clone().map(|branch| {
        view! {
            <span class="text-muted-foreground inline-flex max-w-full min-w-0 items-center gap-1 font-mono text-[11px]">
                <GitBranch class="size-3 shrink-0" />
                <span class="truncate">{branch}</span>
            </span>
        }
    })
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
                            class="text-muted-foreground h-7 w-full justify-start text-xs"
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
    use super::{badge_name, interrupted_by_app};
    use crate::widgets::status::agent_state;
    use atm_types::{AttemptState, ProcessStatus, StopReason, Task, TaskCard, TaskStatus};

    #[test]
    fn quit_and_crash_recovery_offer_continua_user_stop_does_not() {
        assert!(interrupted_by_app(Some(StopReason::AppShutdown)));
        assert!(interrupted_by_app(Some(StopReason::AppRestart)));
        assert!(!interrupted_by_app(Some(StopReason::UserStop)));
        assert!(!interrupted_by_app(Some(StopReason::Crash)));
        assert!(!interrupted_by_app(None));
    }

    #[test]
    fn badge_names_follow_the_agent_state() {
        let name = |f: fn(&mut TaskCard)| {
            let mut card = TaskCard {
                task: Task {
                    id: "t".into(),
                    project_id: "p".into(),
                    title: "Titolo".into(),
                    description: String::new(),
                    status: TaskStatus::InProgress,
                    position: 1.0,
                    created_at: 0,
                    updated_at: 0,
                    parent_id: None,
                    auto: false,
                    after_id: None,
                },
                attempt_id: Some("a".into()),
                attempt_state: Some(AttemptState::Active),
                branch: None,
                running: false,
                pending_approvals: 0,
                last_status: Some(ProcessStatus::Running),
                last_stop_reason: None,
                worktree_state: None,
                subtasks_done: 0,
                subtasks_total: 0,
                verifying: false,
                verify_state: None,
                verify_fixes: 0,
            };
            f(&mut card);
            let (_, status) = agent_state(&card)?;
            Some(badge_name(&card, status))
        };
        assert_eq!(name(|c| c.running = true), Some("running"));
        assert_eq!(name(|c| c.pending_approvals = 1), Some("approval"));
        assert_eq!(name(|_| {}), Some("ending"));
        let failed = name(|c| c.last_status = Some(ProcessStatus::Failed));
        assert_eq!(failed, Some("failed"));
        let stopped = name(|c| c.last_status = Some(ProcessStatus::Killed));
        assert_eq!(stopped, Some("stopped"));
        let interrupted = name(|c| {
            c.last_status = Some(ProcessStatus::Killed);
            c.last_stop_reason = Some(StopReason::AppShutdown);
        });
        assert_eq!(interrupted, Some("interrupted"));
        let ready = name(|c| c.last_status = Some(ProcessStatus::Completed));
        assert_eq!(ready, Some("ready"));
        let merged = name(|c| c.attempt_state = Some(AttemptState::Merged));
        assert_eq!(merged, Some("closed"));
        let discarded = name(|c| c.attempt_state = Some(AttemptState::Discarded));
        assert_eq!(discarded, Some("closed"));
        assert_eq!(name(|c| c.attempt_state = None), None);
    }
}
