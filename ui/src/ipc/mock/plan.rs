//! Mock of the planner (round 2026-10-02, «Pianifica con un agente»): `start_plan`,
//! `get_plan`, `resolve_plan`. A plan's transcript is a real mock attempt
//! (`attempt::plan_attempt`, the `plan` fixture, mode Default), so the card's embedded
//! transcript, «Ferma» and «Mostra la trascrizione» work as in the app.
//!
//! The demo project starts with one plan, chosen with `?plan=` (screenshots of every state):
//! - absent or `awaiting`: finished, «Avvia 2 task?» (`task-parser-tests`, `task-api-docs`);
//! - `running`: its turn replays from the card's first subscription, then ends like a new one;
//! - `started`, `dismissed`: finished with the same two tasks, answered Sì / No;
//! - `empty`: finished with no task («Nessun task creato»);
//! - `failed`: stopped after creating `task-parser-tests`;
//! - `idle`: no plan.
//!
//! A new plan runs the fixture (a few seconds); completed, it creates two tasks on the board
//! (the second «Parte dopo…» the first) and awaits confirmation when the project's mode is
//! Default, else it is started; stopped, it fails. The mock never starts the tasks: Sì and No
//! only change the plan's state.

use std::cell::RefCell;
use std::collections::HashMap;

use atm_types::*;
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;

use super::{attempt, board};

/// How often a running plan's turn is polled for its end.
const POLL_MS: i32 = 300;
/// The tasks a completed mock plan creates, as the fixture's `create_task` calls say.
const CREATED: [(&str, &str); 2] = [
    (
        "Test di integrazione del parser",
        "Copri con test di integrazione i casi del parser: input vuoto, parentesi sbilanciate, span.",
    ),
    (
        "Documenta le rotte di /users",
        "Documenta ogni rotta di src/api/users.rs: parametri, risposte ed errori.",
    ),
];

/// A plan as stored: [`PlanView`] with the created tasks by id (title and status are read
/// from the board each time).
struct Plan {
    project_id: Id,
    view: PlanView,
    created: Vec<Id>,
}

thread_local! {
    /// Every plan by id; the latest of a project is its newest `created_at`.
    static PLANS: RefCell<HashMap<Id, Plan>> = RefCell::new(seed());
}

fn seed() -> HashMap<Id, Plan> {
    let demo = ["task-parser-tests", "task-api-docs"];
    let (state, created): (PlanState, &[&str]) = match query("plan").as_deref() {
        Some("idle") => return HashMap::new(),
        Some("running") => (PlanState::Running, &[]),
        Some("started") => (PlanState::Started, &demo),
        Some("dismissed") => (PlanState::Dismissed, &demo),
        Some("empty") => (PlanState::Started, &[]),
        Some("failed") => (PlanState::Failed, &demo[..1]),
        _ => (PlanState::Awaiting, &demo),
    };
    let running = state == PlanState::Running;
    let id: Id = "plan-demo".into();
    let project_id: Id = "project-demo".into();
    let prompt = "Prepara il rilascio 1.0: test del parser e documentazione dell'API.";
    // In the past even when running: a plan started later is always the newest.
    let created_at = super::now_ms() - if running { 1_000 } else { 20 * 60_000 };
    let attempt_id = attempt::plan_attempt(
        &hidden_task(&id, &project_id, prompt, created_at),
        running,
        true,
    );
    let plan = Plan {
        project_id,
        view: PlanView {
            id: id.clone(),
            prompt: prompt.into(),
            model: Some("opus".into()),
            effort: Some(Effort::High),
            state,
            attempt_id: Some(attempt_id.clone()),
            created: Vec::new(),
            created_at,
        },
        created: created.iter().map(|&t| t.into()).collect(),
    };
    if running {
        spawn_local(watch(id.clone(), attempt_id));
    }
    HashMap::from([(id, plan)])
}

/// The plan's hidden task, as the core stores it (`kind = plan`, never on the board).
fn hidden_task(id: &str, project_id: &str, prompt: &str, now: Millis) -> Task {
    let title = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .chars()
        .take(MAX_PLAN_TITLE)
        .collect();
    Task {
        id: id.into(),
        project_id: project_id.into(),
        title,
        description: prompt.into(),
        status: TaskStatus::InProgress,
        position: 0.0,
        created_at: now,
        updated_at: now,
        parent_id: None,
        auto: false,
        after_id: None,
        kind: TaskKind::Plan,
        launch: false,
    }
}

/// Serves one planner command (`req` the serialized `C::Req`, the result the serialized
/// `C::Res`).
pub fn handle(cmd: &str, req: Value) -> Result<Value, AppError> {
    let res = match cmd {
        StartPlan::NAME => serde_json::to_value(start_plan(serde_json::from_value(req)?)?)?,
        GetPlan::NAME => serde_json::to_value(get_plan(serde_json::from_value(req)?)?)?,
        ResolvePlan::NAME => serde_json::to_value(resolve_plan(serde_json::from_value(req)?)?)?,
        _ => return Err(AppError::not_implemented(cmd)),
    };
    Ok(res)
}

/// The view with the created tasks as the board has them now (deleted ones gone).
fn view(plan: &Plan) -> PlanView {
    let created = plan
        .created
        .iter()
        .filter_map(|id| board::task(id))
        .map(|t| PlannedTask {
            id: t.id,
            title: t.title,
            status: t.status,
            parent_id: t.parent_id,
        })
        .collect();
    PlanView {
        created,
        ..plan.view.clone()
    }
}

fn latest(project_id: &str) -> Option<PlanView> {
    PLANS.with_borrow(|plans| {
        plans
            .values()
            .filter(|p| p.project_id == project_id)
            .max_by_key(|p| p.view.created_at)
            .map(view)
    })
}

fn changed(project_id: &str) {
    super::emit(
        EVENT_CHANGED,
        &Changed {
            project_id: Some(project_id.to_owned()),
            task_id: None,
        },
    );
}

fn start_plan(req: StartPlanReq) -> Result<PlanView, AppError> {
    let prompt = req.prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(AppError::invalid("Scrivi cosa deve pianificare l'agente"));
    }
    let project = board::find_project(&req.project_id)
        .ok_or_else(|| AppError::not_found(format!("progetto {}", req.project_id)))?;
    if latest(&project.id).is_some_and(|p| p.state.is_active()) {
        return Err(AppError::conflict(
            "Una pianificazione è già in corso o in attesa di conferma in questo progetto",
        ));
    }
    if let Some(model) = &req.model
        && !MODEL_ALIASES.contains(&model.as_str())
    {
        return Err(AppError::invalid(format!("Modello sconosciuto: {model}")));
    }
    let id = super::new_id();
    let now = super::now_ms();
    let attempt_id =
        attempt::plan_attempt(&hidden_task(&id, &project.id, &prompt, now), true, false);
    let plan = Plan {
        project_id: project.id.clone(),
        view: PlanView {
            id: id.clone(),
            prompt,
            model: req.model.or_else(|| board::default_model(&project.id)),
            effort: req.effort,
            state: PlanState::Running,
            attempt_id: Some(attempt_id.clone()),
            created: Vec::new(),
            created_at: now,
        },
        created: Vec::new(),
    };
    let out = view(&plan);
    PLANS.with_borrow_mut(|plans| plans.insert(id.clone(), plan));
    changed(&project.id);
    spawn_local(watch(id, attempt_id));
    Ok(out)
}

/// Waits for the end of the plan's turn, then plays the core's end of a plan: completed → two
/// tasks on the board, awaiting (mode Default) or started; stopped → failed.
async fn watch(plan_id: Id, attempt_id: Id) {
    let completed = loop {
        super::sleep(POLL_MS).await;
        if let Some(completed) = attempt::plan_turn_completed(&attempt_id) {
            break completed;
        }
    };
    let Some(project_id) =
        PLANS.with_borrow(|plans| plans.get(&plan_id).map(|p| p.project_id.clone()))
    else {
        return;
    };
    let mut created = Vec::new();
    if completed {
        for (title, description) in CREATED {
            let after = created.last().cloned();
            if let Some(id) = board::create_planned(&project_id, title, description, after) {
                created.push(id);
            }
        }
    }
    let ask = board::find_project(&project_id)
        .is_some_and(|p| p.default_permission_mode == PermissionMode::Default);
    PLANS.with_borrow_mut(|plans| {
        let Some(plan) = plans.get_mut(&plan_id) else {
            return;
        };
        plan.view.state = match (completed, ask && !created.is_empty()) {
            (false, _) => PlanState::Failed,
            (true, true) => PlanState::Awaiting,
            (true, false) => PlanState::Started,
        };
        plan.created = created;
    });
    changed(&project_id);
}

fn get_plan(req: GetPlanReq) -> Result<Option<PlanView>, AppError> {
    board::find_project(&req.project_id)
        .ok_or_else(|| AppError::not_found(format!("progetto {}", req.project_id)))?;
    Ok(latest(&req.project_id))
}

fn resolve_plan(req: ResolvePlanReq) -> Result<PlanView, AppError> {
    let (project_id, out) = PLANS.with_borrow_mut(|plans| {
        let plan = plans
            .get_mut(&req.plan_id)
            .ok_or_else(|| AppError::not_found(format!("pianificazione {}", req.plan_id)))?;
        if plan.view.state != PlanState::Awaiting {
            return Err(AppError::invalid(
                "La pianificazione non è in attesa di conferma",
            ));
        }
        // The mock starts nothing: the tasks stay where they are.
        plan.view.state = if req.proceed {
            PlanState::Started
        } else {
            PlanState::Dismissed
        };
        Ok((plan.project_id.clone(), view(plan)))
    })?;
    changed(&project_id);
    Ok(out)
}

fn query(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        .map(str::to_owned)
}
