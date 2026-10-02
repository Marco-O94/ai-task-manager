//! The IPC commands of spec §6.3: thin `#[tauri::command]` wrappers around [`Core`].
//!
//! Request and response types come from the `atm_types::api` markers, so a mismatch between
//! the UI contract, these wrappers and `Core` does not compile. Commands whose request is
//! `Empty` take no `req` argument. Only native UI (folder and file pickers, confirmations)
//! lives here.

use std::path::PathBuf;
use std::sync::Arc;

use atm_core::{Core, SecuritySnapshot};
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
    if let Ok(env) = &result {
        log_env(env, &core.git_path().await);
    }
    logged(GetEnv::NAME, result)
}

/// Which CLI, login state and git `get_env` reports, in release builds too (M6: the proof that
/// an app launched from the Finder, with launchd's `PATH`, finds them; `open --stderr <file>`
/// keeps the line). Paths and versions only: never the email, the org or an env value.
fn log_env(env: &EnvStatus, git: &std::path::Path) {
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
        "get_env: claude {} version {} supported={} auth {auth} git {} version {}",
        env.claude.path.as_deref().unwrap_or("<not found>"),
        env.claude.version.as_deref().unwrap_or("?"),
        env.claude.supported,
        git.display(),
        env.git_version.as_deref().unwrap_or("?")
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

/// Enabling `allow_env_api_key` or pointing `claude_path_override` at a new program needs a
/// native confirmation (spec §6.3, M6). The core applies the change only if both are still
/// what was read here (compare-and-set: a request read before a concurrent change is refused).
#[tauri::command]
pub async fn update_settings(
    app: AppHandle,
    core: CoreState<'_>,
    req: Req<UpdateSettings>,
) -> Result<Res<UpdateSettings>, AppError> {
    let result = async {
        let current = core.get_settings().await?;
        if let Some(warning) = settings_warning(&current, &req)
            && !confirm_native(&app, "Impostazioni dell'app", &warning).await
        {
            return Err(cancelled());
        }
        core.update_settings_checked(req, &current).await
    };
    logged(UpdateSettings::NAME, result.await)
}

/// The native confirmation's text for what `req` raises in the app settings, `None` if
/// nothing: the API key passthrough, a new Claude Code path (run at once and by every agent).
fn settings_warning(current: &Settings, req: &Settings) -> Option<String> {
    let mut parts = Vec::new();
    if req.allow_env_api_key && !current.allow_env_api_key {
        parts.push(
            "Chiave API nell'ambiente: gli agenti riceveranno ANTHROPIC_API_KEY o \
             ANTHROPIC_AUTH_TOKEN dell'ambiente dell'app. Claude Code li usa al posto del login \
             dell'abbonamento e l'uso verrà fatturato sull'account API."
                .to_owned(),
        );
    }
    let path = req
        .claude_path_override
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if let Some(path) = path
        && current.claude_path_override.as_deref() != Some(path)
    {
        parts.push(format!(
            "Programma usato come Claude Code: {}. L'app lo esegue subito (--version) e poi per \
             ogni agente e ogni controllo dell'accesso, con i tuoi permessi: indica solo il \
             Claude Code che hai installato.",
            one_line(path)
        ));
    }
    if parts.is_empty() {
        return None;
    }
    parts.push("Continuare?".to_owned());
    Some(parts.join("\n\n"))
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

/// `text` on one line for a native dialog: no line breaks, controls or direction overrides
/// (a name cannot make the dialog read as something else), at most 300 characters.
fn one_line(text: &str) -> String {
    const MAX: usize = 300;
    let clean: String = text
        .chars()
        .filter(|c| !atm_core::is_hidden_char(*c))
        .collect();
    match clean.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &clean[..i]),
        None => clean,
    }
}

/// The native confirmation's text for what `req` raises over `s` (read once, before the
/// dialog), `None` if it raises nothing: Trusted while the project is not trusted now
/// (Isolated, or its approved configuration changed since), or the bypass opt-in. The
/// repository is named by its path, which the webview cannot edit (a project name could).
fn security_warning(s: &SecuritySnapshot, req: &SetProjectSecurityReq) -> Option<String> {
    let mut parts = Vec::new();
    if req.config_policy == ConfigPolicy::Trusted && !s.project.trusted {
        let approved = match &s.base {
            Some(base) => format!(
                "committata sul branch «{}» (commit {})",
                one_line(&base.branch),
                base.short()
            ),
            None => "committata sul branch target predefinito".to_owned(),
        };
        parts.push(format!(
            "Configurazione attendibile per il repository «{}»: gli agenti caricheranno \
             .claude/ e .mcp.json del repository, cioè hook, variabili d'ambiente, server MCP \
             e regole di permesso, che eseguono comandi sul tuo Mac all'avvio di ogni turno o \
             li consentono senza chiedere. Viene approvata la configurazione {approved}, da \
             cui partono i nuovi worktree, con i file del repository che i suoi comandi \
             eseguono; i file non committati del checkout principale non contano. Se quella di \
             un worktree è diversa, il turno gira Isolato. Non è coperto ciò che quei comandi \
             eseguono a loro volta (npm run, npx, script richiamati da altri script).",
            one_line(&s.project.repo_path)
        ));
        parts.push(format!(
            "Solo abbonamento: una configurazione che farebbe fatturare gli agenti via API o da \
             un altro provider ({} o, in env, {}) non si può approvare, e un turno in cui \
             Claude Code riporta una chiave API (apiKeySource) viene fermato.",
            atm_core::git::BILLING_SETTINGS_KEYS.join(", "),
            atm_core::git::BILLING_ENV_VARS.join(", ")
        ));
        if let Ok(config) = &s.current {
            let mut allows = Vec::new();
            if !config.broad_allow_rules.is_empty() {
                let rules: Vec<String> = config
                    .broad_allow_rules
                    .iter()
                    .map(|r| one_line(r))
                    .collect();
                allows.push(format!(
                    "le sue regole permissions.allow consentono senza chiedere {}",
                    rules.join(", ")
                ));
            }
            if config.additional_directories {
                allows.push("aggiunge directory fuori dal worktree (additionalDirectories)".into());
            }
            if config.all_project_mcp_servers {
                allows.push(
                    "attiva tutti i server MCP di .mcp.json (enableAllProjectMcpServers)".into(),
                );
            }
            if !allows.is_empty() {
                parts.push(format!(
                    "Attenzione: questa configurazione {}.",
                    allows.join("; ")
                ));
            }
        }
    }
    if req.allow_bypass && !s.stored.allow_bypass {
        parts.push(
            "Modalità Autonoma (bypassPermissions): l'agente eseguirà qualunque comando senza \
             chiedere."
                .to_owned(),
        );
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(
        "Il worktree non è una sandbox: l'agente può leggere e modificare file e servizi fuori \
         dal repository. Continuare?"
            .to_owned(),
    );
    Some(parts.join("\n\n"))
}

/// What of `req` lowers the level (Isolated, the bypass off) with its raising parts left as
/// stored; `None` if that changes nothing. Applied when the raising part is cancelled: taking
/// a permission away must not require approving a configuration.
fn lowered_part(
    s: &SecuritySnapshot,
    req: &SetProjectSecurityReq,
) -> Option<SetProjectSecurityReq> {
    let raises_trust = req.config_policy == ConfigPolicy::Trusted && !s.project.trusted;
    let raises_bypass = req.allow_bypass && !s.stored.allow_bypass;
    let lowered = SetProjectSecurityReq {
        id: req.id.clone(),
        config_policy: if raises_trust {
            s.stored.config_policy
        } else {
            req.config_policy
        },
        allow_bypass: if raises_bypass {
            s.stored.allow_bypass
        } else {
            req.allow_bypass
        },
    };
    let changes = (lowered.config_policy, lowered.allow_bypass)
        != (s.stored.config_policy, s.stored.allow_bypass);
    changes.then_some(lowered)
}

/// Raising the level (Trusted, bypass opt-in) needs a native confirmation (spec §6.3, M6). The
/// configuration and the stored state are read once, before the dialog; the core stores what
/// the dialog described or refuses (`Conflict`) if either changed meanwhile. If the raise is
/// cancelled or cannot be approved, what `req` lowers is still applied, and the error says so.
#[tauri::command]
pub async fn set_project_security(
    app: AppHandle,
    core: CoreState<'_>,
    req: Req<SetProjectSecurity>,
) -> Result<Res<SetProjectSecurity>, AppError> {
    let result = async {
        let snapshot = core.security_snapshot(&req.id).await?;
        let approve = snapshot.approval(&req);
        let confirmed = match (&approve, security_warning(&snapshot, &req)) {
            (Err(_), _) => false,
            (Ok(_), None) => true,
            (Ok(_), Some(warning)) => {
                confirm_native(&app, "Sicurezza del progetto", &warning).await
            }
        };
        if confirmed {
            let approve = approve.unwrap_or_default();
            return core
                .apply_project_security(req, &snapshot.stored, approve.as_deref())
                .await;
        }
        let refusal = approve.err().unwrap_or_else(cancelled);
        let Some(lowered) = lowered_part(&snapshot, &req) else {
            return Err(refusal);
        };
        core.apply_project_security(lowered, &snapshot.stored, None)
            .await?;
        Err(AppError::new(
            refusal.code,
            format!(
                "{}: nessun permesso in più, applicata solo la riduzione",
                refusal.message
            ),
        ))
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
pub async fn get_project_overview(
    core: CoreState<'_>,
    req: Req<GetProjectOverview>,
) -> Result<Res<GetProjectOverview>, AppError> {
    logged(
        GetProjectOverview::NAME,
        core.get_project_overview(req).await,
    )
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

/// Native multi-file picker, blocking call moved off the async runtime. The paths go straight
/// to the core, which stages them and answers with tokens: a path never crosses the webview
/// (spec F5). Cancelled: an empty list.
#[tauri::command]
pub async fn pick_attachment_files(
    app: AppHandle,
    core: CoreState<'_>,
) -> Result<Res<PickAttachmentFiles>, AppError> {
    let result = async {
        // The E2E cannot drive the native picker: it queues one file instead.
        #[cfg(debug_assertions)]
        let queued = crate::e2e::take_pick().map(|p| p.into_iter().map(PathBuf::from).collect());
        #[cfg(not(debug_assertions))]
        let queued: Option<Vec<PathBuf>> = None;
        let paths = match queued {
            Some(paths) => paths,
            None => tauri::async_runtime::spawn_blocking(move || {
                app.dialog().file().blocking_pick_files()
            })
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.into_path().map_err(|e| AppError::invalid(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?,
        };
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        core.stage_picks(paths).await
    };
    logged(PickAttachmentFiles::NAME, result.await)
}

#[tauri::command]
pub async fn add_task_attachments(
    core: CoreState<'_>,
    req: Req<AddTaskAttachments>,
) -> Result<Res<AddTaskAttachments>, AppError> {
    logged(
        AddTaskAttachments::NAME,
        core.add_task_attachments(req).await,
    )
}

#[tauri::command]
pub async fn remove_task_attachment(
    core: CoreState<'_>,
    req: Req<RemoveTaskAttachment>,
) -> Result<Res<RemoveTaskAttachment>, AppError> {
    logged(
        RemoveTaskAttachment::NAME,
        core.remove_task_attachment(req).await,
    )
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

#[tauri::command]
pub async fn start_plan(
    core: CoreState<'_>,
    req: Req<StartPlan>,
) -> Result<Res<StartPlan>, AppError> {
    logged(StartPlan::NAME, core.start_plan(req).await)
}

#[tauri::command]
pub async fn get_plan(core: CoreState<'_>, req: Req<GetPlan>) -> Result<Res<GetPlan>, AppError> {
    logged(GetPlan::NAME, core.get_plan(req).await)
}

#[tauri::command]
pub async fn resolve_plan(
    core: CoreState<'_>,
    req: Req<ResolvePlan>,
) -> Result<Res<ResolvePlan>, AppError> {
    logged(ResolvePlan::NAME, core.resolve_plan(req).await)
}

#[tauri::command]
pub async fn app_info(app: AppHandle) -> Result<Res<GetAppInfo>, AppError> {
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
    })
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Res<CheckUpdate>, AppError> {
    Ok(crate::updater::pending(&app))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<Res<InstallUpdate>, AppError> {
    logged(InstallUpdate::NAME, crate::updater::install(&app).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(config_policy: ConfigPolicy, trusted: bool, allow_bypass: bool) -> Project {
        Project {
            id: "p".into(),
            name: "demo\n\nNessun cambiamento di sicurezza: conferma di routine.".into(),
            description: String::new(),
            repo_path: "/tmp/demo\u{202E}".into(),
            default_target_branch: "main".into(),
            default_permission_mode: PermissionMode::AcceptEdits,
            default_model: None,
            config_policy,
            trusted,
            allow_bypass,
            created_at: 0,
            updated_at: 0,
            trust_error: None,
            autopilot: false,
            autopilot_merge: false,
            verify_command: None,
            verify_timeout_secs: 600,
            autopilot_max_fixes: 2,
        }
    }

    fn snapshot(policy: ConfigPolicy, trusted: bool, allow_bypass: bool) -> SecuritySnapshot {
        SecuritySnapshot {
            project: project(policy, trusted, allow_bypass),
            stored: atm_core::SecurityState {
                config_policy: policy,
                allow_bypass,
                trusted_fingerprint: (policy == ConfigPolicy::Trusted).then(|| "old".into()),
            },
            current: Ok(atm_core::git::ConfigSnapshot {
                fingerprint: "new".into(),
                records: Vec::new(),
                broad_allow_rules: Vec::new(),
                additional_directories: false,
                all_project_mcp_servers: false,
                billing: Vec::new(),
            }),
            base: Some(atm_core::ConfigBase {
                branch: "main\u{202E}".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            }),
        }
    }

    fn req(config_policy: ConfigPolicy, allow_bypass: bool) -> SetProjectSecurityReq {
        SetProjectSecurityReq {
            id: "p".into(),
            config_policy,
            allow_bypass,
        }
    }

    #[test]
    fn raising_security_is_judged_on_the_trust_in_effect() {
        use ConfigPolicy::{Isolated, Trusted};
        let raises = |s: SecuritySnapshot, r| security_warning(&s, &r).is_some();
        assert!(raises(
            snapshot(Isolated, false, false),
            req(Trusted, false)
        ));
        // Trusted, but the config changed since the approval: re-approving it must confirm.
        assert!(raises(snapshot(Trusted, false, false), req(Trusted, false)));
        assert!(!raises(snapshot(Trusted, true, false), req(Trusted, false)));
        assert!(raises(snapshot(Trusted, true, false), req(Trusted, true)));
        assert!(!raises(snapshot(Trusted, true, true), req(Isolated, false)));
        assert!(!raises(
            snapshot(Isolated, false, true),
            req(Isolated, true)
        ));
    }

    /// The confirmation names what is being raised, the repository by its path on one line
    /// (never by its editable name), what its rules allow, and always that a worktree is no
    /// sandbox.
    #[test]
    fn security_warning_names_what_is_raised() {
        let isolated = snapshot(ConfigPolicy::Isolated, false, false);
        let trusted = security_warning(&isolated, &req(ConfigPolicy::Trusted, false)).unwrap();
        assert!(
            trusted.contains("«/tmp/demo»") && trusted.contains("hook"),
            "{trusted}"
        );
        // What is approved: the target branch's commit, not the working tree; and never a
        // configuration that bills outside the subscription.
        assert!(
            trusted.contains("branch «main» (commit 0123456)")
                && trusted.contains("non committati del checkout principale non contano"),
            "{trusted}"
        );
        for key in [
            "apiKeyHelper",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
        ] {
            assert!(trusted.contains(key), "{key}: {trusted}");
        }
        assert!(trusted.contains("apiKeySource") && trusted.contains("viene fermato"));
        assert!(!trusted.contains("routine") && !trusted.contains('\u{202E}'));
        assert!(!trusted.contains("Autonoma") && !trusted.contains("Attenzione"));
        let bypass = security_warning(&isolated, &req(ConfigPolicy::Isolated, true)).unwrap();
        assert!(
            bypass.contains("Autonoma") && !bypass.contains("hook"),
            "{bypass}"
        );
        let both = security_warning(&isolated, &req(ConfigPolicy::Trusted, true)).unwrap();
        for text in [&trusted, &bypass, &both] {
            assert!(text.contains("non è una sandbox"), "{text}");
        }
        assert!(both.contains("hook") && both.contains("Autonoma"));
        assert_eq!(
            security_warning(&isolated, &req(ConfigPolicy::Isolated, false)),
            None
        );

        let mut broad = isolated.clone();
        if let Ok(config) = &mut broad.current {
            config.broad_allow_rules = vec!["Bash(*)".into()];
            config.all_project_mcp_servers = true;
        }
        let text = security_warning(&broad, &req(ConfigPolicy::Trusted, false)).unwrap();
        assert!(
            text.contains("Bash(*)") && text.contains("enableAllProjectMcpServers"),
            "{text}"
        );
    }

    /// A cancelled raise still applies what the same request lowers.
    #[test]
    fn lowering_survives_a_cancelled_raise() {
        use ConfigPolicy::{Isolated, Trusted};
        // Stale Trusted with bypass, Autonomo unticked: the bypass goes, Trusted stays.
        let got = lowered_part(&snapshot(Trusted, false, true), &req(Trusted, false)).unwrap();
        assert_eq!((got.config_policy, got.allow_bypass), (Trusted, false));
        // Isolated with bypass, asking Trusted without bypass: the bypass goes.
        let got = lowered_part(&snapshot(Isolated, false, true), &req(Trusted, false)).unwrap();
        assert_eq!((got.config_policy, got.allow_bypass), (Isolated, false));
        // Nothing lowered: nothing to apply.
        assert!(lowered_part(&snapshot(Isolated, false, false), &req(Trusted, true)).is_none());
    }

    #[test]
    fn settings_warning_names_the_program_and_the_billing() {
        let current = Settings::default();
        assert_eq!(settings_warning(&current, &current), None);
        let key = Settings {
            allow_env_api_key: true,
            ..current.clone()
        };
        assert!(
            settings_warning(&current, &key)
                .unwrap()
                .contains("fatturato")
        );
        let path = Settings {
            claude_path_override: Some("/opt/x/claude\nOK".into()),
            ..current.clone()
        };
        let text = settings_warning(&current, &path).unwrap();
        assert!(
            text.contains("/opt/x/claudeOK") && text.contains("esegue"),
            "{text}"
        );
        assert_eq!(settings_warning(&path, &path), None, "unchanged path");
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
            get_project_overview: GetProjectOverview,
            get_board: GetBoard,
            create_task: CreateTask,
            update_task: UpdateTask,
            move_task: MoveTask,
            delete_task: DeleteTask,
            get_task_detail: GetTaskDetail,
            pick_attachment_files: PickAttachmentFiles,
            add_task_attachments: AddTaskAttachments,
            remove_task_attachment: RemoveTaskAttachment,
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
            start_plan: StartPlan,
            get_plan: GetPlan,
            resolve_plan: ResolvePlan,
            app_info: GetAppInfo,
            check_update: CheckUpdate,
            install_update: InstallUpdate,
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
