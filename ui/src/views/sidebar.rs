//! Sidebar (projects with their menu, "Aggiungi repository", running meter, app settings, the
//! removal confirmation) and topbar (project name, pending approvals and page tabs, account
//! chip, pause banner with "Riprendi"), spec §9.2. Owner: M2-UI-BOARD, UI-SHELL.

use atm_types::{
    AddProject, AddProjectReq, AuthState, Empty, EnvStatus, GetBoard, GetSettings, Id, IdReq,
    PickRepoFolder, ProjectIdReq, RemoveProject, ResumeAgents, TaskCard,
};
use icons::{Ellipsis, Folder, Plus, Settings2, Trash2, TriangleAlert, Zap};
use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, KeyboardEvent, MouseEvent};

use crate::app::{AppCtx, ProjectView, use_app};
use crate::ipc;
use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader,
    DialogTitle,
};
use crate::ui::kbd::Kbd;
use crate::ui::scroll_area::ScrollArea;
use crate::ui::spinner::Spinner;
use crate::ui::tooltip::{Tooltip, TooltipContent, TooltipPosition};
use crate::views::settings::SettingsDialog;
use crate::widgets::context_menu::{ContextMenu, ContextMenuItem, ContextMenuSeparator, MenuState};
use crate::widgets::status::{FOCUS_RING, PILL_TABLIST, Status, StatusDot, pill_tab};

/// Left column, 240 px.
#[component]
pub fn Sidebar() -> impl IntoView {
    let ctx = use_app();
    let settings_open = RwSignal::new(false);
    let loaded = projects_loaded(ctx);
    let menu = ProjectMenu {
        state: MenuState::new(),
        target: RwSignal::new(None),
    };
    // One menu for every project: the items read the target when chosen, never capture it.
    let chosen = move || menu.target.get_untracked();
    let menu_label = Signal::derive(move || {
        menu.target
            .with(|t| t.as_ref().map(|(_, name)| actions_label(name)))
            .unwrap_or_default()
    });
    // ⌘, opens "Impostazioni app", as in any Mac app; not over another dialog.
    let shortcut = window_event_listener(ev::keydown, move |e| {
        if e.meta_key() && e.key() == "," && !dialog_open() {
            e.prevent_default();
            settings_open.set(true);
        }
    });
    on_cleanup(move || shortcut.remove());
    let row = format!(
        "hover:bg-accent hover:text-accent-foreground flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] font-medium transition-colors [&_svg]:size-3.5 [&_svg]:shrink-0 {FOCUS_RING}"
    );
    let add_class = format!("text-muted-foreground mt-1 {row}");
    view! {
        <nav
            class="bg-sidebar text-sidebar-foreground border-sidebar-border flex w-60 shrink-0 flex-col border-r"
            data-view="sidebar"
        >
            <div class="flex h-13 shrink-0 items-center gap-2.5 px-4">
                <span class="bg-primary text-primary-foreground grid size-6 place-items-center rounded-md">
                    <Zap class="size-3.5" />
                </span>
                <span class="text-[13px] font-semibold tracking-tight">"AI Task Manager"</span>
            </div>
            <ScrollArea class="min-h-0 flex-1">
                <div class="px-2 pt-2 pb-3">
                    <p class="text-muted-foreground px-2 pb-1.5 text-[11px] font-medium tracking-wider uppercase">
                        "Progetti"
                    </p>
                    <ul class="flex flex-col gap-0.5" data-testid="projects">
                        <For
                            each=move || ctx.projects.get()
                            key=|p| (p.id.clone(), p.name.clone())
                            let:project
                        >
                            <ProjectItem
                                id=project.id
                                name=project.name
                                repo_path=project.repo_path
                                menu
                            />
                        </For>
                    </ul>
                    <Show when=move || loaded.get() && ctx.projects.with(Vec::is_empty)>
                        <p class="text-muted-foreground px-2 py-1.5 text-xs">"Nessun progetto."</p>
                    </Show>
                    <button
                        type="button"
                        class=add_class
                        on:click=move |_| add_repository(ctx)
                    >
                        <Plus />
                        "Aggiungi repository"
                    </button>
                </div>
            </ScrollArea>
            <div class="border-sidebar-border flex flex-col gap-2 border-t p-3">
                {move || {
                    ctx.env
                        .with(|env| env.as_ref().map(|env| (env.running, env.max_running)))
                        .map(|(running, max)| view! { <RunningMeter running max /> })
                }}
                <button type="button" class=row on:click=move |_| settings_open.set(true)>
                    <Settings2 class="text-muted-foreground" />
                    "Impostazioni app"
                    <Kbd class="border-border ml-auto border font-mono text-[10px]">"⌘,"</Kbd>
                </button>
                {move || {
                    ctx.env
                        .with(|env| env.as_ref().and_then(cli_version))
                        .map(|(version, untested)| {
                            view! {
                                <p
                                    class="text-muted-foreground px-2 text-[11px] leading-4"
                                    title=untested.then_some("Più recente della versione testata")
                                >
                                    {format!("Claude Code {version}")}
                                    {untested.then_some(" · non testata")}
                                </p>
                            }
                        })
                }}
            </div>
            <SettingsDialog open=settings_open />
            <ContextMenu state=menu.state label=menu_label>
                <ContextMenuItem
                    attr:data-action="menu-settings"
                    on_select=move |()| {
                        if let Some((id, _)) = chosen() {
                            ctx.select_project(id, ProjectView::Settings);
                        }
                    }
                >
                    <Settings2 />
                    "Impostazioni progetto"
                </ContextMenuItem>
                <ContextMenuSeparator />
                <ContextMenuItem
                    attr:data-action="menu-remove"
                    destructive=true
                    on_select=move |()| {
                        // The menu has already put the focus back on its trigger, which the
                        // dialog then restores when it closes.
                        if let Some(target) = chosen() {
                            ctx.remove_target.set(Some(target));
                        }
                    }
                >
                    <Trash2 />
                    "Rimuovi dalla lista…"
                </ContextMenuItem>
            </ContextMenu>
            <RemoveProjectDialog />
        </nav>
    }
}

/// A dialog of the app is open.
fn dialog_open() -> bool {
    document()
        .query_selector("[role=dialog][data-state=open]")
        .ok()
        .flatten()
        .is_some()
}

/// "In esecuzione x/y" of the whole app (`EnvStatus.running`/`max_running`): one segment per
/// parallel slot.
#[component]
fn RunningMeter(running: u32, max: u32) -> impl IntoView {
    view! {
        <div class="border-border bg-card/60 rounded-lg border p-2.5" data-testid="running">
            <div class="flex items-center justify-between text-xs">
                <span class="text-muted-foreground">"In esecuzione"</span>
                <span class="font-mono font-semibold tabular-nums">{format!("{running}/{max}")}</span>
            </div>
            <div
                class="mt-2 flex gap-1"
                role="meter"
                aria-label="Agenti in esecuzione"
                aria-valuemin="0"
                aria-valuemax=max.to_string()
                aria-valuenow=running.min(max).to_string()
            >
                {(0..max)
                    .map(|i| {
                        let fill = if i < running { Status::Running.bg() } else { "bg-muted" };
                        view! { <span class=format!("h-1 flex-1 rounded-full {fill}") /> }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

/// The CLI version, and whether it is newer than the tested one.
fn cli_version(env: &EnvStatus) -> Option<(String, bool)> {
    let version = env.claude.version.clone()?;
    let untested = version_newer(&version, &env.claude.tested_version);
    Some((version, untested))
}

/// The sidebar's project menu: its state and the project it acts on, `(id, name)`.
#[derive(Clone, Copy)]
struct ProjectMenu {
    state: MenuState,
    target: RwSignal<Option<(Id, String)>>,
}

impl ProjectMenu {
    /// Sets the project the menu acts on, before it opens (or moves) there.
    fn aim(self, id: &str, name: &str) {
        self.target.set(Some((id.to_owned(), name.to_owned())));
    }

    /// Open on project `id` (tracked).
    fn open_on(self, id: &str) -> bool {
        self.state.is_open()
            && self
                .target
                .with(|t| t.as_ref().is_some_and(|(t, _)| t == id))
    }
}

fn actions_label(name: &str) -> String {
    format!("Azioni per «{name}»")
}

/// A project of the sidebar: selects it on its overview; its menu (right click or "⋯") leads
/// to its settings and to its removal. Items are keyed by `(id, name)`, so capturing them is
/// safe: a change makes a new item.
#[component]
fn ProjectItem(id: String, name: String, repo_path: String, menu: ProjectMenu) -> impl IntoView {
    let ctx = use_app();
    let selected = {
        let id = id.clone();
        Memo::new(move |_| ctx.project.with(|p| p.as_deref() == Some(id.as_str())))
    };
    let item = NodeRef::<html::Li>::new();
    let more = NodeRef::<html::Button>::new();
    let (id, name) = (StoredValue::new(id), StoredValue::new(name));
    let open_here = move || id.with_value(|id| menu.open_on(id));
    let class = move || {
        let tone = if selected.get() {
            "bg-accent text-accent-foreground font-medium"
        } else {
            "text-sidebar-foreground hover:bg-accent/60 [&>svg]:text-muted-foreground"
        };
        // The item whose menu is open keeps a ring while the focus is in the menu.
        let ring = if open_here() { "ring-ring ring-1" } else { "" };
        format!(
            "flex h-8 w-full items-center gap-2 rounded-md px-2 pr-8 text-left text-[13px] transition-colors [&>svg]:size-3.5 [&>svg]:shrink-0 {FOCUS_RING} {tone} {ring}"
        )
    };
    let on_context = move |ev: MouseEvent| {
        ev.prevent_default();
        // The focus comes back to the project's button.
        let trigger = item
            .get_untracked()
            .and_then(|li| li.query_selector("[data-project]").ok().flatten())
            .and_then(|el| el.dyn_into::<HtmlElement>().ok());
        menu.aim(&id.get_value(), &name.get_value());
        let (x, y) = (f64::from(ev.client_x()), f64::from(ev.client_y()));
        menu.state.open_at(x, y, trigger);
    };
    let on_more = move |_| {
        let Some(button) = more.get_untracked() else {
            return;
        };
        let button: HtmlElement = button.into();
        if menu.state.opened_by(&button) {
            menu.state.close(true);
        } else {
            menu.aim(&id.get_value(), &name.get_value());
            menu.state.open_below(button);
        }
    };
    view! {
        <li class="group relative" node_ref=item on:contextmenu=on_context>
            <button
                type="button"
                class=class
                title=repo_path
                data-project=name.get_value()
                aria-current=move || selected.get().then_some("true")
                on:click=move |_| {
                    if !selected.get_untracked() {
                        ctx.select_project(id.get_value(), ProjectView::Overview);
                    }
                }
            >
                <Folder />
                <span class="truncate">{name.get_value()}</span>
            </button>
            <button
                type="button"
                node_ref=more
                class=format!(
                    "text-muted-foreground hover:bg-accent hover:text-foreground absolute top-1/2 right-1 flex size-6 -translate-y-1/2 items-center justify-center rounded-md opacity-0 group-hover:opacity-100 focus-visible:opacity-100 aria-expanded:opacity-100 [&_svg]:size-3.5 {FOCUS_RING}",
                )
                data-action="project-menu"
                aria-haspopup="menu"
                aria-expanded=move || open_here().to_string()
                aria-label=actions_label(&name.get_value())
                on:click=on_more
            >
                <Ellipsis />
            </button>
        </li>
    }
}

/// Confirmation of `remove_project` for [`AppCtx::remove_target`]: one instance, opened by the
/// sidebar menu and by the project settings page. It removes that target, never the selected
/// project as such; `Busy` (agents still running) keeps it open with the reason.
#[component]
fn RemoveProjectDialog() -> impl IntoView {
    let ctx = use_app();
    let open = RwSignal::new(false);
    // The target on screen, kept while the dialog animates out.
    let shown = RwSignal::new(None::<(Id, String)>);
    let error = RwSignal::new(None::<String>);
    // The projects whose removal is in flight: the dialog is busy only for its own target.
    let removing = RwSignal::new(Vec::<Id>::new());
    Effect::new(move |_| {
        if let Some(target) = ctx.remove_target.get() {
            shown.set(Some(target));
            error.set(None);
            open.set(true);
        }
    });
    Effect::new(move |_| {
        if !open.get() && ctx.remove_target.with_untracked(Option::is_some) {
            ctx.remove_target.set(None);
        }
    });
    // The dialog still shows project `id`.
    let on_screen = move |id: &Id| {
        open.try_get_untracked() == Some(true)
            && shown
                .try_with_untracked(|s| s.as_ref().is_some_and(|(s, _)| s == id))
                .unwrap_or(false)
    };
    let busy = move || {
        shown.with(|s| {
            s.as_ref()
                .is_some_and(|(id, _)| removing.with(|r| r.contains(id)))
        })
    };
    let confirm = move |_| {
        let Some((id, name)) = shown.get_untracked() else {
            return;
        };
        if removing.with_untracked(|r| r.contains(&id)) {
            return;
        }
        removing.update(|r| r.push(id.clone()));
        error.set(None);
        spawn_local(async move {
            match ipc::call::<RemoveProject>(&IdReq { id: id.clone() }).await {
                Ok(()) => {
                    if ctx.project.get_untracked().as_ref() == Some(&id) {
                        ctx.open_task.try_set(None);
                    }
                    ctx.toasts
                        .success(format!("«{name}» rimosso dalla lista; i branch restano"));
                    // Not a dialog reopened meanwhile for another project.
                    if on_screen(&id) {
                        open.try_set(false);
                    }
                    ctx.refresh_projects();
                }
                // E.g. `Busy`, agents still running: the reason goes in the dialog.
                Err(e) if on_screen(&id) => {
                    error.try_set(Some(e.message));
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
            removing.try_update(|r| r.retain(|r| *r != id));
        });
    };
    let title = move || {
        shown.with(|s| {
            s.as_ref()
                .map(|(_, name)| format!("Rimuovere «{name}» dalla lista?"))
        })
    };
    view! {
        <Dialog open>
            <DialogContent class="shadow-md sm:max-w-md" data_name_prefix="RemoveProjectDialog">
                <DialogBody>
                    <DialogHeader>
                        <DialogTitle>{title}</DialogTitle>
                        <DialogDescription>
                            "I file del repository e i branch atm/… restano; i worktree dell'app vengono salvati con un commit sul loro branch e rimossi; task, cronologia e allegati vengono eliminati dall'app."
                        </DialogDescription>
                    </DialogHeader>
                    <Show when=move || error.with(Option::is_some)>
                        <Callout
                            variant=CalloutVariant::Warning
                            class="md:mx-0"
                            title="Impossibile rimuovere il progetto"
                            attr:data-remove-error=""
                        >
                            {move || error.get()}
                        </Callout>
                    </Show>
                    <DialogFooter>
                        <DialogClose>"Annulla"</DialogClose>
                        <Button
                            variant=ButtonVariant::Destructive
                            attr:data-action="confirm-remove"
                            attr:disabled=busy
                            on:click=confirm
                        >
                            {move || busy().then(|| view! { <Spinner class="size-4" /> })}
                            "Rimuovi"
                        </Button>
                    </DialogFooter>
                </DialogBody>
            </DialogContent>
        </Dialog>
    }
}

/// `false` until `list_projects` answers for the first time: `AppCtx::projects` starts empty,
/// and "Nessun progetto" must not flash meanwhile.
pub fn projects_loaded(ctx: AppCtx) -> Signal<bool> {
    let loaded = RwSignal::new(ctx.projects.with_untracked(|p| !p.is_empty()));
    Effect::new(move |ran: Option<()>| {
        ctx.projects.track();
        if ran.is_some() && !loaded.get_untracked() {
            loaded.set(true);
        }
    });
    Memo::new(move |_| loaded.get()).into()
}

/// "Aggiungi repository": native folder picker, then `add_project` (spec §8.3); selects the
/// new project and shows its warnings.
pub fn add_repository(ctx: AppCtx) {
    spawn_local(async move {
        let path = match ipc::call::<PickRepoFolder>(&Empty {}).await {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(e) => {
                ctx.toasts.app_error(&e);
                return;
            }
        };
        match ipc::call::<AddProject>(&AddProjectReq { path }).await {
            Ok(res) => {
                ctx.toasts
                    .success(format!("Progetto «{}» aggiunto", res.project.name));
                for warning in res.warnings {
                    ctx.toasts.info(warning);
                }
                ctx.select_project(res.project.id, ProjectView::Overview);
                ctx.refresh_projects();
            }
            Err(e) => ctx.toasts.app_error(&e),
        }
    });
}

/// Bar above the project's page: its name and path, the pending approvals, the page tabs, the
/// account chip; the environment banners under it.
#[component]
pub fn Topbar() -> impl IntoView {
    let ctx = use_app();
    let project = Memo::new(move |_| {
        let id = ctx.project.get()?;
        ctx.projects
            .with(|ps| ps.iter().find(|p| p.id == id).cloned())
    });
    let has_project = Memo::new(move |_| ctx.project.with(Option::is_some));
    let cards = project_cards(ctx, false);
    let count = Signal::derive(move || cards.with(|c| c.as_ref().map(Vec::len)));
    view! {
        <div class="shrink-0" data-view="topbar">
            <header class="border-border flex h-13 items-center gap-4 border-b px-5">
                <div class="flex min-w-0 flex-1 items-baseline gap-2.5">
                    {move || {
                        project
                            .get()
                            .map(|p| {
                                view! {
                                    <h1 class="max-w-full shrink-0 truncate text-[15px] font-semibold tracking-tight">
                                        {p.name}
                                    </h1>
                                    <span class="text-muted-foreground truncate font-mono text-[11px]">
                                        {p.repo_path}
                                    </span>
                                }
                            })
                    }}
                </div>
                // Outside the closure above, which re-runs on every `refresh_projects`.
                <Show when=move || has_project.get()>
                    <ApprovalsPill cards />
                    <PageTabs count />
                </Show>
                {move || ctx.env.get().map(|env| view! { <EnvChips env /> })}
            </header>
            <Banners />
        </div>
    }
}

/// The selected project's cards (`get_board`, again on each `board_version`, as the board
/// does): the topbar's Task count and approvals pill, the Riepilogo's counts. `None` until
/// they arrive. A failure is a toast with `report`, else only logged: the page on screen
/// reports its own.
pub(crate) fn project_cards(ctx: AppCtx, report: bool) -> RwSignal<Option<Vec<TaskCard>>> {
    let cards = RwSignal::new(None);
    let fetches = StoredValue::new(0u64);
    let shown = StoredValue::new(None::<Id>);
    Effect::new(move |_| {
        ctx.board_version.track();
        let project = ctx.project.get();
        let seq = fetches
            .try_update_value(|n| {
                *n += 1;
                *n
            })
            .unwrap_or_default();
        if project != shown.get_value() {
            shown.set_value(project.clone());
            cards.set(None);
        }
        let Some(project_id) = project else {
            return;
        };
        spawn_local(async move {
            let res = ipc::call::<GetBoard>(&ProjectIdReq { project_id }).await;
            if fetches.try_get_value() != Some(seq) {
                return;
            }
            match res {
                Ok(list) => {
                    cards.try_set(Some(list));
                }
                Err(e) if report => ctx.toasts.app_error(&e),
                Err(e) => leptos::logging::warn!("get_board: {e}"),
            }
        });
    });
    cards
}

/// "N in attesa di approvazione": opens the first task that waits, on the Task page. Built
/// once behind a memoized `Show`; the click reads the task then.
#[component]
fn ApprovalsPill(cards: RwSignal<Option<Vec<TaskCard>>>) -> impl IntoView {
    let ctx = use_app();
    let waiting = Memo::new(move |_| {
        cards.with(|cards| {
            let cards = cards.as_ref()?;
            let first = cards.iter().find(|c| c.pending_approvals > 0)?;
            let total: u32 = cards.iter().map(|c| c.pending_approvals).sum();
            Some((total, first.task.id.clone()))
        })
    });
    let open = move |_| {
        if let Some((_, task)) = waiting.get_untracked() {
            ctx.project_view.set(ProjectView::Tasks);
            ctx.open_task.set(Some(task));
        }
    };
    view! {
        <Show when=move || waiting.with(Option::is_some)>
            <button
                type="button"
                class=format!(
                    "bg-status-waiting/12 text-status-waiting hover:bg-status-waiting/20 inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full px-2.5 text-xs font-medium whitespace-nowrap transition-colors {FOCUS_RING}",
                )
                title="Apri il task che attende"
                data-action="open-waiting"
                on:click=open
            >
                <StatusDot status=Status::Waiting ping=true />
                {move || {
                    waiting
                        .with(|w| w.as_ref().map(|(n, _)| format!("{n} in attesa di approvazione")))
                }}
            </button>
        </Show>
    }
}

/// Riepilogo | Task | Impostazioni of the selected project (`role=tablist`, ←/→/Home/End move
/// and select, spec F2); the Task tab shows how many tasks the project has.
#[component]
fn PageTabs(count: Signal<Option<usize>>) -> impl IntoView {
    let ctx = use_app();
    let keys = move |e: KeyboardEvent| {
        let all = ProjectView::ALL;
        let i = all
            .iter()
            .position(|v| *v == ctx.project_view.get_untracked())
            .unwrap_or(0);
        let next = match e.key().as_str() {
            "ArrowRight" => (i + 1) % all.len(),
            "ArrowLeft" => (i + all.len() - 1) % all.len(),
            "Home" => 0,
            "End" => all.len() - 1,
            _ => return,
        };
        e.prevent_default();
        ctx.project_view.set(all[next]);
        if let Some(tab) = document()
            .get_element_by_id(&all[next].tab_id())
            .and_then(|el| el.dyn_into::<HtmlElement>().ok())
        {
            let _ = tab.focus();
        }
    };
    let tab = move |view: ProjectView, label: &'static str| {
        let selected = Memo::new(move |_| ctx.project_view.get() == view);
        view! {
            <button
                type="button"
                role="tab"
                id=view.tab_id()
                data-project-view=view.as_str()
                aria-selected=move || selected.get().to_string()
                aria-controls=move || selected.get().then(|| view.page_id())
                tabindex=move || if selected.get() { "0" } else { "-1" }
                class=move || pill_tab(selected.get())
                on:click=move |_| ctx.project_view.set(view)
            >
                {label}
                {(view == ProjectView::Tasks)
                    .then(|| {
                        view! {
                            {move || {
                                count
                                    .get()
                                    .map(|n| {
                                        // "Task (N)" for screen readers, as "Apri task (N)".
                                        view! {
                                            <span class="sr-only">" ("</span>
                                            <span class="text-muted-foreground font-mono text-[11px] tabular-nums">
                                                {n}
                                            </span>
                                            <span class="sr-only">")"</span>
                                        }
                                    })
                            }}
                        }
                    })}
            </button>
        }
    };
    view! {
        <div role="tablist" aria-label="Pagine del progetto" class=PILL_TABLIST on:keydown=keys>
            {tab(ProjectView::Overview, "Riepilogo")}
            {tab(ProjectView::Tasks, "Task")}
            {tab(ProjectView::Settings, "Impostazioni")}
        </div>
    }
}

/// Account chip: the `subscriptionType` only, never the email or the organization (spec §9.2,
/// §10.1); the login method and provider in its tooltip.
#[component]
fn EnvChips(env: EnvStatus) -> impl IntoView {
    let (chip, details) = match &env.auth {
        AuthState::LoggedIn {
            auth_method,
            api_provider,
            subscription_type,
            ..
        } => {
            let chip = subscription_type.clone().unwrap_or_default();
            let details = [
                auth_method.as_deref().map(|m| format!("Accesso: {m}")),
                api_provider.as_deref().map(|p| format!("Provider: {p}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            (chip, details)
        }
        AuthState::LoggedOut => ("Non connesso".to_owned(), String::new()),
        AuthState::Unknown { reason } => ("Accesso da verificare".to_owned(), reason.clone()),
    };
    let chip = if chip.is_empty() {
        "Connesso".to_owned()
    } else {
        chip
    };
    view! {
        <div class="flex shrink-0 items-center">
            <Tooltip>
                <Badge
                    variant=BadgeVariant::Outline
                    class="border-border text-muted-foreground h-6 max-w-56 truncate px-2 text-[11px] font-medium"
                    attr:data-testid="account"
                >
                    {chip}
                </Badge>
                {(!details.is_empty())
                    .then(|| {
                        view! {
                            <TooltipContent position=TooltipPosition::Left class="shadow-md">
                                {details}
                            </TooltipContent>
                        }
                    })}
            </Tooltip>
        </div>
    }
}

/// Environment banners (spec §7.10, §7.9): usage-limit pause with "Riprendi", billing outside
/// the subscription, API key passthrough, unverifiable login, unsupported CLI, problems.
#[component]
fn Banners() -> impl IntoView {
    let ctx = use_app();
    // Only needed for the API key banner: refreshed whenever the environment changes.
    let allow_env_api_key = RwSignal::new(false);
    Effect::new(move |_| {
        if ctx
            .env
            .with(|e| e.as_ref().is_some_and(|e| e.api_key_in_env))
        {
            spawn_local(async move {
                if let Ok(s) = ipc::call::<GetSettings>(&Empty {}).await {
                    allow_env_api_key.try_set(s.allow_env_api_key);
                }
            });
        }
    });
    let resuming = RwSignal::new(false);
    let resume = move |_| {
        resuming.set(true);
        spawn_local(async move {
            match ipc::call::<ResumeAgents>(&Empty {}).await {
                Ok(env) => ctx.set_env(env),
                Err(e) => ctx.toasts.app_error(&e),
            }
            resuming.try_set(false);
        });
    };

    // The banners with a button are built once behind memoized `Show`s: rebuilding a
    // `<Button on:click>` in place makes Leptos 0.8 keep the old handler next to the new one.
    let paused = Memo::new(move |_| ctx.env.with(|e| e.as_ref()?.paused.clone()));
    let unknown = Memo::new(move |_| {
        ctx.env.with(|e| match e.as_ref().map(|e| &e.auth) {
            Some(AuthState::Unknown { reason }) => Some(reason.clone()),
            _ => None,
        })
    });
    let api_key = Memo::new(move |_| {
        ctx.env
            .with(|e| e.as_ref().is_some_and(|e| e.api_key_in_env))
            && allow_env_api_key.get()
    });
    let notes = Memo::new(move |_| ctx.env.with(|e| e.as_ref().map(notes).unwrap_or_default()));
    view! {
        <Show when=move || paused.with(Option::is_some)>
            <Banner testid="paused">
                <span class="flex-1">"Nuovi avvii in pausa. " {move || paused.get()}</span>
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Outline
                    class="h-7 text-[13px]"
                    attr:disabled=move || resuming.get()
                    on:click=resume
                >
                    "Riprendi"
                </Button>
            </Banner>
        </Show>
        <Show when=move || api_key.get()>
            <Banner testid="api-key">
                <span class="flex-1">
                    "La chiave API dell'ambiente viene passata agli agenti: l'uso viene fatturato via API."
                </span>
            </Banner>
        </Show>
        <Show when=move || unknown.with(Option::is_some)>
            <Banner testid="auth-unknown">
                <span class="flex-1">
                    "Impossibile verificare l'accesso a Claude Code: " {move || unknown.get()}
                </span>
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Outline
                    class="h-7 text-[13px]"
                    on:click=move |_| ctx.refresh_env(true)
                >
                    "Ricontrolla"
                </Button>
            </Banner>
        </Show>
        <For
            each=move || notes.get()
            key=|note| note.clone()
            children=|(testid, text)| view! { <Banner testid>{text}</Banner> }
        />
    }
}

/// Text-only banners: billing outside the subscription, another API endpoint in the
/// environment, unsupported CLI, problems.
fn notes(env: &EnvStatus) -> Vec<(&'static str, String)> {
    let mut notes = Vec::new();
    if let Some(via) = billed_via(env) {
        notes.push((
            "billing",
            format!("L'uso degli agenti viene fatturato via {via}, non dall'abbonamento Claude."),
        ));
    }
    if env.base_url_env {
        notes.push((
            "base-url",
            "L'ambiente dell'app imposta ANTHROPIC_BASE_URL (o l'endpoint di un provider \
             cloud): gli agenti mandano le richieste, con le credenziali dell'abbonamento \
             Claude, a quell'endpoint invece che ad Anthropic, e l'uso può essere fatturato lì."
                .into(),
        ));
    }
    if !env.claude.supported {
        notes.push((
            "unsupported",
            format!(
                "Claude Code {} è più vecchio del minimo supportato {}: aggiornalo con `claude update`.",
                env.claude.version.as_deref().unwrap_or_default(),
                env.claude.min_version
            ),
        ));
    }
    notes.extend(env.problems.iter().map(|p| ("problem", p.clone())));
    notes
}

#[component]
fn Banner(testid: &'static str, children: Children) -> impl IntoView {
    view! {
        <div
            class="bg-status-waiting/8 border-status-waiting/25 flex items-center gap-3 border-b px-5 py-2 text-[13px]"
            role="status"
            data-banner=testid
        >
            <TriangleAlert class="text-status-waiting size-4 shrink-0" />
            {children()}
        </div>
    }
}

/// Who bills the agents when it is not the Claude subscription: a login other than
/// claude.ai, a third-party API provider, or a cloud provider selected by the environment.
fn billed_via(env: &EnvStatus) -> Option<String> {
    if env.cloud_provider_env {
        return Some("il provider cloud impostato nell'ambiente".into());
    }
    let AuthState::LoggedIn {
        auth_method,
        api_provider,
        ..
    } = &env.auth
    else {
        return None;
    };
    api_provider
        .clone()
        .filter(|p| p != "firstParty")
        .or_else(|| auth_method.clone().filter(|m| m != "claude.ai"))
}

/// Dotted numeric versions: `true` if `version` is newer than `than`.
fn version_newer(version: &str, than: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(['.', ' ', '-'])
            .map_while(|n| n.parse().ok())
            .collect()
    };
    parse(version) > parse(than)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_banner_names_who_bills() {
        let logged_in = |method: &str, provider: &str| AuthState::LoggedIn {
            auth_method: Some(method.into()),
            api_provider: Some(provider.into()),
            email: None,
            org_name: None,
            subscription_type: None,
        };
        let mut env = EnvStatus {
            claude: atm_types::ClaudeInfo {
                path: None,
                version: None,
                supported: true,
                min_version: String::new(),
                tested_version: String::new(),
            },
            auth: logged_in("claude.ai", "firstParty"),
            git_version: None,
            api_key_in_env: false,
            cloud_provider_env: false,
            base_url_env: false,
            paused: None,
            running: 0,
            max_running: 2,
            problems: Vec::new(),
            checked_at: 0,
        };
        assert_eq!(billed_via(&env), None);
        env.auth = logged_in("console", "firstParty");
        assert_eq!(billed_via(&env).as_deref(), Some("console"));
        env.auth = logged_in("claude.ai", "bedrock");
        assert_eq!(billed_via(&env).as_deref(), Some("bedrock"));
        env.auth = AuthState::LoggedOut;
        assert_eq!(billed_via(&env), None);
        env.cloud_provider_env = true;
        assert!(billed_via(&env).is_some());
        assert!(notes(&env).iter().all(|(id, _)| *id != "base-url"));
        env.base_url_env = true;
        assert!(
            notes(&env)
                .iter()
                .any(|(id, text)| *id == "base-url" && text.contains("ANTHROPIC_BASE_URL"))
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(version_newer("2.1.300", "2.1.283"));
        assert!(version_newer("2.10.0", "2.9.9"));
        assert!(!version_newer("2.1.283", "2.1.283"));
        assert!(!version_newer("2.1.99", "2.1.283"));
    }
}
