//! Root component (spec §9.2): `AppCtx`, onboarding gate, layout (sidebar | board | task
//! panel), global events (`changed` with a 100 ms debounce, `env_changed`) and toasts.

use std::time::Duration;

use atm_types::{
    Changed, EVENT_CHANGED, EVENT_ENV_CHANGED, EnvStatus, GetEnv, GetEnvReq, Id, ListProjects,
    Project,
};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::ipc;
use crate::views::board::Board;
use crate::views::onboarding::{Onboarding, gate_passed};
use crate::views::sidebar::{Sidebar, Topbar};
use crate::views::task_panel::TaskPanel;
use crate::widgets::toast::{Toaster, Toasts};

const CHANGED_DEBOUNCE: Duration = Duration::from_millis(100);

/// Global UI state, provided as context; no router, views switch on these signals.
#[derive(Clone, Copy)]
pub struct AppCtx {
    /// Latest `get_env` / `env_changed`; `None` until a check succeeds.
    pub env: RwSignal<Option<EnvStatus>>,
    pub projects: RwSignal<Vec<Project>>,
    /// Selected project.
    pub project: RwSignal<Option<Id>>,
    /// Bumped when the board must refetch `get_board`.
    pub board_version: RwSignal<u64>,
    /// Task shown in the side panel.
    pub open_task: RwSignal<Option<Id>>,
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
            board_version: RwSignal::new(0),
            open_task: RwSignal::new(None),
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

    /// `list_projects` into `projects`; keeps the selection if it still exists, else
    /// selects the first project.
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
                        self.project.try_set(selected);
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
    ctx.open_task.set(mock_open_task());
    listen_global_events(ctx);
    // Re-check auth on focus once the environment is known (the backend caches 60 s).
    let _ = window_event_listener(ev::focus, move |_| {
        if ctx.env.with_untracked(Option::is_some) {
            ctx.refresh_env(false);
        }
    });

    view! {
        <Show when=move || ready.get() fallback=move || view! { <Onboarding continue_anyway /> }>
            <Layout />
        </Show>
        <Toasts toaster=ctx.toasts />
    }
}

/// Mock builds only: `?task=<id>` opens that task's panel at startup, so the task panel can
/// be developed without the board (e.g. `http://localhost:1420/?task=task-inreview`).
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

/// Sidebar (240 px) | topbar over board and task panel (55 %, while a task is open).
#[component]
fn Layout() -> impl IntoView {
    let ctx = use_app();
    view! {
        <div class="bg-background text-foreground flex h-screen overflow-hidden">
            <Sidebar />
            <div class="flex min-w-0 flex-1 flex-col">
                <Topbar />
                <div class="flex min-h-0 flex-1">
                    <main class="min-w-0 flex-1 overflow-hidden">
                        <Board />
                    </main>
                    {move || {
                        ctx.open_task
                            .get()
                            .map(|task_id| {
                                view! {
                                    <aside class="w-[55%] shrink-0 overflow-hidden border-l">
                                        <TaskPanel task_id />
                                    </aside>
                                }
                            })
                    }}
                </div>
            </div>
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
