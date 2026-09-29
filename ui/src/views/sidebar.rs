//! Sidebar (projects, "Aggiungi repository", settings) and topbar (account chip, pause banner
//! with "Riprendi", running counter), spec §9.2. Owner: M2-UI-BOARD.

use atm_types::{
    AddProject, AddProjectReq, AuthState, Empty, EnvStatus, GetSettings, PickRepoFolder,
    ResumeAgents,
};
use icons::{FolderGit2, FolderPlus, Settings as SettingsIcon, TriangleAlert};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::scroll_area::ScrollArea;
use crate::ui::separator::Separator;
use crate::ui::spinner::Spinner;
use crate::ui::tooltip::{Tooltip, TooltipContent, TooltipPosition};
use crate::views::settings::SettingsDialog;

/// Left column, 240 px.
#[component]
pub fn Sidebar() -> impl IntoView {
    let ctx = use_app();
    let settings_open = RwSignal::new(false);
    let loaded = projects_loaded(ctx);
    view! {
        <nav class="bg-sidenav flex w-60 shrink-0 flex-col border-r" data-view="sidebar">
            <div class="flex h-12 shrink-0 items-center px-4 font-semibold">"AI Task Manager"</div>
            <Separator />
            <p class="text-muted-foreground px-4 pt-4 pb-2 text-xs font-medium tracking-wide uppercase">
                "Progetti"
            </p>
            <ScrollArea class="min-h-0 flex-1">
                <ul class="flex flex-col gap-0.5 px-2" data-testid="projects">
                    <For
                        each=move || ctx.projects.get()
                        key=|p| (p.id.clone(), p.name.clone())
                        let:project
                    >
                        <ProjectItem id=project.id name=project.name repo_path=project.repo_path />
                    </For>
                </ul>
                <Show when=move || loaded.get() && ctx.projects.with(Vec::is_empty)>
                    <p class="text-muted-foreground px-4 text-xs">"Nessun progetto."</p>
                </Show>
            </ScrollArea>
            <Separator />
            <div class="flex flex-col gap-1 p-2">
                <Button
                    variant=ButtonVariant::Ghost
                    class="w-full justify-start"
                    on:click=move |_| add_repository(ctx)
                >
                    <FolderPlus />
                    "Aggiungi repository"
                </Button>
                <Button
                    variant=ButtonVariant::Ghost
                    class="w-full justify-start"
                    on:click=move |_| settings_open.set(true)
                >
                    <SettingsIcon />
                    "Impostazioni"
                </Button>
            </div>
            <SettingsDialog open=settings_open />
        </nav>
    }
}

#[component]
fn ProjectItem(id: String, name: String, repo_path: String) -> impl IntoView {
    let ctx = use_app();
    let selected = {
        let id = id.clone();
        Memo::new(move |_| ctx.project.with(|p| p.as_deref() == Some(id.as_str())))
    };
    let class = move || {
        if selected.get() {
            "bg-accent text-accent-foreground w-full justify-start font-medium"
        } else {
            "text-muted-foreground w-full justify-start font-normal"
        }
    };
    view! {
        <li>
            <Button
                variant=ButtonVariant::Ghost
                size=ButtonSize::Sm
                class=Signal::derive(move || class().to_owned())
                attr:title=repo_path
                attr:aria-current=move || selected.get().then_some("true")
                on:click=move |_| {
                    if !selected.get_untracked() {
                        ctx.open_task.set(None);
                        ctx.project.set(Some(id.clone()));
                    }
                }
            >
                <FolderGit2 />
                <span class="truncate">{name}</span>
            </Button>
        </li>
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
                ctx.open_task.try_set(None);
                ctx.project.try_set(Some(res.project.id));
                ctx.refresh_projects();
            }
            Err(e) => ctx.toasts.app_error(&e),
        }
    });
}

/// Bar above the board and the task panel, with the environment banners under it.
#[component]
pub fn Topbar() -> impl IntoView {
    let ctx = use_app();
    let project = Memo::new(move |_| {
        let id = ctx.project.get()?;
        ctx.projects
            .with(|ps| ps.iter().find(|p| p.id == id).cloned())
    });
    view! {
        <div class="shrink-0" data-view="topbar">
            <header class="flex h-12 items-center gap-3 border-b px-4">
                <div class="flex min-w-0 flex-1 items-baseline gap-2">
                    {move || {
                        project
                            .get()
                            .map(|p| {
                                view! {
                                    <h1 class="truncate font-semibold">{p.name}</h1>
                                    <span class="text-muted-foreground truncate font-mono text-xs">
                                        {p.repo_path}
                                    </span>
                                }
                            })
                    }}
                </div>
                {move || ctx.env.get().map(|env| view! { <EnvChips env /> })}
            </header>
            <Banners />
        </div>
    }
}

/// Running counter, version badge and account chip.
#[component]
fn EnvChips(env: EnvStatus) -> impl IntoView {
    let running = env.running;
    let untested = env
        .claude
        .version
        .clone()
        .filter(|v| version_newer(v, &env.claude.tested_version));
    let (chip, details) = match &env.auth {
        AuthState::LoggedIn {
            auth_method,
            api_provider,
            email,
            org_name,
            subscription_type,
        } => {
            let chip = [email.as_deref(), subscription_type.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let details = [
                auth_method.as_deref().map(|m| format!("Accesso: {m}")),
                api_provider.as_deref().map(|p| format!("Provider: {p}")),
                org_name.as_deref().map(|o| format!("Organizzazione: {o}")),
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
        <div class="flex shrink-0 items-center gap-2">
            <Badge variant=BadgeVariant::Outline class="gap-1.5 font-medium" attr:data-testid="running">
                {(running > 0).then(|| view! { <Spinner class="size-3" /> })}
                {format!("In esecuzione {running}/{}", env.max_running)}
            </Badge>
            {untested
                .map(|v| {
                    view! {
                        <Badge variant=BadgeVariant::Info attr:title="Più recente della versione testata">
                            {format!("Claude Code {v}")}
                        </Badge>
                    }
                })}
            <Tooltip>
                <Badge variant=BadgeVariant::Secondary class="max-w-64 truncate" attr:data-testid="account">
                    {chip}
                </Badge>
                {(!details.is_empty())
                    .then(|| {
                        view! { <TooltipContent position=TooltipPosition::Left>{details}</TooltipContent> }
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
            class="bg-warning-light dark:bg-warning-dark/20 flex items-center gap-3 border-b px-4 py-2 text-sm"
            role="status"
            data-banner=testid
        >
            <TriangleAlert class="text-warning size-4 shrink-0" />
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
