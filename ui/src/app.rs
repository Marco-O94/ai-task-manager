//! Root component (spec §9.2): `AppCtx`, onboarding gate, layout (sidebar | project page:
//! overview, board and task panel, or settings), global events (`changed` with a 100 ms
//! debounce, `env_changed`) and toasts.

use std::time::Duration;

use atm_types::{
    Changed, EVENT_CHANGED, EVENT_ENV_CHANGED, EnvStatus, GetEnv, GetEnvReq, Id, ListProjects,
    Project, TaskCard,
};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::ipc;
use crate::views::board::{Board, NoProject};
use crate::views::onboarding::{Onboarding, gate_passed};
use crate::views::overview::Overview;
use crate::views::settings::ProjectSettings;
use crate::views::sidebar::{Sidebar, Topbar, projects_loaded};
use crate::views::task_dialog::TaskDialogMode;
use crate::views::task_panel::TaskPanel;
use crate::views::update::{RequiredUpdate, blocks_app, provide_update_ctx, use_update};
use crate::widgets::toast::{Toaster, Toasts};

const CHANGED_DEBOUNCE: Duration = Duration::from_millis(100);

/// Page of the selected project, switched by the topbar tabs; not persisted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProjectView {
    /// The landing page (`views/overview.rs`).
    #[default]
    Overview,
    /// Board and task panel.
    Tasks,
    /// Project settings (`views/settings/project.rs`).
    Settings,
}

impl ProjectView {
    pub const ALL: [Self; 3] = [Self::Overview, Self::Tasks, Self::Settings];

    /// `data-project-view` of its tab (DOM contract, spec §9.2).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Tasks => "tasks",
            Self::Settings => "settings",
        }
    }

    /// Id of its topbar tab (`role=tab`).
    pub fn tab_id(self) -> String {
        format!("project-tab-{}", self.as_str())
    }

    /// Id of its page (`role=tabpanel`).
    pub fn page_id(self) -> String {
        format!("project-page-{}", self.as_str())
    }
}

/// Global UI state, provided as context; no router, views switch on these signals.
#[derive(Clone, Copy)]
pub struct AppCtx {
    /// Latest `get_env` / `env_changed`; `None` until a check succeeds.
    pub env: RwSignal<Option<EnvStatus>>,
    pub projects: RwSignal<Vec<Project>>,
    /// Selected project; written through [`AppCtx::select_project`].
    pub project: RwSignal<Option<Id>>,
    /// Page shown for `project`. Only `select_project` and the topbar tabs write it: no
    /// Effect on `project` does, so a caller's choice of page stands.
    pub project_view: RwSignal<ProjectView>,
    /// Project whose removal awaits confirmation, `(id, name)`: the one `RemoveProjectDialog`
    /// (sidebar), opened by the sidebar menu and by the settings page.
    pub remove_target: RwSignal<Option<(Id, String)>>,
    /// Bumped when the board must refetch `get_board`.
    pub board_version: RwSignal<u64>,
    /// The board's cards (`views/board.rs` owns them): the task panel and the approval cards
    /// read task titles here. Empty while no board is mounted.
    pub cards: RwSignal<Vec<TaskCard>>,
    /// Task shown in the side panel.
    pub open_task: RwSignal<Option<Id>>,
    /// The board's one `TaskDialog` (`views/board.rs` mounts it): the task panel opens it for
    /// "Aggiungi sotto task".
    pub task_dialog: RwSignal<Option<TaskDialogMode>>,
    /// Bumped when the panel must refetch `get_task_detail`.
    pub detail_version: RwSignal<u64>,
    pub toasts: Toaster,
}

pub fn use_app() -> AppCtx {
    expect_context()
}

impl AppCtx {
    fn new() -> Self {
        Self {
            env: RwSignal::new(None),
            projects: RwSignal::new(Vec::new()),
            project: RwSignal::new(None),
            project_view: RwSignal::new(ProjectView::default()),
            remove_target: RwSignal::new(None),
            board_version: RwSignal::new(0),
            cards: RwSignal::new(Vec::new()),
            open_task: RwSignal::new(None),
            task_dialog: RwSignal::new(None),
            detail_version: RwSignal::new(0),
            toasts: Toaster::new(),
        }
    }

    /// Stores `env` unless a newer one is already there: a slow `get_env` reply (`force`
    /// waits up to 10 s for `auth status`) can arrive after a newer `env_changed`.
    pub fn set_env(self, env: EnvStatus) {
        self.env.try_update(|current| {
            if current
                .as_ref()
                .is_none_or(|c| c.checked_at <= env.checked_at)
            {
                *current = Some(env);
            }
        });
    }

    /// `get_env{force}` into `env`; a failure becomes an error toast.
    pub fn refresh_env(self, force: bool) {
        spawn_local(async move {
            match ipc::call::<GetEnv>(&GetEnvReq { force }).await {
                Ok(env) => self.set_env(env),
                Err(e) => {
                    leptos::logging::warn!("get_env: {e}");
                    self.toasts.app_error(&e);
                }
            }
        });
    }

    /// Selects project `id` on page `view` and closes the task panel; every selection goes
    /// through here (sidebar, "Aggiungi repository", the sidebar menu, the automatic one of
    /// [`AppCtx::refresh_projects`]). Re-selecting the selected project keeps `project`
    /// untouched, so its pages do not refill.
    pub fn select_project(self, id: Id, view: ProjectView) {
        if self.open_task.with_untracked(Option::is_some) {
            self.open_task.try_set(None);
        }
        if self.project_view.get_untracked() != view {
            self.project_view.try_set(view);
        }
        if self.project.with_untracked(|p| p.as_ref() != Some(&id)) {
            self.project.try_set(Some(id));
        }
    }

    /// `list_projects` into `projects`; keeps the selection if it still exists, else
    /// selects the first project on its overview.
    pub fn refresh_projects(self) {
        spawn_local(async move {
            match ipc::call::<ListProjects>(&Default::default()).await {
                Ok(projects) => {
                    let keep = self
                        .project
                        .get_untracked()
                        .filter(|id| projects.iter().any(|p| &p.id == id));
                    let selected = keep.or_else(|| projects.first().map(|p| p.id.clone()));
                    self.projects.try_set(projects);
                    if selected != self.project.get_untracked() {
                        match selected {
                            Some(id) => self.select_project(id, ProjectView::Overview),
                            None => {
                                self.open_task.try_set(None);
                                self.project.try_set(None);
                            }
                        }
                    }
                }
                Err(e) => self.toasts.app_error(&e),
            }
        });
    }

    /// Applies a batch of `changed` events (spec §6.4).
    fn apply_changes(self, changes: &[Changed]) {
        if changes.iter().any(|c| c.project_id.is_none()) {
            self.refresh_projects();
        }
        let project = self.project.get_untracked();
        if project.is_some() && changes.iter().any(|c| c.project_id == project) {
            self.board_version.update(|v| *v += 1);
        }
        let task = self.open_task.get_untracked();
        if task.is_some() && changes.iter().any(|c| c.task_id == task) {
            self.detail_version.update(|v| *v += 1);
        }
    }
}

#[component]
pub fn App() -> impl IntoView {
    let ctx = AppCtx::new();
    provide_context(ctx);
    provide_update_ctx();
    let update = use_update();

    // The gate stays closed until the first successful `get_env` says CLI + login are fine.
    let continue_anyway = RwSignal::new(false);
    let ready = Memo::new(move |_| {
        ctx.env.with(|env| {
            env.as_ref()
                .is_some_and(|env| gate_passed(env, continue_anyway.get()))
        })
    });
    Effect::new(move |was_ready: Option<bool>| {
        let ready = ready.get();
        if ready && was_ready != Some(true) {
            ctx.refresh_projects();
        }
        ready
    });

    ctx.refresh_env(false);
    #[cfg(feature = "mock")]
    open_mock_task(ctx);
    listen_global_events(ctx);
    // Re-check auth on focus once the environment is known (the backend caches 60 s).
    let _ = window_event_listener(ev::focus, move |_| {
        if ctx.env.with_untracked(Option::is_some) {
            ctx.refresh_env(false);
        }
    });

    view! {
        // Under the modal of a required update nothing takes the focus.
        <div class="contents" inert=move || blocks_app(update)>
            <Show when=move || ready.get() fallback=move || view! { <Onboarding continue_anyway /> }>
                <Layout />
            </Show>
        </div>
        <RequiredUpdate />
        <Toasts toaster=ctx.toasts />
    }
}

/// Mock builds only: `?task=<id>` opens that task's panel on the Tasks page, so the task
/// panel can be developed without the board (e.g. `http://localhost:1420/?task=task-inreview`).
/// It waits for the first automatic selection, which lands on the overview and closes the
/// panel.
#[cfg(feature = "mock")]
fn open_mock_task(ctx: AppCtx) {
    let Some(task) = mock_open_task() else {
        return;
    };
    Effect::new(move |opened: Option<bool>| {
        if opened == Some(true) {
            return true;
        }
        if ctx.project.with(Option::is_none) {
            return false;
        }
        ctx.project_view.set(ProjectView::Tasks);
        ctx.open_task.set(Some(task.clone()));
        true
    });
}

#[cfg(feature = "mock")]
fn mock_open_task() -> Option<Id> {
    let search = window().location().search().ok()?;
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("task="))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Sidebar (240 px) | topbar over the `main` landmark with the selected project's page:
/// overview, board and task panel (55 %, while a task is open), or settings. Each page sits
/// behind a `Show` on a memo, so it is mounted fresh on every visit and never rebuilt in place
/// (Leptos 0.8 would keep the old buttons' click handlers).
#[component]
fn Layout() -> impl IntoView {
    let ctx = use_app();
    let has_project = Memo::new(move |_| ctx.project.with(Option::is_some));
    let current = Memo::new(move |_| ctx.project_view.get());
    let projects_ready = projects_loaded(ctx);
    view! {
        <div class="bg-background text-foreground flex h-screen overflow-hidden">
            <Sidebar />
            <div class="flex min-w-0 flex-1 flex-col">
                <Topbar />
                <main class="flex min-h-0 flex-1">
                    <Show
                        when=move || has_project.get()
                        fallback=move || projects_ready.get().then(|| view! { <NoProject /> })
                    >
                        <Show when=move || current.get() == ProjectView::Overview>
                            <ProjectPage page=ProjectView::Overview class="overflow-y-auto">
                                <Overview />
                            </ProjectPage>
                        </Show>
                        <Show when=move || current.get() == ProjectView::Tasks>
                            <ProjectPage page=ProjectView::Tasks class="overflow-hidden">
                                <Board />
                            </ProjectPage>
                            {move || {
                                ctx.open_task
                                    .get()
                                    .map(|task_id| {
                                        view! {
                                            <aside class="border-border bg-background w-[55%] shrink-0 overflow-hidden border-l shadow-lg">
                                                <TaskPanel task_id />
                                            </aside>
                                        }
                                    })
                            }}
                        </Show>
                        <Show when=move || current.get() == ProjectView::Settings>
                            <ProjectPage page=ProjectView::Settings class="overflow-y-auto">
                                <ProjectSettings />
                            </ProjectPage>
                        </Show>
                    </Show>
                </main>
            </div>
        </div>
    }
}

/// One page of the selected project: the `role=tabpanel` its topbar tab controls, inside the
/// layout's `main` (a `main` with another role would no longer be the landmark).
#[component]
fn ProjectPage(page: ProjectView, class: &'static str, children: Children) -> impl IntoView {
    view! {
        <div
            id=page.page_id()
            role="tabpanel"
            aria-labelledby=page.tab_id()
            class=format!("min-w-0 flex-1 {class}")
        >
            {children()}
        </div>
    }
}

/// `changed` (debounced, spec §6.4) and `env_changed`, for the whole app lifetime.
fn listen_global_events(ctx: AppCtx) {
    let pending = StoredValue::new(Vec::<Changed>::new());
    let on_changed = move |c: Changed| {
        let first = pending
            .try_update_value(|p| {
                p.push(c);
                p.len() == 1
            })
            .unwrap_or(false);
        if first {
            set_timeout(
                move || {
                    if let Some(changes) = pending.try_update_value(std::mem::take) {
                        ctx.apply_changes(&changes);
                    }
                },
                CHANGED_DEBOUNCE,
            );
        }
    };
    let on_env = move |env: EnvStatus| ctx.set_env(env);
    spawn_local(async move {
        for result in [
            ipc::listen::<Changed>(EVENT_CHANGED, on_changed).await,
            ipc::listen::<EnvStatus>(EVENT_ENV_CHANGED, on_env).await,
        ] {
            match result {
                // App-lifetime listeners: never unlistened.
                Ok((closure, _unlisten)) => closure.forget(),
                Err(e) => ctx.toasts.app_error(&e),
            }
        }
    });
}
