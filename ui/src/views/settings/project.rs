//! Project settings page, the project's "Impostazioni" tab (spec F2): name, description and
//! defaults (`update_project`), security (`set_project_security`; the native confirmation is
//! the shell's, M6) and "Rimuovi dalla lista…" (the sidebar's `RemoveProjectDialog`).

use atm_types::{
    AppError, ConfigPolicy, ListBranches, ListProjects, MAX_PROJECT_DESCRIPTION, PermissionMode,
    Project, ProjectIdReq, SetProjectSecurity, SetProjectSecurityReq, UpdateProject,
    UpdateProjectReq,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use super::{Checkbox, Field, Select, models, non_empty, owned};
use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::input::Input;
use crate::ui::separator::Separator;
use crate::ui::textarea::Textarea;
use crate::views::start_dialog::mode_help;

/// Permission modes with their UI names (spec D6).
const MODES: &[(&str, &str)] = &[
    ("default", "Supervisionato"),
    ("acceptEdits", "Auto-edit"),
    ("bypassPermissions", "Autonomo"),
];
const POLICIES: &[(&str, &str)] = &[
    ("isolated", "Isolata (consigliata)"),
    ("trusted", "Attendibile"),
];

/// Settings of the selected project. Mounted fresh on each visit of the page (the `Layout`
/// shows it behind a memoized `Show`), never rebuilt in place: Leptos 0.8 would keep the old
/// buttons' click handlers. The fill Effect tracks only `AppCtx::project`, which the sidebar
/// menu's "Impostazioni progetto" may switch while the page is shown.
#[component]
pub fn ProjectSettings() -> impl IntoView {
    let ctx = use_app();
    let name = RwSignal::new(String::new());
    let description = RwSignal::new(String::new());
    let branch = RwSignal::new(String::new());
    let branches = RwSignal::new(Vec::<(String, String)>::new());
    let mode = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let policy = RwSignal::new(String::new());
    let allow_bypass = RwSignal::new(false);
    // The trust in effect as the backend computes it (`Project::trusted`): `Some` only while
    // the saved policy is Trusted.
    let trust = RwSignal::new(None::<bool>);
    // Why the configuration cannot be checked (`Project::trust_error`).
    let trust_error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let fill = move |p: &Project| {
        name.set(p.name.clone());
        description.set(p.description.clone());
        branch.set(p.default_target_branch.clone());
        mode.set(p.default_permission_mode.as_str().to_owned());
        model.set(p.default_model.clone().unwrap_or_default());
        policy.set(p.config_policy.as_str().to_owned());
        allow_bypass.set(p.allow_bypass);
        trust.set((p.config_policy == ConfigPolicy::Trusted).then_some(p.trusted));
        trust_error.set(p.trust_error.clone());
    };
    // The replies below still concern the project on screen.
    let still = move |id: &str| ctx.project.try_get_untracked().flatten().as_deref() == Some(id);
    Effect::new(move |_| {
        let Some(id) = ctx.project.get() else {
            return;
        };
        if let Some(p) = ctx
            .projects
            .with_untracked(|ps| ps.iter().find(|p| p.id == id).cloned())
        {
            fill(&p);
        }
        // The cached list follows project events, and an edit of `.claude/` on disk sends none:
        // the trust in effect is read again on each visit of the page, for this project only.
        let fresh = id.clone();
        spawn_local(async move {
            let Ok(projects) = ipc::call::<ListProjects>(&Default::default()).await else {
                return;
            };
            if let Some(p) = projects
                .iter()
                .find(|p| p.id == fresh)
                .filter(|_| still(&fresh))
            {
                trust.try_set((p.config_policy == ConfigPolicy::Trusted).then_some(p.trusted));
                trust_error.try_set(p.trust_error.clone());
            }
            ctx.projects.try_set(projects);
        });
        spawn_local(async move {
            match ipc::call::<ListBranches>(&ProjectIdReq {
                project_id: id.clone(),
            })
            .await
            {
                Ok(list) if still(&id) => {
                    branches.try_set(list.branches.into_iter().map(|b| (b.clone(), b)).collect());
                }
                Ok(_) => {}
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    });
    let description_len = Memo::new(move |_| description.with(|d| d.trim().chars().count()));
    let description_over = move || description_len.get() > MAX_PROJECT_DESCRIPTION;

    let save_defaults = move |_| {
        let Some(id) = ctx.project.get_untracked() else {
            return;
        };
        let req = UpdateProjectReq {
            id,
            name: name.get_untracked().trim().to_owned(),
            default_target_branch: branch.get_untracked(),
            default_permission_mode: mode
                .get_untracked()
                .parse()
                .unwrap_or(PermissionMode::AcceptEdits),
            default_model: non_empty(model.get_untracked()),
            description: description.get_untracked(),
        };
        run(
            ctx,
            busy,
            "Progetto aggiornato",
            async move { ipc::call::<UpdateProject>(&req).await },
            drop,
        );
    };
    let save_security = move |_| {
        let Some(id) = ctx.project.get_untracked() else {
            return;
        };
        let req = SetProjectSecurityReq {
            id: id.clone(),
            config_policy: policy
                .get_untracked()
                .parse()
                .unwrap_or(ConfigPolicy::Isolated),
            allow_bypass: allow_bypass.get_untracked(),
        };
        run(
            ctx,
            busy,
            "Sicurezza del progetto aggiornata",
            async move { ipc::call::<SetProjectSecurity>(&req).await },
            // Autonomo is no longer selectable; the backend may also have reset the default.
            // Only for the project still on screen: the sidebar menu may have switched it.
            move |p: Project| {
                if !still(&id) {
                    return;
                }
                if !p.allow_bypass
                    && mode.try_get_untracked().as_deref() == Some("bypassPermissions")
                {
                    mode.try_set(PermissionMode::AcceptEdits.as_str().to_owned());
                }
                trust.try_set((p.config_policy == ConfigPolicy::Trusted).then_some(p.trusted));
                trust_error.try_set(p.trust_error.clone());
            },
        );
    };
    // The shared confirmation dialog removes the project read here, at click time.
    let remove = move |_| {
        let Some(id) = ctx.project.get_untracked() else {
            return;
        };
        let saved = ctx
            .projects
            .with_untracked(|ps| ps.iter().find(|p| p.id == id).map(|p| p.name.clone()));
        let name = saved.unwrap_or_else(|| name.get_untracked());
        ctx.remove_target.set(Some((id, name)));
    };

    // Autonomo only once the project allows bypass (spec §9.2).
    let no_bypass = Signal::derive(move || {
        if allow_bypass.get() {
            Vec::new()
        } else {
            vec!["bypassPermissions".to_owned()]
        }
    });
    view! {
        <div class="mx-auto flex max-w-2xl flex-col gap-4 p-6" data-testid="project-settings">
            <h2 class="text-lg font-semibold">"Impostazioni progetto"</h2>
            <Field id="project-name" label="Nome">
                <Input id="project-name" bind_value=name />
            </Field>
            <Field id="project-description" label="Descrizione">
                <Textarea
                    id="project-description"
                    class="min-h-24"
                    bind_value=description
                    placeholder="A cosa serve il repository, convenzioni, link utili…"
                />
                <div class="text-muted-foreground flex justify-between gap-3 text-xs">
                    <span>"Mostrata nel riepilogo del progetto."</span>
                    <span
                        class=move || if description_over() { "text-destructive tabular-nums" } else { "tabular-nums" }
                        data-testid="project-description-count"
                    >
                        {move || format!("{}/{MAX_PROJECT_DESCRIPTION}", description_len.get())}
                    </span>
                </div>
            </Field>
            <div class="grid grid-cols-2 gap-4">
                <Field id="project-branch" label="Branch target predefinito">
                    <Select id="project-branch" value=branch options=branches />
                </Field>
                <Field id="project-model" label="Modello predefinito">
                    <Select id="project-model" value=model options=models() />
                </Field>
            </div>
            <Field id="project-mode" label="Modalità predefinita">
                <Select id="project-mode" value=mode options=owned(MODES) disabled=no_bypass />
                <p class="text-muted-foreground text-xs">
                    {move || mode.with(|m| m.parse::<PermissionMode>().ok()).map(mode_help)}
                </p>
            </Field>
            <div class="flex justify-end">
                <Button
                    size=ButtonSize::Sm
                    attr:disabled=move || busy.get() || description_over()
                    on:click=save_defaults
                >
                    "Salva il progetto"
                </Button>
            </div>

            <Separator />
            <h4 class="font-medium">"Sicurezza"</h4>
            <Field
                id="project-policy"
                label="Configurazione Claude del repository"
                hint="Isolata: .claude/, .mcp.json, hook, server MCP e regole di permesso del repository non vengono caricati. Attendibile: approvi la configurazione committata sul branch target predefinito, da cui partono i worktree, e gli agenti la caricano finché resta quella; a ogni turno l'app ricontrolla .claude/, .mcp.json e i file del repository che i loro comandi eseguono nel worktree e, se sono cambiati, il turno gira Isolato. Una configurazione che farebbe fatturare gli agenti via API invece che con l'abbonamento (apiKeyHelper, chiavi, endpoint o provider in env) non si può approvare. Revocare Attendibile o la modalità Autonoma ferma i turni in corso che le usano."
            >
                <Select id="project-policy" value=policy options=owned(POLICIES) />
            </Field>
            {move || match trust.get() {
                Some(true) => view! {
                    <p class="text-muted-foreground text-xs" data-trust="trusted">
                        "Approvata la configurazione committata sul branch target predefinito, da cui partono i nuovi worktree. I file non committati del checkout principale (per esempio .claude/settings.local.json) non contano: non arrivano nei worktree."
                    </p>
                }
                .into_any(),
                Some(false) => match trust_error.get() {
                    Some(error) => view! {
                        <Callout variant=CalloutVariant::Warning class="md:mx-0" title="Configurazione non approvabile" attr:data-trust="unverifiable">
                            {format!("La configurazione Claude del branch target non si può verificare o approvare: {error}. I turni girano Isolati finché il problema resta.")}
                        </Callout>
                    }
                    .into_any(),
                    None => view! {
                        <Callout variant=CalloutVariant::Warning class="md:mx-0" title="Configurazione cambiata" attr:data-trust="stale">
                            "La configurazione Claude committata sul branch target è cambiata dopo l'approvazione: gli attempt che partono da lì girano Isolati. «Applica» con Attendibile approva quella attuale, dopo una conferma."
                        </Callout>
                    }
                    .into_any(),
                },
                None => ().into_any(),
            }}
            <Checkbox id="project-bypass" checked=allow_bypass>
                "Consenti la modalità Autonoma (bypassPermissions)"
            </Checkbox>
            <Show when=move || allow_bypass.get() || policy.get() == "trusted">
                <Callout variant=CalloutVariant::Warning class="md:mx-0" title="Il worktree non è una sandbox">
                    "Gli agenti, gli hook e i server MCP del repository girano sul tuo Mac con i tuoi permessi e possono toccare file e servizi fuori dal worktree. L'app chiede una conferma nativa prima di applicare."
                </Callout>
            </Show>
            <div class="flex justify-end">
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Outline
                    attr:data-action="apply-security"
                    attr:disabled=move || busy.get()
                    on:click=save_security
                >
                    "Applica"
                </Button>
            </div>

            <Separator />
            <div class="flex items-center gap-3">
                <p class="text-muted-foreground flex-1 text-xs">
                    "Toglie il progetto dall'app, dopo una conferma: i file del repository e i branch restano."
                </p>
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Destructive
                    attr:data-action="remove-project"
                    attr:disabled=move || busy.get()
                    on:click=remove
                >
                    "Rimuovi dalla lista…"
                </Button>
            </div>
        </div>
    }
}

/// Runs one action: `busy` meanwhile, then `on_ok` with the result, a toast and a reload of
/// the project list.
fn run<T: 'static>(
    ctx: AppCtx,
    busy: RwSignal<bool>,
    done: &'static str,
    call: impl Future<Output = Result<T, AppError>> + 'static,
    on_ok: impl FnOnce(T) + 'static,
) {
    busy.set(true);
    spawn_local(async move {
        match call.await {
            Ok(res) => {
                on_ok(res);
                ctx.toasts.success(done);
                ctx.refresh_projects();
            }
            Err(e) => ctx.toasts.app_error(&e),
        }
        busy.try_set(false);
    });
}
