//! "Progetto" tab of the settings dialog: defaults (`update_project`), security
//! (`set_project_security`; the native confirmation is the shell's, M6) and removal.

use atm_types::{
    AppError, ConfigPolicy, IdReq, ListBranches, PermissionMode, Project, ProjectIdReq,
    RemoveProject, SetProjectSecurity, SetProjectSecurityReq, UpdateProject, UpdateProjectReq,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use super::{Checkbox, Field, MODELS, Select, non_empty, owned};
use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::input::Input;
use crate::ui::separator::Separator;

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

/// Settings of the selected project, refilled whenever the dialog opens or the selection
/// changes. One instance for the dialog's lifetime: re-creating it per project would rebuild
/// its buttons in place, and Leptos 0.8 then keeps the old instance's click handlers.
#[component]
pub(super) fn ProjectSettings(open: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let name = RwSignal::new(String::new());
    let branch = RwSignal::new(String::new());
    let branches = RwSignal::new(Vec::<(String, String)>::new());
    let mode = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let policy = RwSignal::new(String::new());
    let allow_bypass = RwSignal::new(false);
    let confirm_remove = RwSignal::new(false);
    let busy = RwSignal::new(false);

    let fill = move |p: &Project| {
        name.set(p.name.clone());
        branch.set(p.default_target_branch.clone());
        mode.set(p.default_permission_mode.as_str().to_owned());
        model.set(p.default_model.clone().unwrap_or_default());
        policy.set(p.config_policy.as_str().to_owned());
        allow_bypass.set(p.allow_bypass);
    };
    Effect::new(move |_| {
        let Some(id) = ctx.project.get().filter(|_| open.get()) else {
            return;
        };
        confirm_remove.set(false);
        if let Some(p) = ctx
            .projects
            .with_untracked(|ps| ps.iter().find(|p| p.id == id).cloned())
        {
            fill(&p);
        }
        spawn_local(async move {
            match ipc::call::<ListBranches>(&ProjectIdReq { project_id: id }).await {
                Ok(list) => {
                    branches.try_set(list.branches.into_iter().map(|b| (b.clone(), b)).collect());
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    });

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
        };
        run(ctx, busy, "Progetto aggiornato", async move {
            ipc::call::<UpdateProject>(&req).await.map(|_| ())
        });
    };
    let save_security = move |_| {
        let Some(id) = ctx.project.get_untracked() else {
            return;
        };
        let req = SetProjectSecurityReq {
            id,
            config_policy: policy
                .get_untracked()
                .parse()
                .unwrap_or(ConfigPolicy::Isolated),
            allow_bypass: allow_bypass.get_untracked(),
        };
        run(ctx, busy, "Sicurezza del progetto aggiornata", async move {
            ipc::call::<SetProjectSecurity>(&req).await.map(|_| ())
        });
    };
    let remove = move |_| {
        if !confirm_remove.get_untracked() {
            confirm_remove.set(true);
            return;
        }
        let Some(id) = ctx.project.get_untracked() else {
            return;
        };
        let req = IdReq { id };
        open.set(false);
        ctx.open_task.set(None);
        run(
            ctx,
            busy,
            "Progetto rimosso; i branch restano",
            async move { ipc::call::<RemoveProject>(&req).await },
        );
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
        <div class="flex flex-col gap-4 pt-2" data-testid="project-settings">
            <Field id="project-name" label="Nome">
                <Input id="project-name" bind_value=name />
            </Field>
            <div class="grid grid-cols-2 gap-4">
                <Field id="project-branch" label="Branch target predefinito">
                    <Select id="project-branch" value=branch options=branches />
                </Field>
                <Field id="project-model" label="Modello predefinito">
                    <Select id="project-model" value=model options=owned(MODELS) />
                </Field>
            </div>
            <Field id="project-mode" label="Modalità predefinita">
                <Select id="project-mode" value=mode options=owned(MODES) disabled=no_bypass />
            </Field>
            <div class="flex justify-end">
                <Button size=ButtonSize::Sm attr:disabled=move || busy.get() on:click=save_defaults>
                    "Salva il progetto"
                </Button>
            </div>

            <Separator />
            <h4 class="font-medium">"Sicurezza"</h4>
            <Field
                id="project-policy"
                label="Configurazione Claude del repository"
                hint="Isolata: .claude/, hook e server MCP del repository non vengono caricati."
            >
                <Select id="project-policy" value=policy options=owned(POLICIES) />
            </Field>
            <Checkbox id="project-bypass" checked=allow_bypass>
                "Consenti la modalità Autonoma (bypassPermissions)"
            </Checkbox>
            <Show when=move || allow_bypass.get() || policy.get() == "trusted">
                <Callout variant=CalloutVariant::Warning class="md:mx-0" title="Il worktree non è una sandbox">
                    "Gli agenti possono eseguire comandi sul tuo Mac. L'app chiede una conferma prima di applicare."
                </Callout>
            </Show>
            <div class="flex justify-end">
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Outline
                    attr:disabled=move || busy.get()
                    on:click=save_security
                >
                    "Applica"
                </Button>
            </div>

            <Separator />
            <div class="flex items-center gap-3">
                <p class="text-muted-foreground flex-1 text-xs">
                    "Rimuove il progetto dall'app: i worktree vengono salvati e rimossi, i branch restano."
                </p>
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Destructive
                    attr:disabled=move || busy.get()
                    on:click=remove
                >
                    {move || if confirm_remove.get() { "Conferma rimozione" } else { "Rimuovi progetto" }}
                </Button>
            </div>
        </div>
    }
}

/// Runs one action: `busy` meanwhile, then a toast and a reload of the project list.
fn run(
    ctx: AppCtx,
    busy: RwSignal<bool>,
    done: &'static str,
    call: impl Future<Output = Result<(), AppError>> + 'static,
) {
    busy.set(true);
    spawn_local(async move {
        match call.await {
            Ok(()) => {
                ctx.toasts.success(done);
                ctx.refresh_projects();
            }
            Err(e) => ctx.toasts.app_error(&e),
        }
        busy.try_set(false);
    });
}
