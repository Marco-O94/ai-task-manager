//! The planner (round 2026-10-02): a hidden task of kind `plan` whose agent explores the
//! repository read-only and fills the board (`tasks.planned_by`); at the end of its turn the
//! created tasks are started (the scheduler's `launch` flag, or `auto` with the autopilot), or
//! wait for the user's confirmation (`plan_state = awaiting`) in a project asking for
//! approvals. Its attempt is discarded once its turn is over: worktree and branch go, the
//! transcript stays.

use std::path::Path;
use std::sync::Arc;

use atm_types::{
    AppError, AttemptIdReq, ErrorCode, GetPlanReq, MODEL_ALIASES, PermissionMode, PlanState,
    PlanView, ProcessStatus, ResolvePlanReq, StartAttemptReq, StartPlanReq,
};

use crate::db::AttemptCtx;
use crate::{Core, Inner, new_id, non_empty, now_ms, repo_key, task_key};

/// Title of the planner's notifications.
const TITLE: &str = "Pianificazione";

impl Core {
    /// Inserts the plan and starts its attempt (mode Default whatever the project's, its
    /// target branch, no sub-agent limit). A start that fails leaves nothing behind: the plan
    /// is removed and the error returned. Errors: `Invalid` (empty prompt, unknown model),
    /// `NotFound`, `Conflict` (a plan running or awaiting), the errors of `start_attempt`.
    pub async fn start_plan(&self, req: StartPlanReq) -> Result<PlanView, AppError> {
        let s = &self.inner;
        let prompt = req.prompt.trim();
        if prompt.is_empty() {
            return Err(AppError::invalid("Scrivi cosa deve pianificare l'agente"));
        }
        let model = non_empty(req.model);
        if let Some(model) = &model
            && !MODEL_ALIASES.contains(&model.as_str())
        {
            return Err(AppError::invalid(format!("Modello sconosciuto: {model}")));
        }
        let project = s.db.project(&req.project_id)?;
        let id = new_id();
        s.db.insert_plan(&id, &project.id, prompt, now_ms())?;
        let start = StartAttemptReq {
            task_id: id.clone(),
            target_branch: project.default_target_branch.clone(),
            permission_mode: PermissionMode::Default,
            model,
            effort: req.effort,
            subagent_model: None,
            max_subagents: None,
        };
        if let Err(e) = self.start_attempt_by(start, None).await {
            let git = s.git().await;
            let _task = s.lock(task_key(&id)).await;
            if let Err(e) = s.remove_task(&git, &project, &id).await {
                eprintln!("plan {id}: not removed after a failed start: {e}");
            }
            return Err(e);
        }
        s.emit_changed(Some(&project.id), None);
        s.db.plan_view(&id)
    }

    /// The project's newest plan, any state. Errors: `NotFound` (project).
    pub async fn get_plan(&self, req: GetPlanReq) -> Result<Option<PlanView>, AppError> {
        let s = &self.inner;
        s.db.project(&req.project_id)?;
        s.db.latest_plan(&req.project_id)?
            .map(|id| s.db.plan_view(&id))
            .transpose()
    }

    /// The user's answer to «Avvia N task?»: `proceed` hands the created tasks still in todo
    /// to the scheduler, else the plan is dismissed and they stay where they are. Errors:
    /// `NotFound` (not a plan), `Invalid` (the plan is not awaiting).
    pub async fn resolve_plan(&self, req: ResolvePlanReq) -> Result<PlanView, AppError> {
        let s = &self.inner;
        let not_awaiting = || AppError::invalid("La pianificazione non è in attesa di conferma");
        s.db.plan_state(&req.plan_id)?;
        let _plan = s.lock(task_key(&req.plan_id)).await;
        if s.db.plan_state(&req.plan_id)? != PlanState::Awaiting {
            return Err(not_awaiting());
        }
        let plan = s.db.task(&req.plan_id)?;
        let project = s.db.project(&plan.project_id)?;
        if req.proceed {
            let ids =
                s.db.launch_plan(&plan.id, project.autopilot, now_ms())
                    .map_err(|e| match e.code {
                        ErrorCode::Conflict => not_awaiting(),
                        _ => e,
                    })?;
            for id in &ids {
                s.emit_changed(Some(&project.id), Some(id));
            }
            s.autopilot.wake();
        } else if !s.db.set_plan_state(
            &plan.id,
            &[PlanState::Awaiting],
            PlanState::Dismissed,
            now_ms(),
        )? {
            return Err(not_awaiting());
        }
        s.emit_changed(Some(&project.id), Some(&plan.id));
        s.db.plan_view(&plan.id)
    }
}

impl Inner {
    /// The end of a plan's turn (the autopilot's follow-up, never its verification): completed
    /// → the created tasks start at once when the project does not ask for approvals
    /// (AcceptEdits, BypassPermissions) or none was created, else the plan awaits the user;
    /// anything else → failed, the created tasks untouched. Then its attempt is discarded.
    pub(crate) async fn plan_ended(self: &Arc<Self>, ctx: AttemptCtx, process_id: &str) {
        let plan_id = ctx.task.id.clone();
        {
            let _plan = self.lock(task_key(&plan_id)).await;
            self.settle_or_fail(&plan_id, process_id);
        }
        self.discard_plan_attempt(&ctx.attempt.id).await;
        self.emit_changed(Some(&ctx.project.id), Some(&plan_id));
    }

    /// [`Self::settle_plan`]; on an error the plan fails rather than stay `running` with no
    /// turn (which would refuse every new plan until a restart).
    fn settle_or_fail(&self, plan_id: &str, process_id: &str) {
        if let Err(e) = self.settle_plan(plan_id, process_id) {
            eprintln!("plan {plan_id}: {e}");
            let failed =
                self.db
                    .set_plan_state(plan_id, &[PlanState::Running], PlanState::Failed, now_ms());
            if let Err(e) = failed {
                eprintln!("plan {plan_id}: not failed either: {e}");
            }
        }
    }

    /// The plan's state after its turn (`process_id`), from `running` only.
    fn settle_plan(&self, plan_id: &str, process_id: &str) -> Result<(), AppError> {
        if self.db.plan_state(plan_id)? != PlanState::Running {
            return Ok(());
        }
        let now = now_ms();
        let plan = self.db.task(plan_id)?;
        let process = self.db.process(process_id)?;
        if process.status != ProcessStatus::Completed {
            self.db
                .set_plan_state(plan_id, &[PlanState::Running], PlanState::Failed, now)?;
            return Ok(());
        }
        // The settings as they are now.
        let project = self.db.project(&plan.project_id)?;
        let created = self.db.plan_tasks(plan_id)?;
        let at_once = matches!(
            project.default_permission_mode,
            PermissionMode::AcceptEdits | PermissionMode::BypassPermissions
        );
        if created.is_empty() || at_once {
            for id in self.db.launch_plan(plan_id, project.autopilot, now)? {
                self.emit_changed(Some(&project.id), Some(&id));
            }
            self.autopilot.wake();
        } else if self.db.set_plan_state(
            plan_id,
            &[PlanState::Running],
            PlanState::Awaiting,
            now,
        )? {
            let top = created.iter().filter(|t| t.parent_id.is_none()).count();
            let body = format!("«{}»: avviare {top} task?", plan.title);
            self.notify(Some(plan_id), TITLE, &body);
        }
        Ok(())
    }

    /// The existing discard (snapshot commit, worktree removed), then the plan's `atm/…`
    /// branch: nothing of a plan is ever merged. Errors are logged.
    pub(crate) async fn discard_plan_attempt(self: &Arc<Self>, attempt_id: &str) {
        let core = Core {
            inner: Arc::clone(self),
        };
        let req = AttemptIdReq {
            attempt_id: attempt_id.to_owned(),
        };
        if let Err(e) = core.discard_attempt(req).await {
            return eprintln!("plan attempt {attempt_id}: not discarded: {e}");
        }
        let Ok(ctx) = self.db.attempt_ctx(attempt_id) else {
            return;
        };
        let git = self.git().await;
        let _repo = self.lock(repo_key(&ctx.project.repo_path)).await;
        let repo = Path::new(&ctx.project.repo_path);
        if let Err(e) = git.delete_branch(repo, &ctx.attempt.branch).await {
            eprintln!("plan attempt {attempt_id}: branch not deleted: {e}");
        }
    }

    /// Startup, after the orphans' recovery: a plan whose turn the app closing cut fails, one
    /// whose turn completed just before settles as it would have; then the attempts of the
    /// plans whose turn is over are discarded, and the branches of closed ones deleted.
    pub(crate) async fn recover_plans(self: &Arc<Self>) {
        match self.db.stale_plans() {
            Ok(plans) => {
                for (id, process) in plans {
                    match process {
                        Some(process) => self.settle_or_fail(&id, &process),
                        None => {
                            let failed = self.db.set_plan_state(
                                &id,
                                &[PlanState::Running],
                                PlanState::Failed,
                                now_ms(),
                            );
                            if let Err(e) = failed {
                                eprintln!("plan {id}: {e}");
                            }
                        }
                    }
                    eprintln!("plan {id}: settled at startup");
                }
            }
            Err(e) => eprintln!("plans: {e}"),
        }
        match self.db.plan_attempts_left() {
            Ok(ids) => {
                for id in ids {
                    self.discard_plan_attempt(&id).await;
                }
            }
            Err(e) => eprintln!("plans: {e}"),
        }
        let branches = match self.db.closed_plan_branches() {
            Ok(branches) => branches,
            Err(e) => return eprintln!("plans: {e}"),
        };
        if branches.is_empty() {
            return;
        }
        let git = self.git().await;
        for (repo_path, branch) in branches {
            let _repo = self.lock(repo_key(&repo_path)).await;
            let repo = Path::new(&repo_path);
            if git.branch_tip(repo, &branch).await.is_ok()
                && let Err(e) = git.delete_branch(repo, &branch).await
            {
                eprintln!("plan branch {branch}: not deleted: {e}");
            }
        }
    }
}
