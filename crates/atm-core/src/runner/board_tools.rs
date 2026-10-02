//! The board tools of the in-process MCP server `atm` (spec §7.3, §7.8): their `tools/list`
//! and their `tools/call`, scoped to the calling attempt's project. Every change goes through
//! the same `Core` method as the UI's command (validation, locks, `changed`); an [`AppError`]
//! becomes an MCP tool error (`isError`) with an English text for the model.

use std::pin::Pin;
use std::sync::Arc;

use atm_types::{
    AppError, AttemptView, CreateTaskReq, Effort, ErrorCode, Id, IdReq, MoveTaskReq, ProjectIdReq,
    StartAttemptReq, Task, TaskCard, TaskKind, TaskStatus, UpdateTaskReq, VerifyState,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::db::AttemptCtx;
use crate::{Core, Inner, now_ms};

/// Board tools a plan's agent may not call (round 2026-10-02).
const PLAN_DENIED: &[&str] = &["update_task", "move_task", "start_task"];

/// The id that names the calling attempt's own task.
const SELF_ID: &str = "self";

/// `result.tools` of `tools/list`: name, description and `inputSchema` of each tool.
pub(super) fn list() -> Value {
    let id = json!({"type": "string",
        "description": "Task id, or \"self\" for the task you are working on."});
    let status = json!({"type": "string", "enum": TaskStatus::ALL.iter().map(|s| s.as_str())
        .collect::<Vec<_>>(),
        "description": "Board column: todo, inprogress (In progress), inreview (In review), \
                        done, cancelled."});
    json!([
        {"name": "list_tasks",
         "description": "List the tasks of this project's board (id, title, status, parent, \
                         autopilot, after, agent and verification state), optionally filtered \
                         by status or parent.",
         "inputSchema": {"type": "object", "properties": {"status": status, "parent_id": id}}},
        {"name": "get_task",
         "description": "Get one task: title, description, status, parent, autopilot, after, \
                         agent and verification state and its subtasks with their status.",
         "inputSchema": {"type": "object", "properties": {"id": id}, "required": ["id"]}},
        {"name": "create_task",
         "description": "Create a task on this project's board (in todo unless a status is \
                         given). With parent_id \"self\" it is a subtask of your task; a \
                         subtask cannot have subtasks of its own. If your task is driven by \
                         the autopilot, so are your subtasks: they start on their own, each \
                         one after its `after` task is done.",
         "inputSchema": {"type": "object", "properties": {
             "title": {"type": "string"}, "description": {"type": "string"},
             "status": status, "parent_id": id,
             "after": {"type": "string", "description": "Task id, or \"self\": the new task \
                                                         starts only after that task is done."}},
             "required": ["title"]}},
        {"name": "update_task",
         "description": "Change the title and/or the description of a task. Needs the user's \
                         approval.",
         "inputSchema": {"type": "object", "properties": {"id": id,
             "title": {"type": "string"}, "description": {"type": "string"}},
             "required": ["id"]}},
        {"name": "move_task",
         "description": "Move a task to another column of the board. Needs the user's \
                         approval; a task whose agent is running cannot go to done or \
                         cancelled.",
         "inputSchema": {"type": "object", "properties": {"id": id, "status": status},
             "required": ["id", "status"]}},
        {"name": "start_task",
         "description": "Start an agent on a task, in its own worktree, with the project's \
                         defaults; a subtask's agent runs in your permission mode. Needs the \
                         user's approval. When too many agents are running it fails, or, in a \
                         project with the autopilot on, it is queued and starts on its own \
                         later. An agent started by another agent cannot start agents.",
         "inputSchema": {"type": "object", "properties": {"id": id,
             "model": {"type": "string", "description": "Model alias, e.g. sonnet or opus; \
                                                         default: the project's."},
             "effort": {"type": "string", "enum": Effort::ALL.iter().map(|e| e.as_str())
                 .collect::<Vec<_>>()}},
             "required": ["id"]}},
    ])
}

/// `result` of a `tools/call` (`params: {name, arguments}`): the tool's answer as JSON text,
/// or `isError` with the reason.
pub(super) async fn call(inner: &Arc<Inner>, ctx: &AttemptCtx, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    match run(inner, ctx, name, &params["arguments"]).await {
        Ok(answer) => json!({"content": [{"type": "text", "text": answer.to_string()}]}),
        Err(e) => json!({"content": [{"type": "text", "text": error_text(&e)}], "isError": true}),
    }
}

#[derive(Deserialize)]
struct ListArgs {
    status: Option<TaskStatus>,
    parent_id: Option<String>,
}

#[derive(Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
struct CreateArgs {
    title: String,
    #[serde(default)]
    description: String,
    status: Option<TaskStatus>,
    parent_id: Option<String>,
    after: Option<String>,
}

#[derive(Deserialize)]
struct UpdateArgs {
    id: String,
    title: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct MoveArgs {
    id: String,
    status: TaskStatus,
}

#[derive(Deserialize)]
struct StartArgs {
    id: String,
    model: Option<String>,
    effort: Option<Effort>,
}

async fn run(
    inner: &Arc<Inner>,
    ctx: &AttemptCtx,
    name: &str,
    args: &Value,
) -> Result<Value, AppError> {
    // The UI's service over the same state: one wrapper per call, nothing of its own.
    let core = Core {
        inner: Arc::clone(inner),
    };
    let project_id = ctx.project.id.clone();
    let plan = ctx.task.kind == TaskKind::Plan;
    // Denied by the plan's argv too (`claude::PLAN_DENY`): the planner only reads and creates.
    if plan && PLAN_DENIED.contains(&name) {
        return Err(AppError::invalid(
            "You are the planner: you may only list, read and create tasks.",
        ));
    }
    match name {
        "list_tasks" => {
            let a: ListArgs = parse(args)?;
            let parent = a.parent_id.map(|id| resolve(ctx, id));
            let cards = core.get_board(ProjectIdReq { project_id }).await?;
            let listed: Vec<Value> = cards
                .iter()
                .filter(|c| a.status.is_none_or(|s| c.task.status == s))
                .filter(|c| parent.is_none() || c.task.parent_id == parent)
                .map(card_json)
                .collect();
            Ok(listed.into())
        }
        "get_task" => {
            let a: IdArgs = parse(args)?;
            let task = own_task(inner, ctx, &resolve(ctx, a.id))?;
            let d = core.get_task_detail(IdReq { id: task.id }).await?;
            let parent = match &d.task.parent_id {
                Some(id) => {
                    let p = inner.db.task(id)?;
                    json!({"id": p.id, "title": p.title})
                }
                None => Value::Null,
            };
            let after = match &d.task.after_id {
                Some(id) => {
                    let a = inner.db.task(id)?;
                    json!({"id": a.id, "title": a.title, "status": a.status})
                }
                None => Value::Null,
            };
            let attempt = d.attempt.as_ref().map(|a| {
                json!({"state": a.state, "running": a.running, "branch": a.branch,
                       "pending_approvals": a.pending_approvals,
                       "verify": verify_json(a.verify_state, a.verify_fixes)})
            });
            Ok(json!({
                "id": d.task.id, "title": d.task.title, "description": d.task.description,
                "status": d.task.status, "parent": parent, "auto": d.task.auto,
                "after": after, "attempt": attempt,
                "subtasks": d.subtasks.iter().map(card_json).collect::<Vec<_>>(),
            }))
        }
        "create_task" => {
            let a: CreateArgs = parse(args)?;
            let parent = match a.parent_id.map(|id| resolve(ctx, id)) {
                Some(id) => Some(own_task(inner, ctx, &id)?),
                None => None,
            };
            // A plan adds sub-tasks only to the tasks it created, never to the user's.
            if plan
                && let Some(p) = &parent
                && !inner
                    .db
                    .plan_tasks(&ctx.task.id)?
                    .iter()
                    .any(|t| t.id == p.id)
            {
                return Err(AppError::invalid(
                    "You are the planner: you may add sub-tasks only to the tasks you created.",
                ));
            }
            let after_id = match a.after.map(|id| resolve(ctx, id)) {
                Some(id) => Some(own_task(inner, ctx, &id)?.id),
                None => None,
            };
            // A subtask of the caller's own task is the autopilot's when that task is, started
            // as if the caller had (`auto_by`): not when the caller was itself started by an
            // agent, which may not start agents (depth 2, spec §10).
            let auto = parent
                .as_ref()
                .is_some_and(|p| p.id == ctx.task.id && p.auto)
                && inner
                    .db
                    .attempt(&ctx.attempt.id)?
                    .started_by_attempt
                    .is_none();
            let req = CreateTaskReq {
                project_id: project_id.clone(),
                title: a.title,
                description: a.description,
                status: a.status,
                parent_id: parent.map(|p| p.id),
                auto: false,
                after_id,
            };
            let task = core.create_task(req).await?.task;
            if plan {
                // The plan's own: its card lists it, its end starts it (never `auto` here).
                inner.db.set_planned_by(&task.id, &ctx.task.id, now_ms())?;
                inner.emit_changed(Some(&project_id), Some(&task.id));
                return Ok(task_json(&inner.db.task(&task.id)?));
            }
            if !auto {
                return Ok(task_json(&task));
            }
            inner
                .db
                .set_task_auto_by(&task.id, &ctx.attempt.id, now_ms())?;
            inner.emit_changed(Some(&project_id), Some(&task.id));
            inner.autopilot.wake();
            Ok(task_json(&inner.db.task(&task.id)?))
        }
        "update_task" => {
            let a: UpdateArgs = parse(args)?;
            let task = own_task(inner, ctx, &resolve(ctx, a.id))?;
            let req = UpdateTaskReq {
                id: task.id,
                title: a.title.unwrap_or(task.title),
                description: a.description.unwrap_or(task.description),
                auto: None,
                after_id: None,
            };
            Ok(task_json(&core.update_task(req).await?.task))
        }
        "move_task" => {
            let a: MoveArgs = parse(args)?;
            let task = own_task(inner, ctx, &resolve(ctx, a.id))?;
            let req = MoveTaskReq {
                id: task.id.clone(),
                status: a.status,
                before_id: None,
            };
            core.move_task(req).await?;
            Ok(json!({"id": task.id, "status": a.status}))
        }
        "start_task" => {
            let a: StartArgs = parse(args)?;
            // Read now, not from the plan: the chain stops at depth 2 (spec §10).
            let caller = inner.db.attempt(&ctx.attempt.id)?;
            if caller.started_by_attempt.is_some() {
                return Err(AppError::invalid(
                    "You were started by another agent, so you cannot start agents yourself. \
                     Do the work directly or leave the task for the user to start.",
                ));
            }
            let task = own_task(inner, ctx, &resolve(ctx, a.id))?;
            let project = inner.db.project(&project_id)?;
            // A sub-task gets the caller's mode, any other task the project's.
            let permission_mode = match task.parent_id {
                Some(_) => caller.permission_mode,
                None => project.default_permission_mode,
            };
            let req = StartAttemptReq {
                task_id: task.id.clone(),
                target_branch: project.default_target_branch,
                permission_mode,
                model: a.model,
                effort: a.effort,
                subagent_model: caller.subagent_model,
                max_subagents: caller.max_subagents,
            };
            let view = match start(core, req, caller.id.clone()).await {
                // The autopilot starts it once a slot is free (it only starts tasks in todo).
                Err(e)
                    if e.code == ErrorCode::ConcurrencyLimit
                        && project.autopilot
                        && task.status == TaskStatus::Todo =>
                {
                    inner.db.set_task_auto_by(&task.id, &caller.id, now_ms())?;
                    inner.emit_changed(Some(&project_id), Some(&task.id));
                    inner.autopilot.wake();
                    return Ok(json!({"task_id": task.id, "queued": true,
                        "message": "Too many agents are running: the task is queued and the \
                                    autopilot starts it once one of them has finished."}));
                }
                result => result?,
            };
            Ok(
                json!({"task_id": task.id, "attempt_id": view.id, "branch": view.branch,
                      "permission_mode": view.permission_mode, "model": view.model}),
            )
        }
        _ => Err(AppError::not_found(format!("Unknown tool: {name}"))),
    }
}

/// [`Core::start_attempt_by`] as a boxed `Send` future: the new turn runs this very module,
/// and the compiler cannot prove the recursive future `Send` through an opaque type.
fn start(
    core: Core,
    req: StartAttemptReq,
    started_by: Id,
) -> Pin<Box<dyn Future<Output = Result<AttemptView, AppError>> + Send>> {
    Box::pin(async move { core.start_attempt_by(req, Some(started_by)).await })
}

/// The tool's `arguments` (absent = none).
fn parse<T: DeserializeOwned>(args: &Value) -> Result<T, AppError> {
    let args = if args.is_null() {
        json!({})
    } else {
        args.clone()
    };
    serde_json::from_value(args).map_err(|e| AppError::invalid(format!("Bad arguments: {e}")))
}

/// [`SELF_ID`] → the calling attempt's task.
fn resolve(ctx: &AttemptCtx, id: String) -> String {
    if id == SELF_ID {
        ctx.task.id.clone()
    } else {
        id
    }
}

/// The task `id` if it belongs to the calling attempt's project and is not a plan (hidden
/// everywhere, the caller's own included); any other is not found, so that an agent learns
/// nothing of other projects.
fn own_task(inner: &Inner, ctx: &AttemptCtx, id: &str) -> Result<Task, AppError> {
    match inner.db.task(id) {
        Ok(task) if task.project_id == ctx.project.id && task.kind == TaskKind::Task => Ok(task),
        Ok(_)
        | Err(AppError {
            code: ErrorCode::NotFound,
            ..
        }) => Err(AppError::not_found(format!(
            "No task with id {id} in this project"
        ))),
        Err(e) => Err(e),
    }
}

fn task_json(task: &Task) -> Value {
    json!({"id": task.id, "title": task.title, "status": task.status,
           "parent_id": task.parent_id, "auto": task.auto, "after_id": task.after_id})
}

/// The active attempt's verification: `null` before the first, else its state and how many
/// fixes the autopilot asked for.
fn verify_json(state: Option<VerifyState>, fixes: u32) -> Value {
    state.map_or(Value::Null, |s| json!({"state": s, "fixes": fixes}))
}

fn card_json(card: &TaskCard) -> Value {
    let mut v = task_json(&card.task);
    v["agent"] = match (card.running, card.attempt_state) {
        (true, _) => "running".into(),
        (false, Some(state)) => state.as_str().into(),
        (false, None) => Value::Null,
    };
    let verify_state = if card.verifying {
        Some(VerifyState::Running)
    } else {
        card.verify_state
    };
    v["verify"] = verify_json(verify_state, card.verify_fixes);
    if card.subtasks_total > 0 {
        v["subtasks_done"] = card.subtasks_done.into();
        v["subtasks_total"] = card.subtasks_total.into();
    }
    v
}

/// English text of a tool error: what happened, then the app's own message (Italian).
fn error_text(e: &AppError) -> String {
    let what = match e.code {
        ErrorCode::NotFound => "Not found",
        ErrorCode::Invalid => "Invalid request",
        ErrorCode::Conflict => "Conflict (the task already has an active agent attempt)",
        ErrorCode::Busy => "Busy (the task's agent is running)",
        ErrorCode::ConcurrencyLimit => {
            "Too many agents are running: start it later, once one of them has finished"
        }
        ErrorCode::UsageLimited => "The usage limit is reached: agents are paused",
        _ => "The operation failed",
    };
    format!("{what}. {:?}: {}", e.code, e.message)
}
