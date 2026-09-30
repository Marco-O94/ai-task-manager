//! "Avvia" dialog (spec §9.2): target branch, model, effort, sub-agent limit and model (spec
//! F6), permission mode (Autonomo only with `allow_bypass`, with the bypass callout) →
//! `start_attempt`. Owner: M2-UI-TASK, sub-agents UI-TASKS.

use atm_types::{
    AppError, Effort, Empty, GetSettings, GetTaskDetail, Id, IdReq, ListBranches, MAX_SUBAGENTS,
    MODEL_ALIASES, PermissionMode, ProjectIdReq, StartAttempt, StartAttemptReq,
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

/// Label of the `""` choice of the model select: the model `start_attempt` resolves without
/// one (the project's default, then the app's, spec §7.4), else the CLI's own default.
fn default_model_label(project: Option<&str>, app: Option<&str>) -> String {
    let resolved = [project, app]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|m| !m.is_empty());
    format!("Predefinito ({})", resolved.unwrap_or("CLI"))
}

/// `max_subagents` choices `(value, label)`: `""` = no limit, then 0..=[`MAX_SUBAGENTS`].
fn subagent_limits() -> impl Iterator<Item = (String, String)> {
    let named = [("", "Predefinito del CLI"), ("0", "Nessun sub-agent")];
    named
        .into_iter()
        .map(|(v, l)| (v.to_owned(), l.to_owned()))
        .chain((1..=MAX_SUBAGENTS).map(|n| (n.to_string(), n.to_string())))
}

/// `(max_subagents, subagent_model)` of the request from the two selects: `""` is `None`, and
/// with no sub-agents allowed their model is `None` too.
fn subagent_fields(max: &str, model: &str) -> (Option<u8>, Option<String>) {
    let max = max.parse::<u8>().ok();
    let model = Some(model.to_owned()).filter(|m| !m.is_empty() && max != Some(0));
    (max, model)
}

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
    /// [`default_model_label`]: the default can be a full model id, not in [`MODEL_ALIASES`].
    default_model: String,
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
    let max_subagents = RwSignal::new(String::new());
    let subagent_model = RwSignal::new(String::new());
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
                mode.try_set(defaults.mode.as_str().to_owned());
                for choice in [model, effort, max_subagents, subagent_model] {
                    choice.try_set(String::new());
                }
                opts
            });
            options.try_set(Some(loaded));
        });
    });

    let allow_bypass = move || options.with(|o| matches!(o, Some(Ok(o)) if o.allow_bypass));
    let bypass_selected = move || mode.with(|m| m == PermissionMode::BypassPermissions.as_str());
    let no_subagents = move || max_subagents.with(|m| m == "0");

    let start = move |_| {
        let Some(id) = task_id.get_untracked() else {
            return;
        };
        let Ok(permission_mode) = mode.get_untracked().parse::<PermissionMode>() else {
            return;
        };
        let (max_subagents, subagent_model) = subagent_fields(
            &max_subagents.get_untracked(),
            &subagent_model.get_untracked(),
        );
        let req = StartAttemptReq {
            task_id: id.clone(),
            target_branch: target.get_untracked(),
            permission_mode,
            model: Some(model.get_untracked()).filter(|m| !m.is_empty()),
            effort: effort.get_untracked().parse::<Effort>().ok(),
            subagent_model,
            max_subagents,
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
            <DialogContent class="overflow-y-auto rounded-xl shadow-md sm:max-w-lg" data_name_prefix="StartDialog">
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
                                            {option(String::new(), opts.default_model, model)}
                                            {MODEL_ALIASES
                                                .iter()
                                                .map(|m| option((*m).into(), (*m).into(), model))
                                                .collect_view()}
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
                                    <div class="grid grid-cols-2 gap-4">
                                        <div class="grid gap-2">
                                            <Label html_for="start-max-subagents">"Sub-agent (max)"</Label>
                                            <SelectNative
                                                id="start-max-subagents"
                                                value=max_subagents.read_only()
                                                on_change=select(max_subagents)
                                            >
                                                {subagent_limits()
                                                    .map(|(v, l)| option(v, l, max_subagents))
                                                    .collect_view()}
                                            </SelectNative>
                                        </div>
                                        <Show when=move || !no_subagents()>
                                            <div class="grid gap-2">
                                                <Label html_for="start-subagent-model">"Modello dei sub-agent"</Label>
                                                <SelectNative
                                                    id="start-subagent-model"
                                                    value=subagent_model.read_only()
                                                    on_change=select(subagent_model)
                                                >
                                                    {option(String::new(), "Predefinito del CLI".into(), subagent_model)}
                                                    {MODEL_ALIASES
                                                        .iter()
                                                        .map(|m| option((*m).into(), (*m).into(), subagent_model))
                                                        .collect_view()}
                                                </SelectNative>
                                            </div>
                                        </Show>
                                    </div>
                                    <p class="text-muted-foreground text-xs" data-testid="subagent-help">
                                        {move || {
                                            if no_subagents() {
                                                "L'agente lavora da solo: non può avviare sub-agent."
                                            } else {
                                                "Il massimo vale per tutto il tentativo. Il modello vale per i sub-agent che non ne chiedono uno proprio: Explore o una chiamata con un modello esplicito possono usarne un altro."
                                            }
                                        }}
                                    </p>
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
    mode: PermissionMode,
}

/// The task's project (for its defaults and `allow_bypass`), its local branches and, when the
/// project has no default model, the app's.
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
    let project_model = project.and_then(|p| p.default_model);
    let app_model = match project_model {
        Some(_) => None,
        None => ipc::call::<GetSettings>(&Empty {}).await?.default_model,
    };
    let options = Options {
        branches: list.branches,
        allow_bypass,
        default_model: default_model_label(project_model.as_deref(), app_model.as_deref()),
    };
    Ok((options, Defaults { target, mode }))
}

#[cfg(test)]
mod tests {
    use super::{default_model_label, subagent_fields, subagent_limits};

    #[test]
    fn the_default_model_is_the_one_start_attempt_resolves() {
        assert_eq!(
            default_model_label(Some("opus"), Some("sonnet")),
            "Predefinito (opus)"
        );
        assert_eq!(
            default_model_label(None, Some("claude-sonnet-4-5")),
            "Predefinito (claude-sonnet-4-5)"
        );
        assert_eq!(default_model_label(Some(" "), None), "Predefinito (CLI)");
        assert_eq!(default_model_label(None, None), "Predefinito (CLI)");
    }

    #[test]
    fn subagent_choices_map_to_the_request() {
        let limits: Vec<String> = subagent_limits().map(|(v, _)| v).collect();
        assert_eq!(limits.first().map(String::as_str), Some(""));
        assert_eq!(limits.len(), 12); // "", 0, 1..=10
        assert_eq!(limits.last().map(String::as_str), Some("10"));

        assert_eq!(subagent_fields("", ""), (None, None));
        assert_eq!(subagent_fields("", "haiku"), (None, Some("haiku".into())));
        assert_eq!(
            subagent_fields("3", "sonnet"),
            (Some(3), Some("sonnet".into()))
        );
        // No sub-agents: their model is not sent.
        assert_eq!(subagent_fields("0", "sonnet"), (Some(0), None));
    }
}
