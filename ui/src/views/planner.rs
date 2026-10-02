//! «Pianifica con un agente» (round 2026-10-02, section P): the overview's planner card, between
//! the hero and the bento. Owner: UI. A form (prompt, model, effort) → `start_plan`; next to it
//! the project's latest plan from `get_plan`, fetched on its own (never waiting for
//! `get_project_overview`), again on each `changed` of the project (`board_version`).
//!
//! The plan, by state: running → its transcript, compact and live (approvals inline), and
//! «Ferma» (`stop_attempt` on the plan's attempt); awaiting → «Avvia N task?» Sì/No
//! (`resolve_plan`); started, dismissed, failed → a state line and, on request, the transcript.
//! The tasks the planner created are listed in every state; each opens its task panel on the
//! Task page.
//!
//! DOM contract (e2e `planner_card`): root `[data-planner]`, prompt `#planner-prompt`, model
//! `#planner-model`, effort `#planner-effort`, `[data-action=start-plan]`; the plan
//! `[data-planner-state=<PlanState>]` with `[data-planner-transcript]`,
//! `[data-action=stop-plan]`, `[data-testid=plan-question]`, `[data-action=plan-proceed]`,
//! `[data-action=plan-dismiss]`, `[data-testid=plan-outcome]`,
//! `[data-action=toggle-plan-transcript]` and one `[data-planned-task=<id>]` per created task.

use atm_types::{
    AppError, AttemptIdReq, Effort, GetPlan, GetPlanReq, Id, MODEL_ALIASES, PlanState, PlanView,
    PlannedTask, Project, ResolvePlan, ResolvePlanReq, StartPlan, StartPlanReq, StopAttempt,
};
use icons::{ArrowRight, ChevronRight, CircleStop, ListChecks, ListTodo, WandSparkles};
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;

use crate::app::{ProjectView, use_app};
use crate::ipc;
use crate::ui::alert::{Alert, AlertDescription};
use crate::ui::label::Label;
use crate::ui::select_native::SelectNative;
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;
use crate::ui::textarea::Textarea;
use crate::views::board::column_title;
use crate::views::overview::{BTN_OUTLINE_SM, BTN_PRIMARY, CARD, LINK, OUTLINE_BADGE};
use crate::views::start_dialog::{EFFORTS, default_model_label, option};
use crate::views::transcript::Transcript;
use crate::widgets::status::{AgentBadge, FOCUS_RING, Status, StatusDot};

/// The transcript's box: fixed height, the transcript scrolls inside.
const TRANSCRIPT_BOX: &str =
    "flex h-72 flex-col overflow-hidden rounded-md border border-border bg-card";

/// A `get_plan` reply (or a plan a command returned), numbered: a newer one owns the card.
type Fetched = (u64, Result<Option<PlanView>, AppError>);

/// The card. `app_model` is the app's default model (`None` while loading), for the label of
/// the model select's default.
#[component]
pub fn PlannerCard(
    project: Memo<Option<Project>>,
    app_model: RwSignal<Option<Option<String>>>,
) -> impl IntoView {
    let ctx = use_app();
    let plan = RwSignal::new(None::<Fetched>);
    let seq = StoredValue::new(0u64);
    let next = move || {
        seq.try_update_value(|n| {
            *n += 1;
            *n
        })
        .unwrap_or_default()
    };
    // A plan a command returned: it supersedes any `get_plan` on its way.
    let show = move |view: PlanView| {
        let n = next();
        plan.try_set(Some((n, Ok(Some(view)))));
    };
    let fetch = move |project_id: Id| {
        let n = next();
        spawn_local(async move {
            let res = ipc::call::<GetPlan>(&GetPlanReq { project_id }).await;
            if seq.try_get_value() == Some(n) {
                plan.try_set(Some((n, res)));
            }
        });
    };

    let prompt = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let effort = RwSignal::new(String::new());
    let starting = RwSignal::new(false);
    let error = RwSignal::new(None::<AppError>);

    // On mount, on another project (a fresh form), and on each `changed` of this project.
    Effect::new(move |prev: Option<Option<Id>>| {
        let id = ctx.project.get();
        ctx.board_version.track();
        if prev.as_ref() != Some(&id) {
            plan.set(None);
            prompt.set(String::new());
            model.set(String::new());
            effort.set(String::new());
            error.set(None);
        }
        if let Some(id) = id.clone() {
            fetch(id);
        }
        id
    });

    let current = Memo::new(move |_| {
        plan.with(|p| match p {
            Some((_, Ok(view))) => view.clone(),
            _ => None,
        })
    });
    let state = Memo::new(move |_| current.with(|p| p.as_ref().map(|p| p.state)));
    let active = Memo::new(move |_| state.get().is_some_and(PlanState::is_active));

    let default_label = move || {
        let own = project.with(|p| p.as_ref().and_then(|p| p.default_model.clone()));
        let app = app_model.get().flatten();
        default_model_label(own.as_deref(), app.as_deref())
    };
    let empty_prompt = move || prompt.with(|p| p.trim().is_empty());

    let start = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(project_id) = ctx.project.get_untracked() else {
            return;
        };
        if empty_prompt() || active.get_untracked() || starting.get_untracked() {
            return;
        }
        let req = StartPlanReq {
            project_id: project_id.clone(),
            prompt: prompt.get_untracked(),
            model: Some(model.get_untracked()).filter(|m| !m.is_empty()),
            effort: effort.get_untracked().parse::<Effort>().ok(),
        };
        starting.set(true);
        error.set(None);
        spawn_local(async move {
            let res = ipc::call::<StartPlan>(&req).await;
            starting.try_set(false);
            // The user moved to another project meanwhile: its card is not this plan's.
            if ctx.project.try_get_untracked().flatten().as_ref() != Some(&project_id) {
                return;
            }
            match res {
                Ok(view) => {
                    prompt.try_set(String::new());
                    show(view);
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

    let badge = move || {
        current.with(|p| {
            p.as_ref().map(|p| {
                let (label, status) = state_badge(p.state, p.created.is_empty());
                view! { <AgentBadge label status /> }
            })
        })
    };

    view! {
        <section class=CARD data-planner="" aria-labelledby="planner-title">
            <header class="flex items-start justify-between gap-3 px-4 pt-4 pb-3">
                <div class="flex min-w-0 items-start gap-3">
                    <span class="grid size-8 shrink-0 place-items-center rounded-lg bg-primary/10 text-primary">
                        <WandSparkles class="size-4" />
                    </span>
                    <div class="min-w-0">
                        <h2 id="planner-title" class="text-[13px] font-semibold">
                            "Pianifica con un agente"
                        </h2>
                        <p class="mt-0.5 max-w-[72ch] text-xs text-muted-foreground text-pretty">
                            "Descrivi un obiettivo: un agente esplora il repository senza modificarlo e lo divide in task sulla board, pronti da avviare."
                        </p>
                    </div>
                </div>
                {badge}
            </header>
            <div class="grid grid-cols-12 gap-4 px-4 pb-4">
                <form class="col-span-5 flex flex-col gap-3" on:submit=start>
                    <div class="grid gap-2">
                        <Label html_for="planner-prompt">"Obiettivo"</Label>
                        <Textarea
                            id="planner-prompt"
                            rows=5u32
                            class="max-h-60 min-h-28 text-[13px] md:text-[13px]"
                            placeholder="Per esempio: prepara il rilascio 1.0, con i test che mancano al parser e la documentazione dell'API."
                            bind_value=prompt
                        />
                    </div>
                    <div class="grid grid-cols-2 gap-3">
                        <div class="grid gap-2">
                            <Label html_for="planner-model">"Modello"</Label>
                            <SelectNative id="planner-model" value=model.read_only() on_change=select(model)>
                                {move || option(String::new(), default_label(), model)}
                                {MODEL_ALIASES
                                    .iter()
                                    .map(|m| option((*m).into(), (*m).into(), model))
                                    .collect_view()}
                            </SelectNative>
                        </div>
                        <div class="grid gap-2">
                            <Label html_for="planner-effort">"Effort"</Label>
                            <SelectNative id="planner-effort" value=effort.read_only() on_change=select(effort)>
                                {EFFORTS
                                    .iter()
                                    .map(|(v, l)| option((*v).into(), (*l).into(), effort))
                                    .collect_view()}
                            </SelectNative>
                        </div>
                    </div>
                    {move || {
                        error
                            .get()
                            .map(|e| {
                                view! {
                                    <Alert class="border-destructive/50 text-destructive">
                                        <AlertDescription>{e.message}</AlertDescription>
                                    </Alert>
                                }
                            })
                    }}
                    <div class="mt-auto flex items-center justify-between gap-3">
                        <p class="text-xs text-muted-foreground text-pretty" data-testid="planner-help">
                            {move || {
                                if active.get() {
                                    "Una pianificazione alla volta: aspetta che questa finisca o rispondi alla domanda."
                                } else {
                                    "Sola lettura: legge il repository, non esegue comandi e non modifica file."
                                }
                            }}
                        </p>
                        <button
                            type="submit"
                            class=format!("{BTN_PRIMARY} shrink-0 disabled:pointer-events-none disabled:opacity-50")
                            data-action="start-plan"
                            disabled=move || starting.get() || active.get() || empty_prompt()
                        >
                            {move || {
                                if starting.get() {
                                    view! { <Spinner class="size-3.5" /> }.into_any()
                                } else {
                                    view! { <WandSparkles class="size-3.5 shrink-0" /> }.into_any()
                                }
                            }}
                            "Pianifica"
                        </button>
                    </div>
                </form>
                <div class="col-span-7 min-w-0">
                    {move || match plan.with(|p| p.as_ref().map(|(_, r)| r.as_ref().err().cloned())) {
                        None => view! { <Skeleton class="h-full min-h-48 w-full rounded-lg" /> }.into_any(),
                        Some(Some(e)) => view! {
                            <Alert class="border-destructive/50 text-destructive">
                                <AlertDescription>{format!("Pianificazione non disponibile: {}", e.message)}</AlertDescription>
                            </Alert>
                        }
                        .into_any(),
                        Some(None) => ().into_any(),
                    }}
                    <Show when=move || plan.with(|p| matches!(p, Some((_, Ok(None)))))>
                        <NoPlan />
                    </Show>
                    <Show when=move || current.with(Option::is_some)>
                        <PlanPanel current state show=Callback::new(show) />
                    </Show>
                </div>
            </div>
        </section>
    }
}

/// No plan in this project yet.
#[component]
fn NoPlan() -> impl IntoView {
    view! {
        <div class="flex h-full min-h-48 flex-col items-center justify-center rounded-lg border border-dashed border-border px-6 py-7 text-center" data-planner-state="none">
            <span class="grid size-9 place-items-center rounded-lg bg-muted text-muted-foreground">
                <ListTodo class="size-4" />
            </span>
            <p class="mt-3 text-[13px] font-medium">"Nessuna pianificazione ancora"</p>
            <p class="mt-1 max-w-[46ch] text-xs text-muted-foreground text-pretty">
                "Qui segui l'agente mentre lavora, poi trovi i task che ha creato e scegli se avviarli."
            </p>
        </div>
    }
}

/// The latest plan. Built once per card: its parts follow memos, so a refetch never remounts
/// the live transcript.
#[component]
fn PlanPanel(
    current: Memo<Option<PlanView>>,
    state: Memo<Option<PlanState>>,
    show: Callback<PlanView>,
) -> impl IntoView {
    let ctx = use_app();
    let plan_id = Memo::new(move |_| current.with(|p| p.as_ref().map(|p| p.id.clone())));
    let attempt =
        Memo::new(move |_| current.with(|p| p.as_ref().and_then(|p| p.attempt_id.clone())));
    let running = Memo::new(move |_| state.get() == Some(PlanState::Running));
    let created = Memo::new(move |_| {
        current.with(|p| p.as_ref().map(|p| p.created.clone()).unwrap_or_default())
    });
    let count = Memo::new(move |_| created.with(Vec::len));
    // «Avvia N task?» starts the top-level ones only: a sub-task is its parent agent's.
    let to_start =
        Memo::new(move |_| created.with(|c| c.iter().filter(|t| t.parent_id.is_none()).count()));
    let title = Memo::new(move |_| {
        current.with(|p| {
            p.as_ref()
                .map(|p| plan_title(&p.prompt))
                .unwrap_or_default()
        })
    });
    let prompt = Memo::new(move |_| {
        current.with(|p| p.as_ref().map(|p| p.prompt.clone()).unwrap_or_default())
    });
    let settings = Memo::new(move |_| {
        current.with(|p| {
            p.as_ref()
                .map(|p| settings_label(p.model.as_deref(), p.effort))
                .unwrap_or_default()
        })
    });

    // The finished plan's transcript, on request; closed again for another plan.
    let transcript_open = RwSignal::new(false);
    let stopping = RwSignal::new(false);
    let resolving = RwSignal::new(false);
    Effect::new(move |_| {
        plan_id.track();
        transcript_open.set(false);
    });
    Effect::new(move |_| {
        if !running.get() {
            stopping.set(false);
        }
    });

    let stop = move |_| {
        let Some(attempt_id) = attempt.get_untracked() else {
            return;
        };
        stopping.set(true);
        spawn_local(async move {
            if let Err(e) = ipc::call::<StopAttempt>(&AttemptIdReq { attempt_id }).await {
                stopping.try_set(false);
                ctx.toasts.app_error(&e);
            }
        });
    };
    let resolve = move |proceed: bool| {
        let Some(id) = plan_id.get_untracked() else {
            return;
        };
        resolving.set(true);
        spawn_local(async move {
            let res = ipc::call::<ResolvePlan>(&ResolvePlanReq {
                plan_id: id.clone(),
                proceed,
            })
            .await;
            resolving.try_set(false);
            match res {
                Ok(view) if plan_id.try_get_untracked().flatten().as_ref() == Some(&id) => {
                    show.run(view);
                }
                Ok(_) => {}
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    };

    let outcome = move || {
        let n = count.get();
        match state.get() {
            Some(PlanState::Started) if n == 0 => {
                Some(("Nessun task creato.", "text-muted-foreground".to_owned()))
            }
            Some(PlanState::Started) => Some((
                "Avviati: partono appena c'è un agente libero, nell'ordine delle dipendenze.",
                "text-status-done".to_owned(),
            )),
            Some(PlanState::Dismissed) => Some((
                "Non avviati: i task restano in Da fare.",
                "text-muted-foreground".to_owned(),
            )),
            Some(PlanState::Failed) => Some((
                "La pianificazione si è interrotta. I task già creati restano come sono.",
                "text-status-failed".to_owned(),
            )),
            _ => None,
        }
    };

    view! {
        <div
            class="flex h-full flex-col rounded-lg border border-border bg-background"
            data-planner-state=move || state.get().map(PlanState::as_str)
        >
            <div class="flex min-w-0 items-start gap-3 px-3 pt-3">
                <p class="min-w-0 flex-1 truncate text-[13px] font-medium" title=move || prompt.get()>
                    {move || title.get()}
                </p>
                <span class=OUTLINE_BADGE title="Modello ed effort della pianificazione">
                    {move || settings.get()}
                </span>
            </div>
            <Show when=move || running.get()>
                <div class="px-3 pt-3">
                    <div class=TRANSCRIPT_BOX data-planner-transcript="">
                        {move || match attempt.get() {
                            Some(attempt_id) => view! { <Transcript attempt_id /> }.into_any(),
                            None => view! {
                                <div class="flex flex-1 items-center justify-center gap-2 text-xs text-muted-foreground">
                                    <Spinner class="size-3.5" />
                                    "Avvio dell'agente…"
                                </div>
                            }
                            .into_any(),
                        }}
                    </div>
                </div>
                <div class="flex items-center gap-2 px-3 pt-3">
                    <StatusDot status=Status::Running ping=true />
                    <p class="min-w-0 flex-1 text-xs text-muted-foreground text-pretty">
                        {move || match count.get() {
                            0 => "L'agente esplora il repository: i task che crea compaiono qui.".to_owned(),
                            n => format!("L'agente sta lavorando · {} finora.", tasks_label(n)),
                        }}
                    </p>
                    <button
                        type="button"
                        class=format!("{BTN_OUTLINE_SM} disabled:pointer-events-none disabled:opacity-50")
                        data-action="stop-plan"
                        disabled=move || stopping.get() || attempt.with(Option::is_none)
                        on:click=stop
                    >
                        {move || {
                            if stopping.get() {
                                view! { <Spinner class="size-3.5" /> }.into_any()
                            } else {
                                view! { <CircleStop class="size-3.5 shrink-0" /> }.into_any()
                            }
                        }}
                        "Ferma"
                    </button>
                </div>
            </Show>
            <Show when=move || { count.get() > 0 }>
                <div class="px-3 pt-3">
                    <p class="mb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
                        {move || format!("Task creati · {}", count.get())}
                    </p>
                    <ul class="-mx-1 flex flex-col" data-testid="planned-tasks">
                        <For
                            each=move || created.get()
                            key=|t| (t.id.clone(), t.title.clone(), t.status)
                            children=move |task| view! { <PlannedRow task /> }
                        />
                    </ul>
                </div>
            </Show>
            <Show when=move || state.get() == Some(PlanState::Awaiting)>
                <div
                    role="status"
                    class="mx-3 mt-3 flex items-center gap-3 rounded-lg border border-status-waiting/25 bg-status-waiting/8 px-3 py-2.5"
                >
                    <ListChecks class="size-4 shrink-0 text-status-waiting" />
                    <div class="min-w-0 flex-1">
                        <p class="text-[13px] font-medium" data-testid="plan-question">
                            {move || format!("Avvia {}?", tasks_label(to_start.get()))}
                        </p>
                        <p class="text-xs text-muted-foreground text-pretty">
                            "Con No restano in Da fare: li puoi avviare tu, uno per uno."
                        </p>
                    </div>
                    <button
                        type="button"
                        class=format!("{BTN_OUTLINE_SM} disabled:pointer-events-none disabled:opacity-50")
                        data-action="plan-dismiss"
                        disabled=move || resolving.get()
                        on:click=move |_| resolve(false)
                    >
                        "No"
                    </button>
                    <button
                        type="button"
                        class=format!("{BTN_PRIMARY} h-7 disabled:pointer-events-none disabled:opacity-50")
                        data-action="plan-proceed"
                        disabled=move || resolving.get()
                        on:click=move |_| resolve(true)
                    >
                        {move || resolving.get().then(|| view! { <Spinner class="size-3.5" /> })}
                        "Sì, avvia"
                    </button>
                </div>
            </Show>
            {move || {
                outcome()
                    .map(|(text, color)| {
                        view! {
                            <p class=format!("px-3 pt-3 text-xs text-pretty {color}") data-testid="plan-outcome">
                                {text}
                            </p>
                        }
                    })
            }}
            <Show when=move || !running.get() && attempt.with(Option::is_some)>
                <div class="px-3 pt-2">
                    <button
                        type="button"
                        class=format!("{LINK} inline-flex items-center gap-1 text-xs")
                        data-action="toggle-plan-transcript"
                        aria-expanded=move || transcript_open.get().to_string()
                        on:click=move |_| transcript_open.update(|o| *o = !*o)
                    >
                        <span class=move || {
                            tw_merge!(
                                "transition-transform", if transcript_open.get() { "rotate-90" } else { "" }
                            )
                        }>
                            <ChevronRight class="size-3.5" />
                        </span>
                        {move || if transcript_open.get() { "Nascondi la trascrizione" } else { "Mostra la trascrizione" }}
                    </button>
                    {move || {
                        transcript_open
                            .get()
                            .then(|| attempt.get())
                            .flatten()
                            .map(|attempt_id| {
                                view! {
                                    <div class=format!("{TRANSCRIPT_BOX} mt-2") data-planner-transcript="">
                                        <Transcript attempt_id />
                                    </div>
                                }
                            })
                    }}
                </div>
            </Show>
            <div class="pb-3"></div>
        </div>
    }
}

/// One created task: its column's dot, title and column; opens its panel on the Task page.
#[component]
fn PlannedRow(task: PlannedTask) -> impl IntoView {
    let ctx = use_app();
    let PlannedTask {
        id, title, status, ..
    } = task;
    let target = id.clone();
    let open = move |_| {
        ctx.open_task.set(Some(target.clone()));
        ctx.project_view.set(ProjectView::Tasks);
    };
    view! {
        <li>
            <button
                type="button"
                class=format!(
                    "group flex w-full min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-accent {FOCUS_RING}",
                )
                data-planned-task=id
                title="Apri il task"
                on:click=open
            >
                <StatusDot status=Status::of_column(status) />
                <span class="min-w-0 flex-1 truncate text-[13px]">{title}</span>
                <span class="shrink-0 text-[11px] text-muted-foreground">{column_title(status)}</span>
                <ArrowRight class="size-3.5 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100" />
            </button>
        </li>
    }
}

/// Badge of the card's header for the latest plan.
fn state_badge(state: PlanState, none_created: bool) -> (&'static str, Status) {
    match state {
        PlanState::Running => ("In corso", Status::Running),
        PlanState::Awaiting => ("Da confermare", Status::Waiting),
        PlanState::Started if none_created => ("Completata", Status::Done),
        PlanState::Started => ("Task avviati", Status::Done),
        PlanState::Dismissed => ("Non avviati", Status::Cancelled),
        PlanState::Failed => ("Interrotta", Status::Failed),
    }
}

/// The plan's first non-empty line, as the hidden task's title.
fn plan_title(prompt: &str) -> String {
    prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// `opus · Alto`, `Predefinito`.
fn settings_label(model: Option<&str>, effort: Option<Effort>) -> String {
    let effort = effort.and_then(|e| {
        EFFORTS
            .iter()
            .find(|(v, _)| *v == e.as_str())
            .map(|(_, l)| *l)
    });
    match (model, effort) {
        (Some(m), Some(e)) => format!("{m} · {e}"),
        (Some(m), None) => m.to_owned(),
        (None, Some(e)) => format!("Modello predefinito · {e}"),
        (None, None) => "Modello predefinito".to_owned(),
    }
}

/// `1 task`, `3 task`: «task» stays the same in Italian, the count reads either way.
fn tasks_label(n: usize) -> String {
    format!("{n} task")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(
            plan_title("\n  Prepara il rilascio  \nDettagli"),
            "Prepara il rilascio"
        );
        assert_eq!(plan_title(""), "");
        assert_eq!(
            settings_label(Some("opus"), Some(Effort::High)),
            "opus · Alto"
        );
        assert_eq!(settings_label(None, None), "Modello predefinito");
        assert_eq!(tasks_label(2), "2 task");
        assert_eq!(state_badge(PlanState::Started, true).0, "Completata");
        assert_eq!(state_badge(PlanState::Awaiting, false).1, Status::Waiting);
    }
}
