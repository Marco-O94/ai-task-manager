//! Onboarding gate (spec §7.10): CLI not found, CLI too old, not logged in, login in Terminal.
//! Owner: M2-UI-BOARD.

use atm_types::{AuthState, EnvStatus};
use leptos::prelude::*;

use crate::app::use_app;
use crate::ui::button::Button;
use crate::ui::card::{Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle};

/// The gate opens once the CLI is found, its version is supported (or the user chose
/// "Continua comunque") and the user is not logged out (`Unknown` passes, as in the runner).
pub fn gate_passed(env: &EnvStatus, continue_anyway: bool) -> bool {
    env.claude.path.is_some()
        && (env.claude.supported || continue_anyway)
        && !matches!(env.auth, AuthState::LoggedOut)
}

/// Blocking screen while the gate is closed. `AppCtx::env` is `None` until a check succeeds.
#[component]
pub fn Onboarding(continue_anyway: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let _ = continue_anyway;
    view! {
        <main class="flex h-screen items-center justify-center p-8" data-view="onboarding">
            <Card class="w-full max-w-md">
                <CardHeader>
                    <CardTitle>"AI Task Manager"</CardTitle>
                    <CardDescription>"Usa il Claude Code installato sul tuo Mac."</CardDescription>
                </CardHeader>
                <CardContent>
                    <p class="text-muted-foreground text-sm">"Verifica dell'ambiente in corso…"</p>
                </CardContent>
                <CardFooter>
                    <Button on:click=move |_| ctx.refresh_env(true)>"Ricontrolla"</Button>
                </CardFooter>
            </Card>
        </main>
    }
}
