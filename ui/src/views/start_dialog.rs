//! "Avvia" dialog (spec §9.2): target branch, model, effort, permission mode (Autonomo only
//! with `allow_bypass`, with the bypass callout) → `start_attempt`. Owner: M2-UI-TASK.

use atm_types::{
    AppError, Effort, GetTaskDetail, Id, IdReq, ListBranches, PermissionMode, ProjectIdReq,
    StartAttempt, StartAttemptReq,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::use_app;
use crate::ipc;
use crate::ui::alert::{Alert, AlertDescription};
use crate::ui::button::Button;
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader,
    DialogTitle,
};
use crate::ui::label::Label;
use crate::ui::select_native::SelectNative;
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;

/// `--model` aliases (spec §0.1); "" = the CLI default.
const MODELS: &[(&str, &str)] = &[
    ("", "Predefinito"),
    ("opus", "opus"),
    ("sonnet", "sonnet"),
    ("fable", "fable"),
];
const EFFORTS: &[(&str, &str)] = &[
    ("", "Predefinito"),
    ("low", "Basso"),
    ("medium", "Medio"),
    ("high", "Alto"),
    ("xhigh", "Molto alto"),
    ("max", "Massimo"),
];
const MODES: &[(PermissionMode, &str)] = &[
    (
        PermissionMode::Default,
        "Supervisionato: chiede quasi sempre",
    ),
    (
        PermissionMode::AcceptEdits,
        "Auto-edit: approva modifiche e comandi sui file",
    ),
    (
        PermissionMode::BypassPermissions,
        "Autonomo: non chiede mai",
    ),
];

/// What each permission mode lets the agent do without asking (spec D6; the CLI 2.1.283 as
/// observed in M5, spec §10.2): shown under the mode's select here and in the project settings.
pub fn mode_help(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => {
            "Supervisionato: chiede prima di ogni modifica e di ogni comando; passano da sole solo \
             le letture (per esempio ls)."
        }
        PermissionMode::AcceptEdits => {
            "Auto-edit: approva da solo le modifiche ai file e i comandi shell che leggono o \
             scrivono file nel worktree (per esempio printf … >> README.md); per tutto il resto \
             chiede, come Supervisionato."
        }
        PermissionMode::BypassPermissions => {
            "Autonomo: non chiede mai, esegue qualunque comando. Il worktree non è una sandbox."
        }
    }
}

/// Options loaded for the task when the dialog opens.
#[derive(Clone)]
struct Options {
    branches: Vec<String>,
    allow_bypass: bool,
    /// The project's default model when it is not one of [`MODELS`] (e.g. a full model id).
    other_model: Option<String>,
}

/// Open while `task_id` is `Some`; closing or starting sets it to `None`. Used by the board
/// (drop on "In corso") and by the task panel.
#[component]
pub fn StartDialog(task_id: RwSignal<Option<Id>>) -> impl IntoView {
    let ctx = use_app();
    let open = RwSignal::new(false);
    let options = RwSignal::new(None::<Result<Options, AppError>>);
    let target = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let effort = RwSignal::new(String::new());
    let mode = RwSignal::new(PermissionMode::AcceptEdits.as_str().to_owned());
    let starting = RwSignal::new(false);
    let error = RwSignal::new(None::<AppError>);

    // `task_id` drives the dialog; closing it (Esc, backdrop, Annulla) clears `task_id`.
    Effect::new(move |_| {
        let wanted = task_id.with(Option::is_some);
        if open.get_untracked() != wanted {
            open.set(wanted);
        }
    });
    Effect::new(move |_| {
        if !open.get() && task_id.get_untracked().is_some() {
            task_id.set(None);
        }
    });

    // Branches and project defaults for the task being started.
    Effect::new(move |_| {
        let Some(id) = task_id.get() else {
            return;
        };
        options.set(None);
        error.set(None);
        spawn_local(async move {
            let loaded = load_options(ctx.projects, id.clone()).await;
            if task_id.try_get_untracked().flatten() != Some(id) {
                return;
            }
            let loaded = loaded.map(|(opts, defaults)| {
                target.try_set(defaults.target);
                model.try_set(defaults.model);
                mode.try_set(defaults.mode.as_str().to_owned());
                effort.try_set(String::new());
                opts
            });
            options.try_set(Some(loaded));
        });
    });

    let allow_bypass = move || options.with(|o| matches!(o, Some(Ok(o)) if o.allow_bypass));
    let bypass_selected = move || mode.with(|m| m == PermissionMode::BypassPermissions.as_str());

    let start = move |_| {
        let Some(id) = task_id.get_untracked() else {
            return;
        };
        let Ok(permission_mode) = mode.get_untracked().parse::<PermissionMode>() else {
            return;
        };
        let req = StartAttemptReq {
            task_id: id.clone(),
            target_branch: target.get_untracked(),
            permission_mode,
            model: Some(model.get_untracked()).filter(|m| !m.is_empty()),
            effort: effort.get_untracked().parse::<Effort>().ok(),
        };
        starting.set(true);
        error.set(None);
        spawn_local(async move {
            let res = ipc::call::<StartAttempt>(&req).await;
            starting.try_set(false);
            match res {
                Ok(attempt) => {
                    ctx.toasts
                        .success(format!("Tentativo avviato su {}.", attempt.branch));
                    open.try_set(false);
                    if ctx.open_task.get_untracked().as_ref() != Some(&id) {
                        ctx.open_task.set(Some(id));
                    }
                }
                Err(e) => {
                    error.try_set(Some(e));
                }
            }
        });
    };

    let select = move |signal: RwSignal<String>| {
        Callback::new(move |ev: leptos::ev::Event| signal.set(event_target_value(&ev)))
    };

    view! {
        <Dialog open>
            <DialogContent class="sm:max-w-lg" data_name_prefix="StartDialog">
                <DialogBody>
                    <DialogHeader>
                        <DialogTitle>"Avvia l'agente"</DialogTitle>
                        <DialogDescription>
                            "Un nuovo worktree e un branch dedicato per questo tentativo."
                        </DialogDescription>
                    </DialogHeader>
                    {move || match options.get() {
                        None => view! {
                            <div class="flex flex-col gap-3">
                                <Skeleton class="h-9 w-full" />
                                <Skeleton class="h-9 w-full" />
                                <Skeleton class="h-9 w-full" />
                            </div>
                        }
                        .into_any(),
                        Some(Err(e)) => view! {
                            <Alert class="border-destructive/50 text-destructive">
                                <AlertDescription>{e.to_string()}</AlertDescription>
                            </Alert>
                        }
                        .into_any(),
                        Some(Ok(opts)) => view! {
                            <div class="grid gap-4">
                                <div class="grid gap-2">
                                    <Label html_for="start-target">"Branch target"</Label>
                                    <SelectNative
                                        id="start-target"
                                        value=target.read_only()
                                        on_change=select(target)
                                    >
                                        {opts
                                            .branches
                                            .into_iter()
                                            .map(|b| option(b.clone(), b, target))
                                            .collect_view()}
                                    </SelectNative>
                                </div>
                                <div class="grid grid-cols-2 gap-4">
                                    <div class="grid gap-2">
                                        <Label html_for="start-model">"Modello"</Label>
                                        <SelectNative
                                            id="start-model"
                                            value=model.read_only()
                                            on_change=select(model)
                                        >
                                            {MODELS
                                                .iter()
                                                .map(|(v, l)| option((*v).into(), (*l).into(), model))
                                                .collect_view()}
                                            {opts
                                                .other_model
                                                .map(|m| option(m.clone(), format!("Progetto: {m}"), model))}
                                        </SelectNative>
                                    </div>
                                    <div class="grid gap-2">
                                        <Label html_for="start-effort">"Effort"</Label>
                                        <SelectNative
                                            id="start-effort"
                                            value=effort.read_only()
                                            on_change=select(effort)
                                        >
                                            {EFFORTS
                                                .iter()
                                                .map(|(v, l)| option((*v).into(), (*l).into(), effort))
                                                .collect_view()}
                                        </SelectNative>
                                    </div>
                                </div>
                                <div class="grid gap-2">
                                    <Label html_for="start-mode">"Permessi"</Label>
                                    <SelectNative
                                        id="start-mode"
                                        value=mode.read_only()
                                        on_change=select(mode)
                                    >
                                        {MODES
                                            .iter()
                                            .map(|(m, l)| {
                                                let disabled = *m == PermissionMode::BypassPermissions
                                                    && !opts.allow_bypass;
                                                let label = if disabled {
                                                    format!("{l} (da abilitare nel progetto)")
                                                } else {
                                                    (*l).to_owned()
                                                };
                                                view! {
                                                    <option
                                                        value=m.as_str()
                                                        disabled=disabled
                                                        prop:selected=move || mode.with(|v| v == m.as_str())
                                                    >
                                                        {label}
                                                    </option>
                                                }
                                            })
                                            .collect_view()}
                                    </SelectNative>
                                    <p class="text-muted-foreground text-xs" data-testid="mode-help">
                                        {move || {
                                            mode.with(|m| m.parse::<PermissionMode>().ok())
                                                .map(mode_help)
                                        }}
                                    </p>
                                </div>
                            </div>
                        }
                        .into_any(),
                    }}
                    <Show when=move || bypass_selected() && allow_bypass()>
                        <Callout variant=CalloutVariant::Warning title="Modalità Autonomo">
                            "L'agente esegue qualunque comando senza chiedere. Il worktree non è una sandbox: può toccare file e servizi fuori dal repository."
                        </Callout>
                    </Show>
                    {move || {
                        error
                            .get()
                            .map(|e| {
                                view! {
                                    <Alert class="border-destructive/50 text-destructive">
                                        <AlertDescription>{e.to_string()}</AlertDescription>
                                    </Alert>
                                }
                            })
                    }}
                    <DialogFooter>
                        <DialogClose>"Annulla"</DialogClose>
                        <Button
                            attr:data-action="start"
                            attr:disabled=move || {
                                starting.get() || !options.with(|o| matches!(o, Some(Ok(_))))
                                    || target.with(String::is_empty)
                            }
                            on:click=start
                        >
                            {move || starting.get().then(|| view! { <Spinner /> })}
                            "Avvia"
                        </Button>
                    </DialogFooter>
                </DialogBody>
            </DialogContent>
        </Dialog>
    }
}

/// `prop:selected` keeps the choice when the options render after the value is set.
fn option(value: String, label: String, current: RwSignal<String>) -> impl IntoView {
    let selected = value.clone();
    view! {
        <option value=value prop:selected=move || current.with(|c| *c == selected)>
            {label}
        </option>
    }
}

struct Defaults {
    target: String,
    model: String,
    mode: PermissionMode,
}

/// The task's project (for its defaults and `allow_bypass`) and its local branches.
async fn load_options(
    projects: RwSignal<Vec<atm_types::Project>>,
    task_id: Id,
) -> Result<(Options, Defaults), AppError> {
    let detail = ipc::call::<GetTaskDetail>(&IdReq { id: task_id }).await?;
    let project_id = detail.task.project_id;
    let project = projects
        .try_with_untracked(|ps| ps.iter().find(|p| p.id == project_id).cloned())
        .flatten();
    let list = ipc::call::<ListBranches>(&ProjectIdReq { project_id }).await?;
    let preferred = project
        .as_ref()
        .map(|p| p.default_target_branch.clone())
        .filter(|b| list.branches.contains(b));
    let target = preferred
        .or(list.current.clone())
        .or_else(|| list.branches.first().cloned())
        .unwrap_or_default();
    let allow_bypass = project.as_ref().is_some_and(|p| p.allow_bypass);
    let mode = project
        .as_ref()
        .map(|p| p.default_permission_mode)
        .filter(|m| *m != PermissionMode::BypassPermissions || allow_bypass)
        .unwrap_or(PermissionMode::AcceptEdits);
    let model = project.and_then(|p| p.default_model).unwrap_or_default();
    let options = Options {
        branches: list.branches,
        allow_bypass,
        other_model: Some(model.clone()).filter(|m| MODELS.iter().all(|(v, _)| v != m)),
    };
    let defaults = Defaults {
        target,
        model,
        mode,
    };
    Ok((options, defaults))
}
