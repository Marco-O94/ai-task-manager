//! Project overview, the page a project opens on (spec F3, §9.2): description, state, agent
//! instructions, MCP servers, `.claude/`, README, from `get_project_overview`. Owner: UI-SHELL.
//! Root `[data-view=overview]`; files `[data-overview-file=<path>]`, servers
//! `[data-mcp-server=<name>]` (DOM contract, spec §9.2).
//!
//! The overview is fetched on mount, when the selected project changes and with "Aggiorna";
//! the task counts follow `board_version` as the board does. File contents are plain text
//! (`whitespace-pre-wrap`): rendering markdown would need a new dependency and raw HTML, which
//! §10.3 forbids.
//!
//! Layout of the design: a hero card (branch, configuration, name, description, task
//! distribution) over a 12-column bento of the repository's files.

use atm_types::{
    AppError, ConfigPolicy, ContextFile, ContextFileKind, Empty, GetProjectOverview, GetSettings,
    Id, McpServer, Project, ProjectIdReq, ProjectOverview, TaskCard, TaskStatus,
};
use icons::{
    ArrowRight, ChevronRight, FileText, GitBranch, Pencil, Plus, RefreshCw, Server, Shield,
    ShieldAlert, ShieldCheck, TriangleAlert,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;

use crate::app::{ProjectView, use_app};
use crate::ipc;
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::collapsible::{Collapsible, CollapsibleContent, CollapsibleTrigger};
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;
use crate::views::board::column_title;
use crate::views::sidebar::project_cards;
use crate::views::start_dialog::mode_help;
use crate::views::transcript::mode_label;
use crate::widgets::status::{Status, StatusDot};

const CARD: &str = "rounded-xl border border-border bg-card text-card-foreground shadow-xs";
const BTN_OUTLINE: &str = "inline-flex h-8 items-center justify-center gap-1.5 rounded-md border border-border bg-card px-3 text-[13px] font-medium whitespace-nowrap text-foreground shadow-xs transition-colors hover:bg-accent hover:text-accent-foreground disabled:pointer-events-none disabled:opacity-50 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";
const BTN_OUTLINE_SM: &str = "inline-flex h-7 shrink-0 items-center justify-center gap-1.5 rounded-md border border-border bg-card px-3 text-[13px] font-medium whitespace-nowrap text-foreground shadow-xs transition-colors hover:bg-accent hover:text-accent-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";
const BTN_PRIMARY: &str = "inline-flex h-8 items-center justify-center gap-1.5 rounded-md bg-primary px-3 text-[13px] font-medium whitespace-nowrap text-primary-foreground shadow-xs transition-colors hover:bg-primary/90 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";
const LINK: &str = "rounded-sm font-medium text-primary hover:underline outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";
/// Branch and configuration chips of the hero.
const CHIP: &str = "inline-flex h-6 items-center gap-1.5 rounded-md border border-border bg-background px-2 text-[11px]";
const OUTLINE_BADGE: &str = "inline-flex h-5 shrink-0 items-center rounded-md border border-border px-1.5 text-[11px] font-medium whitespace-nowrap text-muted-foreground";
/// A name under `.claude/` or an MCP key.
const NAME_CHIP: &str = "rounded-sm bg-muted px-1.5 font-mono text-[11px]";
/// A file the repository does not have.
const PLACEHOLDER: &str = "flex h-9 items-center gap-2 rounded-lg border border-dashed border-border px-3 text-muted-foreground";
/// The first lines of a file block, then the fade.
const PRE_PREVIEW: &str = "max-h-[92px] overflow-hidden whitespace-pre-wrap [overflow-wrap:anywhere] px-3 py-2.5 font-mono text-[11.5px] leading-[18px] text-foreground/85";
const PRE_FULL: &str = "max-h-96 overflow-auto whitespace-pre-wrap [overflow-wrap:anywhere] px-3 py-2.5 font-mono text-[11.5px] leading-[18px] text-foreground/85 outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring";
/// The agent instruction files, in the order the backend reads them (spec F3).
const INSTRUCTIONS: [&str; 3] = ["CLAUDE.md", ".claude/CLAUDE.md", "AGENTS.md"];

/// A `get_project_overview` reply, numbered by its fetch.
type Fetched = (u64, Result<ProjectOverview, AppError>);

#[component]
pub fn Overview() -> impl IntoView {
    let ctx = use_app();
    let project = Memo::new(move |_| {
        let id = ctx.project.get()?;
        ctx.projects
            .with(|ps| ps.iter().find(|p| p.id == id).cloned())
    });

    // Latest reply for the project on screen; `None` while the first one is on its way.
    let overview = RwSignal::new(None::<Fetched>);
    let loading = RwSignal::new(false);
    let fetches = StoredValue::new(0u64);
    let fetch = move |project_id: Id| {
        let seq = fetches
            .try_update_value(|n| {
                *n += 1;
                *n
            })
            .unwrap_or_default();
        loading.set(true);
        spawn_local(async move {
            let res = ipc::call::<GetProjectOverview>(&ProjectIdReq { project_id }).await;
            // A newer fetch (another project, "Aggiorna") owns the page.
            if fetches.try_get_value() == Some(seq) {
                overview.try_set(Some((seq, res)));
                loading.try_set(false);
            }
        });
    };
    Effect::new(move |_| {
        let Some(id) = ctx.project.get() else {
            return;
        };
        overview.set(None);
        fetch(id);
    });
    let refresh = Callback::new(move |()| {
        if let Some(id) = ctx.project.get_untracked() {
            fetch(id);
        }
    });

    // Task counts: `get_board`, again on each `board_version`.
    let cards = project_cards(ctx, true);

    // The app default, for a project without its own model.
    let app_model = RwSignal::new(None::<Option<String>>);
    spawn_local(async move {
        if let Ok(s) = ipc::call::<GetSettings>(&Empty {}).await {
            app_model.try_set(Some(s.default_model));
        }
    });

    view! {
        <div class="mx-auto w-full max-w-[1120px] space-y-4 px-8 py-6" data-view="overview">
            <Hero project overview cards loading refresh />
            // Keyed by the fetch: each reply mounts its sections afresh instead of patching the
            // previous ones in place.
            <For
                each=move || overview.get()
                key=|(seq, _)| *seq
                children=move |(_, res)| match res {
                    Ok(overview) => view! { <Sections overview project app_model /> }.into_any(),
                    Err(e) => {
                        view! {
                            <div class="grid grid-cols-12 items-start gap-4">
                                <Callout
                                    variant=CalloutVariant::Warning
                                    class="col-span-7 md:mx-0"
                                    title="File del repository non disponibili"
                                    attr:data-overview-error=""
                                >
                                    {e.message}
                                </Callout>
                                <ConfigCard project app_model class="col-span-5" />
                            </div>
                        }
                            .into_any()
                    }
                }
            />
            // The bento's shape while the first reply is on its way.
            <Show when=move || overview.with(Option::is_none)>
                <div class="grid grid-cols-12 gap-4">
                    <Skeleton class="col-span-7 row-span-2 h-[22rem] rounded-xl" />
                    <Skeleton class="col-span-5 h-44 rounded-xl" />
                    <Skeleton class="col-span-5 h-40 rounded-xl" />
                    <Skeleton class="col-span-7 h-32 rounded-xl" />
                    <Skeleton class="col-span-5 h-32 rounded-xl" />
                </div>
            </Show>
        </div>
    }
}

/// The project at a glance: branch read and configuration, name, description, "Aggiorna" and
/// "Apri task", then the tasks by column, the running agents and a pending approval.
#[component]
fn Hero(
    project: Memo<Option<Project>>,
    overview: RwSignal<Option<Fetched>>,
    cards: RwSignal<Option<Vec<TaskCard>>>,
    loading: RwSignal<bool>,
    refresh: Callback<()>,
) -> impl IntoView {
    let ctx = use_app();
    let name = move || project.with(|p| p.as_ref().map(|p| p.name.clone()));
    let description = Memo::new(move |_| {
        project.with(|p| {
            p.as_ref()
                .map(|p| p.description.trim().to_owned())
                .unwrap_or_default()
        })
    });
    let branch = move || {
        let read = overview.with(|o| match o {
            Some((_, Ok(o))) => Some(format!("{} @ {}", o.branch, short_commit(&o.commit))),
            _ => None,
        });
        read.or_else(|| project.with(|p| p.as_ref().map(|p| p.default_target_branch.clone())))
    };
    let config = Memo::new(move |_| project.with(|p| p.as_ref().map(Config::of)));
    let total = move || cards.with(|c| c.as_ref().map(Vec::len));
    let no_tasks = Memo::new(move |_| total() == Some(0));
    let to_settings = move |_| ctx.project_view.set(ProjectView::Settings);
    view! {
        <section class=CARD>
            <div class="flex items-start gap-6 p-6">
                <div class="min-w-0 flex-1">
                    <div class="flex flex-wrap items-center gap-2">
                        <span
                            class=format!("{CHIP} font-mono")
                            title="Branch target predefinito e commit da cui sono letti i file: la base dei worktree degli agenti"
                            data-testid="overview-branch"
                        >
                            <GitBranch class="size-3 shrink-0 text-muted-foreground" />
                            {branch}
                        </span>
                        {move || {
                            config
                                .get()
                                .map(|c| {
                                    view! {
                                        <span class=format!("{CHIP} font-medium") title=c.note()>
                                            {c.icon("size-3")}
                                            {c.label()}
                                        </span>
                                    }
                                })
                        }}
                    </div>
                    <h2 class="mt-3 text-[26px] leading-8 font-semibold tracking-tight [overflow-wrap:anywhere]">
                        {name}
                    </h2>
                    <Show
                        when=move || description.with(|d| !d.is_empty())
                        fallback=move || {
                            view! {
                                <div class="mt-3 flex max-w-[68ch] items-center gap-3 rounded-lg border border-dashed border-border px-3 py-2.5">
                                    <p class="flex-1 text-[13px] text-muted-foreground text-pretty">
                                        "Nessuna descrizione. Scrivi a cosa serve il repository: la trovi qui."
                                    </p>
                                    <button
                                        type="button"
                                        class=BTN_OUTLINE_SM
                                        data-action="edit-description"
                                        on:click=to_settings
                                    >
                                        <Pencil class="size-3.5 shrink-0" />
                                        "Scrivi descrizione"
                                    </button>
                                </div>
                            }
                        }
                    >
                        <p class="mt-2 max-w-[68ch] text-[14px] leading-[22px] text-muted-foreground text-pretty">
                            <span class="whitespace-pre-wrap [overflow-wrap:anywhere]" data-testid="project-description">
                                {move || description.get()}
                            </span>
                            " "
                            <button type="button" class=LINK on:click=to_settings>
                                "Modifica"
                            </button>
                        </p>
                    </Show>
                </div>
                <div class="flex shrink-0 items-center gap-2">
                    <button
                        type="button"
                        class=BTN_OUTLINE
                        data-action="refresh-overview"
                        disabled=move || loading.get()
                        on:click=move |_| refresh.run(())
                    >
                        {move || {
                            if loading.get() {
                                view! { <Spinner class="size-3.5" /> }.into_any()
                            } else {
                                view! { <RefreshCw class="size-3.5 shrink-0" /> }.into_any()
                            }
                        }}
                        "Aggiorna"
                    </button>
                    // The count reads "Apri task (N)" as text: the brackets are for screen
                    // readers only, the chip is the visible count. A project without tasks is
                    // invited to create its first one, on the (empty) Task page.
                    <button
                        type="button"
                        class=BTN_PRIMARY
                        data-action="open-tasks"
                        on:click=move |_| ctx.project_view.set(ProjectView::Tasks)
                    >
                        <Show
                            when=move || no_tasks.get()
                            fallback=move || {
                                view! {
                                    "Apri task"
                                    {move || {
                                        total()
                                            .map(|n| {
                                                view! {
                                                    <span class="sr-only">" ("</span>
                                                    <span class="rounded-sm bg-primary-foreground/20 px-1 font-mono text-[11px] tabular-nums">
                                                        {n}
                                                    </span>
                                                    <span class="sr-only">")"</span>
                                                }
                                            })
                                    }}
                                    <ArrowRight class="size-3.5 shrink-0" />
                                }
                            }
                        >
                            <Plus class="size-3.5 shrink-0" />
                            "Crea il primo task"
                        </Show>
                    </button>
                </div>
            </div>
            <TaskStats cards />
            <ApprovalCallout cards />
        </section>
    }
}

/// Distribution bar and counters of the board's columns, and the running agents: this
/// project's and the app's against its limit.
#[component]
fn TaskStats(cards: RwSignal<Option<Vec<TaskCard>>>) -> impl IntoView {
    let ctx = use_app();
    let counts = Memo::new(move |_| {
        cards.with(|c| {
            c.as_ref().map(|c| {
                TaskStatus::ALL
                    .iter()
                    .map(|&s| c.iter().filter(|card| card.task.status == s).count())
                    .collect::<Vec<_>>()
            })
        })
    });
    let count = move |i: usize| {
        counts.with(|c| {
            c.as_ref()
                .map_or_else(|| "–".to_owned(), |c| c[i].to_string())
        })
    };
    // No tasks yet: only the empty bar and its caption, no row of zeros.
    let empty = Memo::new(move |_| cards.with(|c| c.as_ref().is_some_and(Vec::is_empty)));
    let bar = move || {
        let counts = counts.get().unwrap_or_default();
        let mut segments = Vec::new();
        for (&status, &n) in TaskStatus::ALL.iter().zip(&counts).filter(|(_, n)| **n > 0) {
            if !segments.is_empty() {
                segments.push(view! { <span class="w-0.5 shrink-0 bg-card"></span> }.into_any());
            }
            let fill = Status::of_column(status).bg();
            let dim = if status == TaskStatus::Cancelled {
                "opacity-50"
            } else {
                ""
            };
            segments.push(
                view! { <span class=format!("h-full basis-0 {fill} {dim}") style:flex-grow=n.to_string()></span> }
                    .into_any(),
            );
        }
        view! {
            <div class="flex h-2 overflow-hidden rounded-full bg-muted" role="img" aria-label="Distribuzione dei task per stato">
                {segments}
            </div>
        }
    };
    let running = move || {
        let here = cards.with(|c| c.as_ref().map(|c| c.iter().filter(|c| c.running).count()));
        let app = ctx.env.with(|e| {
            e.as_ref()
                .map(|e| format!("{}/{} nell'app", e.running, e.max_running))
        });
        [here.map(|n| format!("{n} qui")), app]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    };
    view! {
        <div class="border-t border-border px-6 py-4">
            <Show
                when=move || empty.get()
                fallback=move || {
                    view! {
                        {bar}
                        <dl class="mt-3 flex flex-wrap items-center gap-x-6 gap-y-2" data-testid="overview-counts">
                            {TaskStatus::ALL
                                .iter()
                                .enumerate()
                                .map(|(i, &status)| {
                                    view! {
                                        <div class="flex items-center gap-2" data-count=status.as_str()>
                                            <dt class="text-muted-foreground flex items-center gap-2">
                                                <StatusDot status=Status::of_column(status) />
                                                {column_title(status)}
                                            </dt>
                                            <dd class="font-mono font-semibold tabular-nums">{move || count(i)}</dd>
                                        </div>
                                    }
                                })
                                .collect_view()}
                            <div class="ml-auto flex items-center gap-2 text-xs">
                                <dt class="text-muted-foreground">"Agenti attivi"</dt>
                                <dd class="font-medium" data-testid="overview-running">{running}</dd>
                            </div>
                        </dl>
                    }
                }
            >
                <div class="flex items-center gap-3 text-muted-foreground">
                    <div class="h-2 flex-1 rounded-full bg-muted"></div>
                    <span class="text-xs">"Nessun task ancora"</span>
                </div>
            </Show>
        </div>
    }
}

/// A task of the project whose agent waits for an approval, with a way to it. The command
/// waiting is in the task's transcript only, so it is not shown here.
#[component]
fn ApprovalCallout(cards: RwSignal<Option<Vec<TaskCard>>>) -> impl IntoView {
    let ctx = use_app();
    // (task, title, how many tasks wait)
    let waiting = Memo::new(move |_| {
        cards.with(|c| {
            let waiting: Vec<&TaskCard> = c
                .iter()
                .flatten()
                .filter(|c| c.pending_approvals > 0)
                .collect();
            waiting
                .first()
                .map(|c| (c.task.id.clone(), c.task.title.clone(), waiting.len()))
        })
    });
    // Read at the click, never captured: the callout's task changes in place.
    let review = move |_| {
        if let Some((id, ..)) = waiting.get_untracked() {
            ctx.open_task.set(Some(id));
            ctx.project_view.set(ProjectView::Tasks);
        }
    };
    view! {
        <Show when=move || waiting.with(Option::is_some)>
            <div
                role="status"
                class="flex items-center gap-3 rounded-b-xl border-t border-status-waiting/25 bg-status-waiting/8 px-6 py-3"
            >
                <Shield class="size-4 shrink-0 text-status-waiting" />
                <p class="min-w-0 flex-1 truncate">
                    <span class="font-medium">
                        {move || waiting.with(|w| w.as_ref().map(|(_, t, _)| format!("«{t}»")))}
                    </span>
                    " "
                    <span class="text-muted-foreground">
                        {move || {
                            waiting
                                .with(|w| match w {
                                    Some((_, _, 2)) => {
                                        "attende la tua approvazione · un altro task in attesa".to_owned()
                                    }
                                    Some((_, _, n)) if *n > 2 => {
                                        format!("attende la tua approvazione · altri {} task in attesa", n - 1)
                                    }
                                    _ => "attende la tua approvazione".to_owned(),
                                })
                        }}
                    </span>
                </p>
                <button type="button" class=BTN_OUTLINE_SM on:click=review>
                    "Rivedi"
                    <ArrowRight class="size-3.5 shrink-0" />
                </button>
            </div>
        </Show>
    }
}

/// Security mode of a project for its agents (spec §8.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Config {
    Isolated,
    Trusted,
    /// Trusted, but the configuration changed or cannot be verified: the turns run Isolated.
    Unverified,
}

impl Config {
    fn of(p: &Project) -> Self {
        match (p.config_policy, p.trusted) {
            (ConfigPolicy::Isolated, _) => Self::Isolated,
            (ConfigPolicy::Trusted, true) => Self::Trusted,
            (ConfigPolicy::Trusted, false) => Self::Unverified,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Isolated => "Isolata",
            Self::Trusted => "Attendibile",
            Self::Unverified => "Attendibile, non verificata",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::Isolated => "Non caricano .claude/, .mcp.json né CLAUDE.md come memoria.",
            Self::Trusted => "Caricano la configurazione approvata.",
            Self::Unverified => {
                "La configurazione è cambiata o non si può verificare: i turni girano Isolati."
            }
        }
    }

    fn icon(self, class: &'static str) -> AnyView {
        match self {
            Self::Isolated => view! { <Shield class=class /> }.into_any(),
            Self::Trusted => view! { <ShieldCheck class=class /> }.into_any(),
            Self::Unverified => view! { <ShieldAlert class=class /> }.into_any(),
        }
    }
}

/// Bento card: title, optional description and right slot, then the body.
#[component]
fn BentoCard(
    title: &'static str,
    #[prop(optional)] description: Option<&'static str>,
    #[prop(optional)] aside: Option<AnyView>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    view! {
        <section class=format!("{CARD} {class}")>
            <header class="flex items-start justify-between gap-3 px-4 pt-4 pb-3">
                <div class="min-w-0">
                    <h2 class="text-[13px] font-semibold">{title}</h2>
                    {description.map(|d| view! { <p class="mt-0.5 text-xs text-muted-foreground text-pretty">{d}</p> })}
                </div>
                {aside}
            </header>
            {children()}
        </section>
    }
}

/// Default model, permission mode and security mode of the project's agents.
#[component]
fn ConfigCard(
    project: Memo<Option<Project>>,
    app_model: RwSignal<Option<Option<String>>>,
    #[prop(into)] class: String,
) -> impl IntoView {
    let ctx = use_app();
    let model = move || {
        let own = project.with(|p| p.as_ref().and_then(|p| p.default_model.clone()));
        match (own, app_model.get()) {
            (Some(m), _) => m,
            (None, Some(Some(m))) => format!("{m} (predefinito dell'app)"),
            (None, _) => "Predefinito di Claude Code".to_owned(),
        }
    };
    let mode = Memo::new(move |_| project.with(|p| p.as_ref().map(|p| p.default_permission_mode)));
    let config = Memo::new(move |_| project.with(|p| p.as_ref().map(Config::of)));
    let settings = view! {
        <button
            type="button"
            class=format!("{LINK} shrink-0 text-xs")
            on:click=move |_| ctx.project_view.set(ProjectView::Settings)
        >
            "Impostazioni"
        </button>
    }
    .into_any();
    view! {
        <BentoCard title="Configurazione agenti" aside=settings class=class>
            <dl class="divide-y divide-border px-4 pb-2">
                <div class="flex items-start justify-between gap-4 py-2">
                    <dt class="shrink-0 text-muted-foreground">"Modello predefinito"</dt>
                    <dd class="min-w-0 text-right font-medium [overflow-wrap:anywhere]">{model}</dd>
                </div>
                <div class="flex items-start justify-between gap-4 py-2">
                    <dt class="shrink-0 text-muted-foreground">"Modalità"</dt>
                    <dd class="min-w-0 text-right font-medium" title=move || mode.get().map(mode_help)>
                        {move || mode.get().map(|m| mode_label(m.as_str()))}
                    </dd>
                </div>
                <div class="flex items-start justify-between gap-4 py-2">
                    <dt class="shrink-0 text-muted-foreground">"Configurazione"</dt>
                    <dd class="min-w-0 text-right" data-testid="overview-config">
                        {move || {
                            config
                                .get()
                                .map(|c| {
                                    view! {
                                        <span class="inline-flex items-center gap-1.5 font-medium">
                                            {c.icon("size-3.5 shrink-0")}
                                            {c.label()}
                                        </span>
                                        <p class="mt-0.5 text-xs text-muted-foreground text-pretty">{c.note()}</p>
                                    }
                                })
                        }}
                    </dd>
                </div>
            </dl>
        </BentoCard>
    }
}

/// The file cards of one reply.
#[component]
fn Sections(
    overview: ProjectOverview,
    project: Memo<Option<Project>>,
    app_model: RwSignal<Option<Option<String>>>,
) -> impl IntoView {
    let ProjectOverview {
        branch,
        agents_load_config,
        files,
        mcp_servers,
        claude_agents,
        claude_commands,
        claude_skills,
        ..
    } = overview;
    let of_kind = |kind: ContextFileKind| -> Vec<ContextFile> {
        files.iter().filter(|f| f.kind == kind).cloned().collect()
    };
    let at = |path: &str| files.iter().find(|f| f.path == path).cloned();
    let mcp_file = of_kind(ContextFileKind::Mcp);
    let settings = of_kind(ContextFileKind::Settings);
    let readme = of_kind(ContextFileKind::Readme);
    let missing = format!("non presente su {branch}");

    let instructions = INSTRUCTIONS.map(|path| (path, at(path)));
    let present = instructions.iter().filter(|(_, f)| f.is_some()).count();
    let instructions_view = if present == 0 {
        empty_block(
            view! { <FileText class="size-4" /> }.into_any(),
            format!("Nessun file di istruzioni su {branch}"),
            "Gli agenti partono senza CLAUDE.md, .claude/CLAUDE.md né AGENTS.md. Committane uno sul branch target per dare loro convenzioni e contesto.",
        )
        .into_any()
    } else {
        instructions
            .into_iter()
            .map(|(path, file)| match file {
                Some(file) => view! { <FileBlock file open=true /> }.into_any(),
                None => placeholder(path, missing.clone()).into_any(),
            })
            .collect_view()
            .into_any()
    };

    let servers = mcp_servers.len();
    let mcp_view = if mcp_servers.is_empty() && mcp_file.is_empty() {
        view! {
            <div class="px-4 pb-4">
                {empty_block(
                    view! { <Server class="size-4" /> }.into_any(),
                    format!("Nessun .mcp.json su {branch}"),
                    "I server aggiunti con claude mcp add stanno in ~/.claude.json e qui non compaiono.",
                )}
            </div>
        }
        .into_any()
    } else {
        let isolated = (!agents_load_config && !mcp_servers.is_empty())
            .then_some("Con la configurazione Isolata gli agenti non avviano questi server. ");
        view! {
            <div class="space-y-2 px-4">
                {mcp_servers.is_empty()
                    .then(|| {
                        view! { <p class="text-xs text-muted-foreground">"Nessun server in .mcp.json."</p> }
                    })}
                {mcp_servers.into_iter().map(server_card).collect_view()}
                {files_view(mcp_file, false)}
            </div>
            <p class="px-4 pt-3 pb-4 text-xs text-muted-foreground text-pretty">
                {isolated}
                "Quelli aggiunti con "
                <code class="font-mono">"claude mcp add"</code>
                " stanno in ~/.claude.json, che l'app non legge mai: qui non compaiono."
            </p>
        }
        .into_any()
    };

    let has_claude_dir = !settings.is_empty()
        || !claude_agents.is_empty()
        || !claude_commands.is_empty()
        || !claude_skills.is_empty();
    let claude_view = if has_claude_dir {
        view! {
            {(!settings.is_empty())
                .then(|| view! { <div class="space-y-2 px-4 pb-1">{files_view(settings, false)}</div> })}
            <dl class="divide-y divide-border px-4 pb-2">
                {names_row("Agenti", "agents", "Nessun agente", claude_agents)}
                {names_row("Comandi", "commands", "Nessun comando", claude_commands)}
                {names_row("Skill", "skills", "Nessuna skill", claude_skills)}
            </dl>
            {(!agents_load_config)
                .then(|| {
                    view! {
                        <p class="px-4 pb-4 text-xs text-muted-foreground text-pretty">
                            "Con la configurazione Isolata gli agenti non caricano .claude/."
                        </p>
                    }
                })}
        }
        .into_any()
    } else {
        view! {
            <div class="px-4 pb-4">
                {placeholder(".claude/", "nessuna impostazione, agente, comando o skill".to_owned())}
            </div>
        }
        .into_any()
    };

    let readme_view = if readme.is_empty() {
        placeholder("README.md", missing).into_any()
    } else {
        files_view(readme, false)
    };
    let count_badge = |text: String| view! { <span class=OUTLINE_BADGE>{text}</span> }.into_any();

    view! {
        <div class="grid grid-cols-12 gap-4">
            <BentoCard
                title="Istruzioni agenti"
                description="CLAUDE.md, .claude/CLAUDE.md e AGENTS.md, con il modo in cui gli agenti li ricevono."
                aside=view! {
                    <span class="shrink-0 text-xs text-muted-foreground">{format!("{present} di 3 presenti")}</span>
                }
                    .into_any()
                class="col-span-7 row-span-2"
            >
                <div class="space-y-2 px-4 pb-4">{instructions_view}</div>
            </BentoCard>
            <ConfigCard project app_model class="col-span-5" />
            <BentoCard
                title="Server MCP"
                description="Da .mcp.json: solo i nomi delle chiavi, mai i valori."
                aside=count_badge(servers.to_string())
                class="col-span-5"
            >
                {mcp_view}
            </BentoCard>
            <BentoCard
                title="README"
                description="Solo contesto per te: non viene passato agli agenti."
                class="col-span-7"
            >
                <div class="space-y-2 px-4 pb-4">{readme_view}</div>
            </BentoCard>
            <BentoCard
                title=".claude/"
                description="Impostazioni, agenti, comandi e skill del repository."
                class="col-span-5"
            >
                {claude_view}
            </BentoCard>
        </div>
    }
}

fn files_view(files: Vec<ContextFile>, open: bool) -> AnyView {
    files
        .into_iter()
        .map(|file| view! { <FileBlock file open /> })
        .collect_view()
        .into_any()
}

/// A file of the list the repository does not have: dashed, where its block would be.
fn placeholder(path: &'static str, what: String) -> impl IntoView {
    view! {
        <div class=PLACEHOLDER>
            <span class="-ml-1 size-6 shrink-0"></span>
            <span class="shrink-0 font-mono text-xs">{path}</span>
            <span class="min-w-0 truncate text-xs">{what}</span>
        </div>
    }
}

/// A card body with nothing to show: icon, title and what that means.
fn empty_block(icon: AnyView, title: String, text: &'static str) -> impl IntoView {
    view! {
        <div class="flex flex-col items-center rounded-lg border border-dashed border-border px-6 py-7 text-center">
            <span class="grid size-9 place-items-center rounded-lg bg-muted text-muted-foreground">{icon}</span>
            <p class="mt-3 text-[13px] font-medium">{title}</p>
            <p class="mt-1 max-w-[46ch] text-xs text-muted-foreground text-pretty">{text}</p>
        </div>
    }
}

/// One file: path, how the agents get it, a badge for replaced hidden characters, size, then
/// the usage note; the text, when there is one, in a collapsible block that previews five
/// lines and expands in full with "Mostra tutto". `.mcp.json` shows no text: its servers are
/// listed with key names only, and the raw file may hold secrets. The chevron says whether the
/// text is expanded and which element it controls; collapsed, the text stays in the DOM but
/// `inert` (out of the accessibility tree and the focus order).
#[component]
fn FileBlock(file: ContextFile, open: bool) -> impl IntoView {
    let ContextFile {
        path,
        kind,
        size,
        content,
        note,
        hidden_chars,
        used_by_agents,
        usage_note,
    } = file;
    let content = content.filter(|_| kind != ContextFileKind::Mcp);
    let head = view! {
        <span class="max-w-1/2 shrink-0 truncate font-mono text-xs font-medium" title=path.clone()>
            {path.clone()}
        </span>
        <span class=OUTLINE_BADGE>
            {if used_by_agents { "caricato dagli agenti" } else { "non caricato" }}
        </span>
        {hidden_chars
            .then(|| {
                view! {
                    <span
                        class="inline-flex h-5 min-w-0 shrink-[4] items-center gap-1 rounded-md bg-destructive/10 px-1.5 text-[11px] font-medium whitespace-nowrap text-destructive"
                        title="Caratteri invisibili o bidirezionali, mostrati come ⟨U+XXXX⟩"
                        data-hidden-chars=""
                    >
                        <TriangleAlert class="size-3 shrink-0" />
                        // In a narrow card the badge gives way before the file name.
                        <span class="truncate">"caratteri nascosti"</span>
                    </span>
                }
            })}
        <span class="ml-auto shrink-0 font-mono text-[11px] text-muted-foreground tabular-nums">
            {size_label(size)}
        </span>
    };
    let details = view! {
        <p class="-mt-1 px-3 pb-2 text-[11px] leading-4 text-muted-foreground text-pretty">
            {usage_note}
            {note.map(|n| format!(" · {n}"))}
        </p>
    };
    let body = match content {
        Some(text) => {
            let expanded = RwSignal::new(open);
            let text_id = file_text_id(&path);
            let pre_id = format!("{text_id}-pre");
            let label = format!("Testo di {path}");
            let lines = text.lines().count();
            // Past four lines, or a long line wrapping past them, the preview cuts it; a shorter
            // text shows whole.
            let long = lines > 4 || text.chars().count() > 320;
            let full = RwSignal::new(!long);
            view! {
                <Collapsible open=expanded>
                    <div class="flex h-9 min-w-0 items-center gap-2 px-3">
                        <CollapsibleTrigger
                            class="-ml-1 inline-grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
                            attr:aria-expanded=move || expanded.get().to_string()
                            attr:aria-controls=text_id.clone()
                            attr:aria-label=label
                        >
                            <span class=move || {
                                tw_merge!(
                                    "transition-transform", if expanded.get() { "rotate-90" } else { "" }
                                )
                            }>
                                <ChevronRight class="size-3.5" />
                            </span>
                        </CollapsibleTrigger>
                        {head}
                    </div>
                    {details}
                    // Borders inside the animated box would stay visible when collapsed.
                    <CollapsibleContent
                        attr:id=text_id
                        attr:inert=move || (!expanded.get()).then_some("")
                    >
                        <div class="border-t border-border">
                            <div class="relative">
                                <pre
                                    id=pre_id.clone()
                                    class=move || if full.get() { PRE_FULL } else { PRE_PREVIEW }
                                    tabindex=move || (long && full.get()).then_some("0")
                                >
                                    {text}
                                </pre>
                                {long
                                    .then(|| {
                                        view! {
                                            <div class=move || {
                                                if full.get() {
                                                    "hidden"
                                                } else {
                                                    "pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-linear-to-b from-transparent to-background"
                                                }
                                            }></div>
                                        }
                                    })}
                            </div>
                            <div class="flex h-8 items-center justify-between border-t border-border px-3 text-xs">
                                {long
                                    .then(|| {
                                        view! {
                                            <button
                                                type="button"
                                                class=LINK
                                                aria-expanded=move || full.get().to_string()
                                                aria-controls=pre_id
                                                on:click=move |_| full.update(|f| *f = !*f)
                                            >
                                                {move || if full.get() { "Mostra meno" } else { "Mostra tutto" }}
                                            </button>
                                        }
                                    })}
                                <span class="ml-auto font-mono text-[11px] text-muted-foreground tabular-nums">
                                    {lines_label(lines)}
                                </span>
                            </div>
                        </div>
                    </CollapsibleContent>
                </Collapsible>
            }
            .into_any()
        }
        None => view! {
            <div class="flex h-9 min-w-0 items-center gap-2 px-3">
                <span class="-ml-1 size-6 shrink-0"></span>
                {head}
            </div>
            {details}
        }
        .into_any(),
    };
    view! {
        <div class="overflow-hidden rounded-lg border border-border bg-background" data-overview-file=path>
            {body}
        </div>
    }
}

/// One server of `.mcp.json`: name, transport, command or URL, and the names of its env and
/// header keys.
fn server_card(s: McpServer) -> impl IntoView {
    let has_keys = !s.env_keys.is_empty() || !s.header_keys.is_empty();
    let keys = [("env", s.env_keys), ("header", s.header_keys)]
        .into_iter()
        .flat_map(|(what, keys)| keys.into_iter().map(move |k| (what, k)))
        .map(|(what, key)| {
            view! {
                <span class=format!("{NAME_CHIP} text-muted-foreground") title=format!("chiave {what}")>
                    {key}
                </span>
            }
        })
        .collect_view();
    let (name, target) = (s.name.clone(), s.target.clone());
    let target_class = if has_keys {
        "truncate px-3 pt-1 font-mono text-[11px] text-muted-foreground"
    } else {
        "truncate px-3 pt-1 pb-2.5 font-mono text-[11px] text-muted-foreground"
    };
    view! {
        <div class="rounded-lg border border-border" data-mcp-server=name>
            <div class="flex min-w-0 items-center gap-2 px-3 pt-2.5">
                <Server class="size-3.5 shrink-0 text-muted-foreground" />
                <span class="min-w-0 truncate font-mono text-xs font-medium">{s.name}</span>
                <span class=format!("ml-auto {OUTLINE_BADGE}")>{s.transport}</span>
            </div>
            <p class=target_class title=target>
                {s.target}
            </p>
            {has_keys.then(|| view! { <div class="flex flex-wrap gap-1 px-3 pt-2 pb-2.5">{keys}</div> })}
        </div>
    }
}

/// `label` with its count and the names under `.claude/<dir>`, or `none`.
fn names_row(
    label: &'static str,
    dir: &'static str,
    none: &'static str,
    names: Vec<String>,
) -> impl IntoView {
    let count = names.len();
    let names = names
        .into_iter()
        .map(|n| view! { <span class=NAME_CHIP>{n}</span> })
        .collect_view();
    view! {
        <div class="flex items-start justify-between gap-4 py-2">
            <dt class="shrink-0 text-muted-foreground">
                {label}
                " "
                <span class="font-mono tabular-nums">{count}</span>
            </dt>
            <dd class="flex min-w-0 flex-wrap justify-end gap-1" data-claude-dir=dir>
                {(count == 0).then(|| view! { <span class="text-xs text-muted-foreground">{none}</span> })}
                {names}
            </dd>
        </div>
    }
}

/// Id of a file block's collapsible text: one per path in a reply (the paths are fixed and
/// distinct, spec F3).
fn file_text_id(path: &str) -> String {
    let slug: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("overview-text-{slug}")
}

pub(crate) fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// `118 B`, `3 KiB`, `89 KiB`.
fn size_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{} KiB", bytes.div_ceil(1024))
    }
}

/// `1 riga`, `412 righe`.
fn lines_label(lines: usize) -> String {
    if lines == 1 {
        "1 riga".to_owned()
    } else {
        format!("{lines} righe")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_commits_are_short() {
        assert_eq!(size_label(118), "118 B");
        assert_eq!(size_label(1024), "1 KiB");
        assert_eq!(size_label(91_136), "89 KiB");
        assert_eq!(size_label(91_137), "90 KiB");
        assert_eq!(short_commit("3f2a9c1e5b7d"), "3f2a9c1");
        assert_eq!(short_commit("abc"), "abc");
        assert_eq!(lines_label(1), "1 riga");
        assert_eq!(lines_label(412), "412 righe");
    }

    #[test]
    fn file_text_ids_are_distinct_and_plain() {
        let ids: Vec<String> = [
            "CLAUDE.md",
            ".claude/CLAUDE.md",
            "AGENTS.md",
            "README.md",
            ".claude/settings.json",
            ".mcp.json",
        ]
        .into_iter()
        .map(file_text_id)
        .collect();
        assert_eq!(ids[1], "overview-text--claude-CLAUDE-md");
        let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(distinct.len(), ids.len());
    }
}
