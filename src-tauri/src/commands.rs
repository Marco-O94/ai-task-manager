//! The IPC commands of spec §6.3: thin `#[tauri::command]` wrappers around [`Core`].
//!
//! Request and response types come from the `atm_types::api` markers, so a mismatch between
//! the UI contract, these wrappers and `Core` does not compile. Commands whose request is
//! `Empty` take no `req` argument. Only native UI (folder picker, confirmations) lives here.

use std::sync::Arc;

use atm_core::Core;
use atm_types::*;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::confirm::confirm_native;

type Req<C> = <C as Command>::Req;
type Res<C> = <C as Command>::Res;
type CoreState<'a> = State<'a, Arc<Core>>;

/// Logs a failed command on stderr (code and message only, never the request).
fn logged<T>(cmd: &str, result: Result<T, AppError>) -> Result<T, AppError> {
    if let Err(e) = &result {
        eprintln!("command {cmd} failed: {e}");
        #[cfg(debug_assertions)]
        crate::e2e::note_failure(cmd, e);
    }
    result
}

fn cancelled() -> AppError {
    AppError::invalid("Operazione annullata")
}

#[tauri::command]
pub async fn get_env(core: CoreState<'_>, req: Req<GetEnv>) -> Result<Res<GetEnv>, AppError> {
    let result = core.get_env(req).await;
    #[cfg(debug_assertions)]
    if let Ok(env) = &result {
        log_env(env);
    }
    logged(GetEnv::NAME, result)
}

/// Debug builds: which CLI and login state `get_env` reports (never the email or the org).
#[cfg(debug_assertions)]
fn log_env(env: &EnvStatus) {
    let auth = match &env.auth {
        AuthState::LoggedIn {
            auth_method,
            subscription_type,
            ..
        } => format!(
            "loggedIn (authMethod {}, subscription {})",
            auth_method.as_deref().unwrap_or("?"),
            subscription_type.as_deref().unwrap_or("?")
        ),
        AuthState::LoggedOut => "loggedOut".into(),
        AuthState::Unknown { reason } => format!("unknown ({reason})"),
    };
    eprintln!(
        "get_env: claude {} version {} supported={} auth {auth}",
        env.claude.path.as_deref().unwrap_or("<not found>"),
        env.claude.version.as_deref().unwrap_or("?"),
        env.claude.supported
    );
}

#[tauri::command]
pub async fn open_login_terminal(
    core: CoreState<'_>,
    req: Req<OpenLoginTerminal>,
) -> Result<Res<OpenLoginTerminal>, AppError> {
    logged(OpenLoginTerminal::NAME, core.open_login_terminal(req).await)
}

#[tauri::command]
pub async fn resume_agents(core: CoreState<'_>) -> Result<Res<ResumeAgents>, AppError> {
    logged(ResumeAgents::NAME, core.resume_agents().await)
}

#[tauri::command]
pub async fn get_settings(core: CoreState<'_>) -> Result<Res<GetSettings>, AppError> {
    logged(GetSettings::NAME, core.get_settings().await)
}

/// Enabling `allow_env_api_key` needs a native confirmation (spec §6.3, M6).
#[tauri::command]
pub async fn update_settings(
    app: AppHandle,
    core: CoreState<'_>,
    req: Req<UpdateSettings>,
) -> Result<Res<UpdateSettings>, AppError> {
    let result = async {
        let current = core.get_settings().await?;
        if req.allow_env_api_key
            && !current.allow_env_api_key
            && !confirm_native(
                &app,
                "Chiave API nell'ambiente",
                "Gli agenti useranno ANTHROPIC_API_KEY o ANTHROPIC_AUTH_TOKEN dell'ambiente: \
                 l'uso verrà fatturato sull'account API. Continuare?",
            )
            .await
        {
            return Err(cancelled());
        }
        core.update_settings(req).await
    };
    logged(UpdateSettings::NAME, result.await)
}

#[tauri::command]
pub async fn list_projects(core: CoreState<'_>) -> Result<Res<ListProjects>, AppError> {
    logged(ListProjects::NAME, core.list_projects().await)
}

/// Native folder picker, blocking call moved off the async runtime.
#[tauri::command]
pub async fn pick_repo_folder(app: AppHandle) -> Result<Res<PickRepoFolder>, AppError> {
    // The E2E cannot drive the native picker: it queues the folder instead.
    #[cfg(debug_assertions)]
    if let Some(picked) = crate::e2e::take_pick() {
        return Ok(picked);
    }
    let result = async {
        let picked = tauri::async_runtime::spawn_blocking(move || {
            app.dialog().file().blocking_pick_folder()
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
        picked
            .map(|p| {
                p.into_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|e| AppError::invalid(e.to_string()))
            })
            .transpose()
    };
    logged(PickRepoFolder::NAME, result.await)
}

#[tauri::command]
pub async fn add_project(
    core: CoreState<'_>,
    req: Req<AddProject>,
) -> Result<Res<AddProject>, AppError> {
    logged(AddProject::NAME, core.add_project(req).await)
}

#[tauri::command]
pub async fn update_project(
    core: CoreState<'_>,
    req: Req<UpdateProject>,
) -> Result<Res<UpdateProject>, AppError> {
    logged(UpdateProject::NAME, core.update_project(req).await)
}

/// Whether `req` grants more than `current` has in effect: Trusted while the project is not
/// trusted now (Isolated, or its approved `.claude/**`/`.mcp.json` changed since), or the
/// bypass opt-in. Re-approving changed config is exactly what the confirmation guards.
fn raises_security(current: &Project, req: &SetProjectSecurityReq) -> bool {
    let trusted_now = current.config_policy == ConfigPolicy::Trusted && current.trusted;
    (req.config_policy == ConfigPolicy::Trusted && !trusted_now)
        || (req.allow_bypass && !current.allow_bypass)
}

/// Raising the level (Trusted, bypass opt-in) needs a native confirmation (spec §6.3, M6).
#[tauri::command]
pub async fn set_project_security(
    app: AppHandle,
    core: CoreState<'_>,
    req: Req<SetProjectSecurity>,
) -> Result<Res<SetProjectSecurity>, AppError> {
    let result = async {
        let current = core.project(&req.id).await?;
        if raises_security(&current, &req)
            && !confirm_native(
                &app,
                "Sicurezza del progetto",
                "Stai concedendo più fiducia a questo repository (configurazione Claude del \
                 progetto o modalità Autonoma). Un worktree non è una sandbox. Continuare?",
            )
            .await
        {
            return Err(cancelled());
        }
        core.set_project_security(req).await
    };
    logged(SetProjectSecurity::NAME, result.await)
}

#[tauri::command]
pub async fn remove_project(
    core: CoreState<'_>,
    req: Req<RemoveProject>,
) -> Result<Res<RemoveProject>, AppError> {
    logged(RemoveProject::NAME, core.remove_project(req).await)
}

#[tauri::command]
pub async fn list_branches(
    core: CoreState<'_>,
    req: Req<ListBranches>,
) -> Result<Res<ListBranches>, AppError> {
    logged(ListBranches::NAME, core.list_branches(req).await)
}

#[tauri::command]
pub async fn get_board(core: CoreState<'_>, req: Req<GetBoard>) -> Result<Res<GetBoard>, AppError> {
    logged(GetBoard::NAME, core.get_board(req).await)
}

#[tauri::command]
pub async fn create_task(
    core: CoreState<'_>,
    req: Req<CreateTask>,
) -> Result<Res<CreateTask>, AppError> {
    logged(CreateTask::NAME, core.create_task(req).await)
}

#[tauri::command]
pub async fn update_task(
    core: CoreState<'_>,
    req: Req<UpdateTask>,
) -> Result<Res<UpdateTask>, AppError> {
    logged(UpdateTask::NAME, core.update_task(req).await)
}

#[tauri::command]
pub async fn move_task(core: CoreState<'_>, req: Req<MoveTask>) -> Result<Res<MoveTask>, AppError> {
    logged(MoveTask::NAME, core.move_task(req).await)
}

#[tauri::command]
pub async fn delete_task(
    core: CoreState<'_>,
    req: Req<DeleteTask>,
) -> Result<Res<DeleteTask>, AppError> {
    logged(DeleteTask::NAME, core.delete_task(req).await)
}

#[tauri::command]
pub async fn get_task_detail(
    core: CoreState<'_>,
    req: Req<GetTaskDetail>,
) -> Result<Res<GetTaskDetail>, AppError> {
    logged(GetTaskDetail::NAME, core.get_task_detail(req).await)
}

#[tauri::command]
pub async fn start_attempt(
    core: CoreState<'_>,
    req: Req<StartAttempt>,
) -> Result<Res<StartAttempt>, AppError> {
    logged(StartAttempt::NAME, core.start_attempt(req).await)
}

#[tauri::command]
pub async fn send_follow_up(
    core: CoreState<'_>,
    req: Req<SendFollowUp>,
) -> Result<Res<SendFollowUp>, AppError> {
    logged(SendFollowUp::NAME, core.send_follow_up(req).await)
}

#[tauri::command]
pub async fn stop_attempt(
    core: CoreState<'_>,
    req: Req<StopAttempt>,
) -> Result<Res<StopAttempt>, AppError> {
    logged(StopAttempt::NAME, core.stop_attempt(req).await)
}

#[tauri::command]
pub async fn respond_approval(
    core: CoreState<'_>,
    req: Req<RespondApproval>,
) -> Result<Res<RespondApproval>, AppError> {
    logged(RespondApproval::NAME, core.respond_approval(req).await)
}

/// The sink reports `false` once the webview side of the channel is gone.
#[tauri::command]
pub async fn subscribe_transcript(
    core: CoreState<'_>,
    req: Req<SubscribeTranscript>,
    on_event: Channel<TranscriptMsg>,
) -> Result<Res<SubscribeTranscript>, AppError> {
    let sink = Box::new(move |msg| on_event.send(msg).is_ok());
    logged(
        SubscribeTranscript::NAME,
        core.subscribe_transcript(req, sink).await,
    )
}

#[tauri::command]
pub async fn unsubscribe_transcript(
    core: CoreState<'_>,
    req: Req<UnsubscribeTranscript>,
) -> Result<Res<UnsubscribeTranscript>, AppError> {
    logged(
        UnsubscribeTranscript::NAME,
        core.unsubscribe_transcript(req).await,
    )
}

#[tauri::command]
pub async fn get_entries(
    core: CoreState<'_>,
    req: Req<GetEntries>,
) -> Result<Res<GetEntries>, AppError> {
    logged(GetEntries::NAME, core.get_entries(req).await)
}

#[tauri::command]
pub async fn get_diff(core: CoreState<'_>, req: Req<GetDiff>) -> Result<Res<GetDiff>, AppError> {
    logged(GetDiff::NAME, core.get_diff(req).await)
}

#[tauri::command]
pub async fn get_branch_status(
    core: CoreState<'_>,
    req: Req<GetBranchStatus>,
) -> Result<Res<GetBranchStatus>, AppError> {
    logged(GetBranchStatus::NAME, core.get_branch_status(req).await)
}

#[tauri::command]
pub async fn merge_attempt(
    core: CoreState<'_>,
    req: Req<MergeAttempt>,
) -> Result<Res<MergeAttempt>, AppError> {
    logged(MergeAttempt::NAME, core.merge_attempt(req).await)
}

#[tauri::command]
pub async fn discard_attempt(
    core: CoreState<'_>,
    req: Req<DiscardAttempt>,
) -> Result<Res<DiscardAttempt>, AppError> {
    logged(DiscardAttempt::NAME, core.discard_attempt(req).await)
}

#[tauri::command]
pub async fn delete_branch(
    core: CoreState<'_>,
    req: Req<DeleteBranch>,
) -> Result<Res<DeleteBranch>, AppError> {
    logged(DeleteBranch::NAME, core.delete_branch(req).await)
}

#[tauri::command]
pub async fn open_attempt(
    core: CoreState<'_>,
    req: Req<OpenAttempt>,
) -> Result<Res<OpenAttempt>, AppError> {
    logged(OpenAttempt::NAME, core.open_attempt(req).await)
}

#[tauri::command]
pub async fn open_url(core: CoreState<'_>, req: Req<OpenUrl>) -> Result<Res<OpenUrl>, AppError> {
    logged(OpenUrl::NAME, core.open_url(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raising_security_is_judged_on_the_trust_in_effect() {
        let project = |config_policy, trusted, allow_bypass| Project {
            id: "p".into(),
            name: "demo".into(),
            repo_path: "/tmp/demo".into(),
            default_target_branch: "main".into(),
            default_permission_mode: PermissionMode::AcceptEdits,
            default_model: None,
            config_policy,
            trusted,
            allow_bypass,
            created_at: 0,
            updated_at: 0,
        };
        let req = |config_policy, allow_bypass| SetProjectSecurityReq {
            id: "p".into(),
            config_policy,
            allow_bypass,
        };
        use ConfigPolicy::{Isolated, Trusted};
        assert!(raises_security(
            &project(Isolated, false, false),
            &req(Trusted, false)
        ));
        // Trusted, but the config changed since the approval: re-approving it must confirm.
        assert!(raises_security(
            &project(Trusted, false, false),
            &req(Trusted, false)
        ));
        assert!(!raises_security(
            &project(Trusted, true, false),
            &req(Trusted, false)
        ));
        assert!(raises_security(
            &project(Trusted, true, false),
            &req(Trusted, true)
        ));
        assert!(!raises_security(
            &project(Trusted, true, true),
            &req(Isolated, false)
        ));
        assert!(!raises_security(
            &project(Isolated, false, true),
            &req(Isolated, true)
        ));
    }

    /// Each marker's `NAME` is the name of the fn registered for it (Tauri routes by fn name).
    #[test]
    fn command_fns_match_marker_names() {
        macro_rules! pairs {
            ($($f:ident: $c:ty),+ $(,)?) => {{
                $(let _ = $f;)+
                vec![$((stringify!($f), <$c as Command>::NAME)),+]
            }};
        }
        let pairs = pairs![
            get_env: GetEnv,
            open_login_terminal: OpenLoginTerminal,
            resume_agents: ResumeAgents,
            get_settings: GetSettings,
            update_settings: UpdateSettings,
            list_projects: ListProjects,
            pick_repo_folder: PickRepoFolder,
            add_project: AddProject,
            update_project: UpdateProject,
            set_project_security: SetProjectSecurity,
            remove_project: RemoveProject,
            list_branches: ListBranches,
            get_board: GetBoard,
            create_task: CreateTask,
            update_task: UpdateTask,
            move_task: MoveTask,
            delete_task: DeleteTask,
            get_task_detail: GetTaskDetail,
            start_attempt: StartAttempt,
            send_follow_up: SendFollowUp,
            stop_attempt: StopAttempt,
            respond_approval: RespondApproval,
            subscribe_transcript: SubscribeTranscript,
            unsubscribe_transcript: UnsubscribeTranscript,
            get_entries: GetEntries,
            get_diff: GetDiff,
            get_branch_status: GetBranchStatus,
            merge_attempt: MergeAttempt,
            discard_attempt: DiscardAttempt,
            delete_branch: DeleteBranch,
            open_attempt: OpenAttempt,
            open_url: OpenUrl,
        ];
        for (f, name) in &pairs {
            assert_eq!(f, name);
        }
        let fns: Vec<&str> = pairs.iter().map(|(f, _)| *f).collect();
        assert_eq!(
            fns, COMMAND_NAMES,
            "every §6.3 command is covered, in table order"
        );
    }
}
