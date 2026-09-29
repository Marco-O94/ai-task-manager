//! Project overview, the page a project opens on (spec F3, §9.2): description, state, agent
//! instructions, MCP servers, `.claude/`, README, from `get_project_overview`. Owner: UI-SHELL.
//! Root `[data-view=overview]`; files `[data-overview-file=<path>]`, servers
//! `[data-mcp-server=<name>]` (DOM contract, spec §9.2).
//!
//! The overview is fetched on mount, when the selected project changes and with "Aggiorna";
//! the task counts follow `board_version` as the board does. File contents are plain text
//! (`whitespace-pre-wrap`): rendering markdown would need a new dependency and raw HTML, which
//! §10.3 forbids.

use atm_types::{
    AppError, ConfigPolicy, ContextFile, ContextFileKind, Empty, GetBoard, GetProjectOverview,
    GetSettings, Id, McpServer, Project, ProjectIdReq, ProjectOverview, TaskCard, TaskStatus,
};
use icons::{ChevronRight, RefreshCw};
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;

use crate::app::{ProjectView, use_app};
use crate::ipc;
use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::card::{Card, CardContent, CardDescription, CardHeader, CardSize, CardTitle};
use crate::ui::collapsible::{Collapsible, CollapsibleContent, CollapsibleTrigger};
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;
use crate::views::board::column_title;

const PRE_CLASS: &str = "bg-muted max-h-96 overflow-auto rounded-md p-3 font-mono text-xs whitespace-pre-wrap [overflow-wrap:anywhere]";

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
    let refresh = move |_| {
        if let Some(id) = ctx.project.get_untracked() {
            fetch(id);
        }
    };

    // Task counts: `get_board`, again on each `board_version`.
    let cards = RwSignal::new(None::<Vec<TaskCard>>);
    let board_fetches = StoredValue::new(0u64);
    let counted = StoredValue::new(None::<Id>);
    Effect::new(move |_| {
        ctx.board_version.track();
        let Some(project_id) = ctx.project.get() else {
            return;
        };
        if counted.get_value().as_ref() != Some(&project_id) {
            counted.set_value(Some(project_id.clone()));
            cards.set(None);
        }
        let seq = board_fetches
            .try_update_value(|n| {
                *n += 1;
                *n
            })
            .unwrap_or_default();
        spawn_local(async move {
            let res = ipc::call::<GetBoard>(&ProjectIdReq { project_id }).await;
            if board_fetches.try_get_value() != Some(seq) {
                return;
            }
            match res {
                Ok(list) => {
                    cards.try_set(Some(list));
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    });
    let total = move || cards.with(|c| c.as_ref().map(Vec::len));

    view! {
        <section class="mx-auto flex w-full max-w-5xl flex-col gap-4 p-6" data-view="overview">
            <div class="flex flex-wrap items-center gap-2">
                <div class="min-w-0 flex-1">
                    <h2 class="text-lg font-semibold">"Riepilogo"</h2>
                    <p class="text-muted-foreground text-xs">
                        "File letti dal commit in cima al branch target predefinito, da cui partono i worktree degli agenti."
                    </p>
                </div>
                <Button
                    variant=ButtonVariant::Outline
                    size=ButtonSize::Sm
                    attr:data-action="refresh-overview"
                    attr:disabled=move || loading.get()
                    on:click=refresh
                >
                    {move || {
                        if loading.get() {
                            view! { <Spinner class="size-4" /> }.into_any()
                        } else {
                            view! { <RefreshCw /> }.into_any()
                        }
                    }}
                    "Aggiorna"
                </Button>
                <Button
                    size=ButtonSize::Sm
                    attr:data-action="open-tasks"
                    on:click=move |_| ctx.project_view.set(ProjectView::Tasks)
                >
                    {move || match total() {
                        Some(n) => format!("Apri task ({n})"),
                        None => "Apri task".to_owned(),
                    }}
                    <ChevronRight />
                </Button>
            </div>
            // Cards as tall as their content: a short description is not stretched to the
            // height of the Stato card.
            <div class="grid items-start gap-4 lg:grid-cols-2">
                <DescriptionCard project />
                <StateCard project overview cards />
            </div>
            // Keyed by the fetch: each reply mounts its sections afresh instead of patching the
            // previous ones in place.
            <For
                each=move || overview.get()
                key=|(seq, _)| *seq
                children=|(_, res)| match res {
                    Ok(overview) => view! { <Sections overview /> }.into_any(),
                    Err(e) => {
                        view! {
                            <Callout
                                variant=CalloutVariant::Warning
                                class="md:mx-0"
                                title="File del repository non disponibili"
                                attr:data-overview-error=""
                            >
                                {e.message}
                            </Callout>
                        }
                            .into_any()
                    }
                }
            />
            <Show when=move || overview.with(Option::is_none)>
                <div class="grid gap-4 lg:grid-cols-2">
                    <Skeleton class="h-40 lg:col-span-2" />
                    <Skeleton class="h-32" />
                    <Skeleton class="h-32" />
                </div>
            </Show>
        </section>
    }
}

/// Card with a title, an optional description and its content.
#[component]
fn Section(
    title: &'static str,
    #[prop(optional)] description: Option<&'static str>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    view! {
        <Card size=CardSize::Sm class=class>
            <CardHeader>
                <CardTitle class="text-base">{title}</CardTitle>
                {description.map(|d| view! { <CardDescription class="text-xs">{d}</CardDescription> })}
            </CardHeader>
            <CardContent class="flex flex-col gap-3">{children()}</CardContent>
        </Card>
    }
}

/// The project's description, or a way to its settings to write one.
#[component]
fn DescriptionCard(project: Memo<Option<Project>>) -> impl IntoView {
    let ctx = use_app();
    let description = Memo::new(move |_| {
        project.with(|p| {
            p.as_ref()
                .map(|p| p.description.trim().to_owned())
                .unwrap_or_default()
        })
    });
    view! {
        <Section title="Descrizione">
            <Show
                when=move || description.with(|d| !d.is_empty())
                fallback=move || {
                    view! {
                        <p class="text-muted-foreground text-sm">
                            "Nessuna descrizione. "
                            <button
                                type="button"
                                class="text-primary underline-offset-4 hover:underline"
                                data-action="edit-description"
                                on:click=move |_| ctx.project_view.set(ProjectView::Settings)
                            >
                                "Scrivila nelle impostazioni"
                            </button>
                        </p>
                    }
                }
            >
                <p class="text-sm whitespace-pre-wrap [overflow-wrap:anywhere]" data-testid="project-description">
                    {move || description.get()}
                </p>
            </Show>
        </Section>
    }
}

/// Task counts, running agents, branch and commit read, default model, repository
/// configuration.
#[component]
fn StateCard(
    project: Memo<Option<Project>>,
    overview: RwSignal<Option<Fetched>>,
    cards: RwSignal<Option<Vec<TaskCard>>>,
) -> impl IntoView {
    let ctx = use_app();
    // The app default, for a project without its own model.
    let app_model = RwSignal::new(None::<Option<String>>);
    spawn_local(async move {
        if let Ok(s) = ipc::call::<GetSettings>(&Empty {}).await {
            app_model.try_set(Some(s.default_model));
        }
    });

    let count = move |status: TaskStatus| {
        cards.with(|c| {
            c.as_ref().map_or_else(
                || "–".to_owned(),
                |c| {
                    c.iter()
                        .filter(|card| card.task.status == status)
                        .count()
                        .to_string()
                },
            )
        })
    };
    let running = move || {
        let here = cards.with(|c| c.as_ref().map(|c| c.iter().filter(|c| c.running).count()));
        let app = ctx.env.with(|e| {
            e.as_ref()
                .map(|e| format!("{}/{} nell'app", e.running, e.max_running))
        });
        [here.map(|n| format!("{n} in questo progetto")), app]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let branch = move || {
        let read = overview.with(|o| match o {
            Some((_, Ok(o))) => Some(format!("{} @ {}", o.branch, short_commit(&o.commit))),
            _ => None,
        });
        read.or_else(|| project.with(|p| p.as_ref().map(|p| p.default_target_branch.clone())))
    };
    let model = move || {
        let own = project.with(|p| p.as_ref().and_then(|p| p.default_model.clone()));
        match (own, app_model.get()) {
            (Some(m), _) => m,
            (None, Some(Some(m))) => format!("{m} (predefinito dell'app)"),
            (None, _) => "Predefinito di Claude Code".to_owned(),
        }
    };
    let config = move || project.with(|p| p.as_ref().map(config_label));
    view! {
        <Section title="Stato">
            <div class="grid grid-cols-5 gap-2" data-testid="overview-counts">
                {TaskStatus::ALL
                    .iter()
                    .map(|&status| {
                        view! {
                            <div class="bg-muted/60 min-w-0 rounded-md px-2 py-1.5" data-count=status.as_str()>
                                <div class="text-muted-foreground truncate text-xs">{column_title(status)}</div>
                                <div class="text-lg font-semibold tabular-nums">{move || count(status)}</div>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
            <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
                <dt class="text-muted-foreground">"Agenti attivi"</dt>
                <dd data-testid="overview-running">{running}</dd>
                <dt class="text-muted-foreground">"Branch target"</dt>
                <dd class="font-mono text-xs leading-5" data-testid="overview-branch">{branch}</dd>
                <dt class="text-muted-foreground">"Modello predefinito"</dt>
                <dd>{model}</dd>
                <dt class="text-muted-foreground">"Configurazione"</dt>
                <dd data-testid="overview-config">{config}</dd>
            </dl>
        </Section>
    }
}

/// What the security mode means for the agents (spec §8.9).
fn config_label(p: &Project) -> String {
    match (p.config_policy, p.trusted) {
        (ConfigPolicy::Isolated, _) => {
            "Isolata: gli agenti non caricano .claude/, .mcp.json né CLAUDE.md come memoria".into()
        }
        (ConfigPolicy::Trusted, true) => {
            "Attendibile: gli agenti caricano la configurazione approvata".into()
        }
        (ConfigPolicy::Trusted, false) => {
            "Attendibile, ma la configurazione è cambiata o non si può verificare: i turni girano Isolati"
                .into()
        }
    }
}

/// The file sections of one reply.
#[component]
fn Sections(overview: ProjectOverview) -> impl IntoView {
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
    let of_kind = |kinds: &[ContextFileKind]| -> Vec<ContextFile> {
        files
            .iter()
            .filter(|f| kinds.contains(&f.kind))
            .cloned()
            .collect()
    };
    let instructions = of_kind(&[ContextFileKind::Memory, ContextFileKind::Agents]);
    let mcp_file = of_kind(&[ContextFileKind::Mcp]);
    let settings = of_kind(&[ContextFileKind::Settings]);
    let readme = of_kind(&[ContextFileKind::Readme]);
    // What Isolated leaves out, said once per section with something to leave out.
    let isolated = move |note: &'static str| {
        (!agents_load_config).then(|| view! { <p class="text-muted-foreground text-xs">{note}</p> })
    };
    let has_claude_dir = !settings.is_empty()
        || !claude_agents.is_empty()
        || !claude_commands.is_empty()
        || !claude_skills.is_empty();

    let instructions_view = if instructions.is_empty() {
        view! {
            <p class="text-muted-foreground text-sm">
                {format!(
                    "Nessun CLAUDE.md né AGENTS.md su {branch}: gli agenti partono solo dal prompt del task.",
                )}
            </p>
        }
        .into_any()
    } else {
        files_view(instructions, true)
    };
    let mcp_view = view! {
        {if mcp_servers.is_empty() {
            view! { <p class="text-muted-foreground text-sm">"Nessun server in .mcp.json."</p> }
                .into_any()
        } else {
            view! {
                {servers_table(mcp_servers)}
                {isolated("Con la configurazione Isolata gli agenti non avviano questi server.")}
            }
                .into_any()
        }}
        {files_view(mcp_file, false)}
        <p class="text-muted-foreground text-xs">
            "I server aggiunti con «claude mcp add» stanno in ~/.claude.json, che l'app non legge mai: qui non compaiono."
        </p>
    };
    let claude_view = if has_claude_dir {
        view! {
            {files_view(settings, false)}
            <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
                {names_row("Agenti", "agents", claude_agents)}
                {names_row("Comandi", "commands", claude_commands)}
                {names_row("Skill", "skills", claude_skills)}
            </dl>
            {isolated("Con la configurazione Isolata gli agenti non caricano .claude/.")}
        }
        .into_any()
    } else {
        view! {
            <p class="text-muted-foreground text-sm">{format!("Nessuna impostazione, agente, comando o skill in .claude/ su {branch}.")}</p>
        }
        .into_any()
    };
    let readme_view = if readme.is_empty() {
        view! { <p class="text-muted-foreground text-sm">"Nessun README.md."</p> }.into_any()
    } else {
        files_view(readme, false)
    };
    view! {
        <div class="grid items-start gap-4 lg:grid-cols-2">
            <Section
                title="Istruzioni agenti"
                description="CLAUDE.md, .claude/CLAUDE.md e AGENTS.md, con il modo in cui gli agenti li ricevono."
                class="lg:col-span-2"
            >
                {instructions_view}
            </Section>
            <Section
                title="Server MCP"
                description="Da .mcp.json: solo i nomi delle chiavi di env e header, mai i valori."
            >
                {mcp_view}
            </Section>
            <Section
                title=".claude/"
                description="Impostazioni, agenti, comandi e skill del repository."
            >
                {claude_view}
            </Section>
            <Section title="README" class="lg:col-span-2">
                {readme_view}
            </Section>
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

/// One file: path, how the agents get it, size, the note and a badge for replaced hidden
/// characters; the text, when there is one, in a collapsible block. `.mcp.json` shows no text:
/// its servers are listed with key names only, and the raw file may hold secrets. The trigger
/// says whether the text is expanded and which element it controls; collapsed, the text stays
/// in the DOM but `inert` (out of the accessibility tree and the focus order).
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
    let badges = view! {
        <span class="min-w-0 flex-1 truncate font-mono text-xs font-medium" title=path.clone()>
            {path.clone()}
        </span>
        {hidden_chars
            .then(|| {
                view! {
                    <span title="Caratteri invisibili o bidirezionali, mostrati come ⟨U+XXXX⟩">
                        <Badge variant=BadgeVariant::Warning attr:data-hidden-chars="">
                            "caratteri nascosti"
                        </Badge>
                    </span>
                }
            })}
        <Badge variant=if used_by_agents { BadgeVariant::Success } else { BadgeVariant::Muted }>
            {if used_by_agents { "caricato dagli agenti" } else { "non caricato" }}
        </Badge>
        <span class="text-muted-foreground shrink-0 text-xs tabular-nums">{size_label(size)}</span>
    };
    let details = view! {
        <p class="text-muted-foreground px-3 pb-2 text-xs">
            {usage_note}
            {note.map(|n| format!(" · {n}"))}
        </p>
    };
    let body = match content {
        Some(text) => {
            let expanded = RwSignal::new(open);
            let text_id = file_text_id(&path);
            view! {
                <Collapsible open=expanded>
                    <CollapsibleTrigger
                        class="hover:bg-accent/50 flex w-full min-w-0 items-center gap-2 rounded-t-lg px-3 py-2 text-left text-sm"
                        attr:aria-expanded=move || expanded.get().to_string()
                        attr:aria-controls=text_id.clone()
                    >
                        <span class=move || {
                            tw_merge!(
                                "shrink-0 transition-transform", if expanded.get() { "rotate-90" } else { "" }
                            )
                        }>
                            <ChevronRight class="size-4" />
                        </span>
                        {badges}
                    </CollapsibleTrigger>
                    {details}
                    // Padding inside the animated box would stay visible when collapsed.
                    <CollapsibleContent
                        attr:id=text_id
                        attr:inert=move || (!expanded.get()).then_some("")
                    >
                        <div class="px-3 pb-3">
                            <pre class=PRE_CLASS>{text}</pre>
                        </div>
                    </CollapsibleContent>
                </Collapsible>
            }
            .into_any()
        }
        None => view! {
            <div class="flex min-w-0 items-center gap-2 px-3 py-2 text-sm">{badges}</div>
            {details}
        }
        .into_any(),
    };
    view! {
        <div class="rounded-lg border" data-overview-file=path>
            {body}
        </div>
    }
}

fn servers_table(servers: Vec<McpServer>) -> impl IntoView {
    let rows = servers
        .into_iter()
        .map(|s| {
            let keys = [("env", s.env_keys), ("header", s.header_keys)]
                .into_iter()
                .flat_map(|(what, keys)| keys.into_iter().map(move |k| (what, k)))
                .map(|(what, key)| {
                    view! {
                        <span class="bg-muted rounded px-1 font-mono text-xs" title=format!("chiave {what}")>
                            {key}
                        </span>
                    }
                })
                .collect_view();
            let row = s.name.clone();
            view! {
                <tr class="border-t align-top" data-mcp-server=row>
                    <td class="py-1.5 pr-3 font-mono text-xs font-medium">{s.name}</td>
                    <td class="py-1.5 pr-3 text-xs">{s.transport}</td>
                    <td class="py-1.5 pr-3 font-mono text-xs [overflow-wrap:anywhere]">{s.target}</td>
                    <td class="py-1.5">
                        <div class="flex flex-wrap gap-1">{keys}</div>
                    </td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <div class="overflow-x-auto">
            <table class="w-full text-left text-sm">
                <thead class="text-muted-foreground text-xs">
                    <tr>
                        <th class="pr-3 pb-1 font-medium">"Nome"</th>
                        <th class="pr-3 pb-1 font-medium">"Trasporto"</th>
                        <th class="pr-3 pb-1 font-medium">"Comando o URL"</th>
                        <th class="pb-1 font-medium">"Chiavi"</th>
                    </tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
}

/// `label` and the names under `.claude/<dir>`, or "nessuno".
fn names_row(label: &'static str, dir: &'static str, names: Vec<String>) -> impl IntoView {
    let empty = names.is_empty();
    let names = names
        .into_iter()
        .map(|n| view! { <span class="bg-muted rounded px-1 font-mono text-xs">{n}</span> })
        .collect_view();
    view! {
        <dt class="text-muted-foreground">{label}</dt>
        <dd class="flex flex-wrap gap-1" data-claude-dir=dir>
            {empty.then(|| view! { <span class="text-muted-foreground">"nessuno"</span> })}
            {names}
        </dd>
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

fn short_commit(commit: &str) -> &str {
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
