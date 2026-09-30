//! Onboarding gate (spec §7.10): CLI not found, CLI too old, not logged in, login in Terminal.
//! Owner: M2-UI-BOARD.

use std::time::Duration;

use atm_types::{
    AuthState, Empty, EnvStatus, GetEnv, GetEnvReq, GetSettings, LoginMethod, OpenLoginTerminal,
    OpenLoginTerminalReq, OpenUrl, OpenUrlReq, UpdateSettings,
};
use icons::{Copy, ExternalLink, Terminal, Zap};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::callout::{Callout, CalloutVariant};
use crate::ui::card::{Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
};
use crate::ui::input::Input;
use crate::ui::label::Label;
use crate::ui::select_native::SelectNative;
use crate::ui::spinner::Spinner;

const INSTALL_DOCS: &str = "https://docs.anthropic.com/en/docs/claude-code/setup";
/// While the login dialog is open, `auth status` is re-checked this often, for at most
/// `LOGIN_TIMEOUT_MS` (spec §7.10).
const LOGIN_POLL: Duration = Duration::from_secs(3);
const LOGIN_TIMEOUT_MS: f64 = 10.0 * 60.0 * 1000.0;

/// The gate opens once the CLI is found, its version is supported (or the user chose
/// "Continua comunque") and the user is not logged out (`Unknown` passes, as in the runner).
pub fn gate_passed(env: &EnvStatus, continue_anyway: bool) -> bool {
    env.claude.path.is_some()
        && (env.claude.supported || continue_anyway)
        && !matches!(env.auth, AuthState::LoggedOut)
}

/// Which screen of the gate is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// No successful `get_env` yet (or the gate is about to open).
    Checking,
    NoCli,
    Outdated,
    LoggedOut,
}

impl Step {
    fn of(env: Option<&EnvStatus>, continue_anyway: bool) -> Self {
        match env {
            None => Self::Checking,
            Some(env) if env.claude.path.is_none() => Self::NoCli,
            Some(env) if !env.claude.supported && !continue_anyway => Self::Outdated,
            Some(env) if env.auth == AuthState::LoggedOut => Self::LoggedOut,
            Some(_) => Self::Checking,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::NoCli => "no-cli",
            Self::Outdated => "outdated",
            Self::LoggedOut => "logged-out",
        }
    }
}

/// Blocking screen while the gate is closed. `AppCtx::env` is `None` until a check succeeds.
#[component]
pub fn Onboarding(continue_anyway: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let step = Memo::new(move |_| {
        ctx.env
            .with(|env| Step::of(env.as_ref(), continue_anyway.get()))
    });
    let checking = RwSignal::new(false);
    view! {
        <main
            class="bg-background text-foreground flex min-h-screen items-center justify-center p-8"
            data-view="onboarding"
            data-step=move || step.get().as_str()
        >
            <Card class="border-border w-full max-w-lg shadow-xs">
                <CardHeader class="flex flex-row items-center gap-3 sm:flex">
                    <span class="bg-primary text-primary-foreground grid size-9 shrink-0 place-items-center rounded-lg">
                        <Zap class="size-4" />
                    </span>
                    <div class="flex min-w-0 flex-col gap-1">
                        <CardTitle class="text-[15px] tracking-tight">"AI Task Manager"</CardTitle>
                        <CardDescription class="text-[13px]">
                            "Usa il Claude Code installato sul tuo Mac."
                        </CardDescription>
                    </div>
                </CardHeader>
                {move || match step.get() {
                    Step::Checking => view! { <CheckingStep checking /> }.into_any(),
                    Step::NoCli => view! { <NoCliStep checking /> }.into_any(),
                    Step::Outdated => view! { <OutdatedStep checking continue_anyway /> }.into_any(),
                    Step::LoggedOut => view! { <LoggedOutStep checking /> }.into_any(),
                }}
            </Card>
        </main>
    }
}

/// `get_env{force}` into `AppCtx::env`, with `checking` set meanwhile; skipped while a check
/// is in flight (a forced check can take up to 10 s).
fn recheck(ctx: AppCtx, checking: RwSignal<bool>) {
    if checking.get_untracked() {
        return;
    }
    checking.set(true);
    spawn_local(async move {
        match ipc::call::<GetEnv>(&GetEnvReq { force: true }).await {
            Ok(env) => ctx.set_env(env),
            Err(e) => ctx.toasts.app_error(&e),
        }
        checking.try_set(false);
    });
}

#[component]
fn RecheckButton(checking: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    view! {
        <Button
            attr:disabled=move || checking.get()
            attr:data-testid="recheck"
            on:click=move |_| recheck(ctx, checking)
        >
            {move || checking.get().then(|| view! { <Spinner /> })}
            "Ricontrolla"
        </Button>
    }
}

#[component]
fn CheckingStep(checking: RwSignal<bool>) -> impl IntoView {
    view! {
        <CardContent class="flex items-center gap-2">
            <Spinner />
            <p class="text-muted-foreground text-[13px]">"Verifica dell'ambiente in corso…"</p>
        </CardContent>
        <CardFooter class="justify-end">
            <RecheckButton checking />
        </CardFooter>
    }
}

#[component]
fn NoCliStep(checking: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let path = RwSignal::new(String::new());
    // "Ricontrolla" first saves a typed path as `claude_path_override`.
    let save_and_recheck = move |_| {
        let typed = path.get_untracked().trim().to_owned();
        if typed.is_empty() {
            recheck(ctx, checking);
            return;
        }
        checking.set(true);
        spawn_local(async move {
            let saved = match ipc::call::<GetSettings>(&Empty {}).await {
                Ok(mut settings) => {
                    settings.claude_path_override = Some(typed);
                    ipc::call::<UpdateSettings>(&settings).await.map(|_| ())
                }
                Err(e) => Err(e),
            };
            checking.try_set(false);
            match saved {
                Ok(()) => recheck(ctx, checking),
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    };
    let open_docs = move |_| {
        spawn_local(async move {
            if let Err(e) = ipc::call::<OpenUrl>(&OpenUrlReq {
                url: INSTALL_DOCS.into(),
            })
            .await
            {
                ctx.toasts.app_error(&e);
            }
        });
    };
    view! {
        <CardContent class="flex flex-col gap-4">
            <Callout variant=CalloutVariant::Warning class="md:mx-0" title="Claude Code non trovato">
                "Installa Claude Code seguendo la guida ufficiale di Anthropic, poi premi Ricontrolla. "
                "Se è installato in una cartella non standard, indica il percorso qui sotto."
            </Callout>
            <Button variant=ButtonVariant::Outline class="w-full" on:click=open_docs>
                <ExternalLink />
                "Apri la guida di installazione"
            </Button>
            <div class="flex flex-col gap-2">
                <Label html_for="onboarding-claude-path">"Percorso di claude"</Label>
                <Input
                    id="onboarding-claude-path"
                    bind_value=path
                    placeholder="/Users/…/.local/bin/claude"
                    autocomplete="off"
                />
            </div>
            <Problems />
        </CardContent>
        <CardFooter class="justify-end">
            <Button attr:disabled=move || checking.get() attr:data-testid="recheck" on:click=save_and_recheck>
                {move || checking.get().then(|| view! { <Spinner /> })}
                "Ricontrolla"
            </Button>
        </CardFooter>
    }
}

#[component]
fn OutdatedStep(checking: RwSignal<bool>, continue_anyway: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let versions = move || {
        ctx.env.with(|env| {
            env.as_ref()
                .map(|env| {
                    format!(
                        "Trovata la versione {}; serve almeno la {}.",
                        env.claude.version.as_deref().unwrap_or("sconosciuta"),
                        env.claude.min_version
                    )
                })
                .unwrap_or_default()
        })
    };
    view! {
        <CardContent class="flex flex-col gap-4">
            <Callout variant=CalloutVariant::Warning class="md:mx-0" title="Aggiorna Claude Code">
                {versions} " Esegui " <code>"claude update"</code> " nel Terminale, poi premi Ricontrolla."
            </Callout>
            <Problems />
        </CardContent>
        <CardFooter class="justify-end">
            <Button variant=ButtonVariant::Outline on:click=move |_| continue_anyway.set(true)>
                "Continua comunque"
            </Button>
            <RecheckButton checking />
        </CardFooter>
    }
}

#[component]
fn LoggedOutStep(checking: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let method = RwSignal::new(method_value(LoginMethod::ClaudeAi).to_owned());
    let login_open = RwSignal::new(false);
    let opening = RwSignal::new(false);
    let command = Signal::derive(move || login_command(parse_method(&method.get())));
    let login = move |_| {
        let method = parse_method(&method.get_untracked());
        opening.set(true);
        spawn_local(async move {
            match ipc::call::<OpenLoginTerminal>(&OpenLoginTerminalReq { method }).await {
                Ok(()) => {
                    login_open.try_set(true);
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
            opening.try_set(false);
        });
    };
    view! {
        <CardContent class="flex flex-col gap-4">
            <p class="text-[13px] text-pretty">
                "Accedi a Claude Code per avviare gli agenti. L'accesso avviene nel Terminale con il CLI: l'app non vede mai password, codici o token."
            </p>
            <div class="flex flex-col gap-2">
                <Label html_for="login-method">"Metodo di accesso"</Label>
                <SelectNative
                    id="login-method"
                    value=method.read_only()
                    on_change=Callback::new(move |ev| method.set(event_target_value(&ev)))
                >
                    {[LoginMethod::ClaudeAi, LoginMethod::Console, LoginMethod::Sso]
                        .into_iter()
                        .map(|m| {
                            let value = method_value(m);
                            view! {
                                <option value=value selected=move || method.with(|v| v == value)>
                                    {method_label(m)}
                                </option>
                            }
                        })
                        .collect_view()}
                </SelectNative>
            </div>
            <Button class="w-full" attr:disabled=move || opening.get() attr:data-testid="login" on:click=login>
                <Terminal />
                "Accedi con Claude Code (apre il Terminale)"
            </Button>
            <div class="flex flex-col gap-2">
                <p class="text-muted-foreground text-xs">"Oppure esegui nel Terminale:"</p>
                <CopyCommand command />
            </div>
            <Problems />
        </CardContent>
        <CardFooter class="justify-end">
            <RecheckButton checking />
        </CardFooter>
        <LoginDialog open=login_open checking command />
    }
}

/// "Completa l'accesso nel Terminale…": polls `auth status` every 3 s for at most 10 min
/// while open. The gate closes by itself once `env` says logged in.
#[component]
fn LoginDialog(
    open: RwSignal<bool>,
    checking: RwSignal<bool>,
    command: Signal<String>,
) -> impl IntoView {
    let ctx = use_app();
    let timed_out = RwSignal::new(false);
    let started = StoredValue::new(0.0_f64);
    let poller = StoredValue::new(None::<IntervalHandle>);

    let stop = move || {
        if let Some(handle) = poller.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    };
    let start = move || {
        stop();
        timed_out.set(false);
        started.set_value(js_sys::Date::now());
        let handle = set_interval_with_handle(
            move || {
                let Some(started) = started.try_get_value() else {
                    return;
                };
                if js_sys::Date::now() - started >= LOGIN_TIMEOUT_MS {
                    stop();
                    timed_out.try_set(true);
                } else {
                    recheck(ctx, checking);
                }
            },
            LOGIN_POLL,
        );
        poller.set_value(handle.ok());
    };
    Effect::new(move |_| if open.get() { start() } else { stop() });
    on_cleanup(stop);

    view! {
        <Dialog open=open class="absolute">
            <DialogContent class="shadow-md sm:max-w-md" close_on_backdrop_click=false data_name_prefix="Login">
                <DialogBody attr:data-testid="login-dialog">
                    <DialogHeader>
                        <DialogTitle>"Completa l'accesso nel Terminale…"</DialogTitle>
                        <DialogDescription>
                            "Segui le istruzioni nella finestra del Terminale che si è aperta. L'app ricontrolla l'accesso ogni 3 secondi."
                        </DialogDescription>
                    </DialogHeader>
                    {move || {
                        if timed_out.get() {
                            view! {
                                <Callout variant=CalloutVariant::Warning title="Nessun accesso rilevato">
                                    "Sono passati 10 minuti. Se hai completato l'accesso premi Ricontrolla ora, altrimenti riprova."
                                </Callout>
                            }
                                .into_any()
                        } else {
                            view! {
                                <div class="text-muted-foreground flex items-center gap-2 text-[13px]">
                                    <Spinner />
                                    "In attesa dell'accesso…"
                                </div>
                            }
                                .into_any()
                        }
                    }}
                    <div class="flex flex-col gap-2">
                        <p class="text-muted-foreground text-xs">
                            "Il Terminale non si è aperto? Esegui il comando a mano:"
                        </p>
                        <CopyCommand command />
                    </div>
                    <DialogFooter>
                        <Button variant=ButtonVariant::Outline on:click=move |_| open.set(false)>
                            "Chiudi"
                        </Button>
                        <Show when=move || timed_out.get()>
                            <Button variant=ButtonVariant::Outline on:click=move |_| start()>
                                "Riprova"
                            </Button>
                        </Show>
                        <Button attr:disabled=move || checking.get() on:click=move |_| recheck(ctx, checking)>
                            "Ricontrolla ora"
                        </Button>
                    </DialogFooter>
                </DialogBody>
            </DialogContent>
        </Dialog>
    }
}

/// A shell command shown as text, with a copy button.
#[component]
fn CopyCommand(command: Signal<String>) -> impl IntoView {
    let ctx = use_app();
    let copy = move |_| {
        let promise = window()
            .navigator()
            .clipboard()
            .write_text(&command.get_untracked());
        spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(_) => ctx.toasts.success("Comando copiato"),
                Err(_) => ctx
                    .toasts
                    .error("Copia non riuscita: seleziona il comando a mano"),
            }
        });
    };
    view! {
        <div class="bg-muted border-border flex items-center gap-2 rounded-md border py-1 pr-1 pl-3">
            <code class="flex-1 font-mono text-xs select-all" data-testid="login-command">
                {move || command.get()}
            </code>
            <Button
                variant=ButtonVariant::Ghost
                size=ButtonSize::IconSm
                attr:aria-label="Copia il comando"
                attr:title="Copia"
                on:click=copy
            >
                <Copy />
            </Button>
        </div>
    }
}

/// Non-blocking problems reported by `get_env` (e.g. an old git).
#[component]
fn Problems() -> impl IntoView {
    let ctx = use_app();
    move || {
        let problems = ctx.env.with(|e| e.as_ref().map(|e| e.problems.clone()))?;
        (!problems.is_empty()).then(|| {
            view! {
                <Callout class="md:mx-0" title="Da sistemare">
                    {problems.join(" · ")}
                </Callout>
            }
        })
    }
}

fn method_value(method: LoginMethod) -> &'static str {
    match method {
        LoginMethod::ClaudeAi => "claude_ai",
        LoginMethod::Console => "console",
        LoginMethod::Sso => "sso",
    }
}

fn method_label(method: LoginMethod) -> &'static str {
    match method {
        LoginMethod::ClaudeAi => "Account Claude (abbonamento)",
        LoginMethod::Console => "Anthropic Console",
        LoginMethod::Sso => "SSO",
    }
}

fn parse_method(value: &str) -> LoginMethod {
    match value {
        "console" => LoginMethod::Console,
        "sso" => LoginMethod::Sso,
        _ => LoginMethod::ClaudeAi,
    }
}

/// The copyable fallback: `claude auth login` with the flag of the method (spec §7.10).
fn login_command(method: LoginMethod) -> String {
    let flag = match method {
        LoginMethod::ClaudeAi => "",
        LoginMethod::Console => " --console",
        LoginMethod::Sso => " --sso",
    };
    format!("claude auth login{flag}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> EnvStatus {
        EnvStatus {
            claude: atm_types::ClaudeInfo {
                path: Some("/bin/claude".into()),
                version: Some("2.1.283".into()),
                supported: true,
                min_version: "2.1.223".into(),
                tested_version: "2.1.283".into(),
            },
            auth: AuthState::LoggedOut,
            git_version: None,
            api_key_in_env: false,
            cloud_provider_env: false,
            base_url_env: false,
            paused: None,
            running: 0,
            max_running: 2,
            problems: Vec::new(),
            checked_at: 0,
        }
    }

    #[test]
    fn steps_follow_the_gate_order() {
        let mut e = env();
        assert_eq!(Step::of(None, false), Step::Checking);
        assert_eq!(Step::of(Some(&e), false), Step::LoggedOut);
        e.claude.supported = false;
        assert_eq!(Step::of(Some(&e), false), Step::Outdated);
        assert_eq!(Step::of(Some(&e), true), Step::LoggedOut);
        e.claude.path = None;
        assert_eq!(Step::of(Some(&e), true), Step::NoCli);
        e = env();
        e.auth = AuthState::Unknown { reason: "x".into() };
        assert!(gate_passed(&e, false));
        assert_eq!(Step::of(Some(&e), false), Step::Checking);
    }

    #[test]
    fn login_methods_round_trip_and_map_to_flags() {
        for m in [
            LoginMethod::ClaudeAi,
            LoginMethod::Console,
            LoginMethod::Sso,
        ] {
            assert_eq!(parse_method(method_value(m)), m);
        }
        assert_eq!(login_command(LoginMethod::ClaudeAi), "claude auth login");
        assert_eq!(login_command(LoginMethod::Sso), "claude auth login --sso");
    }
}
