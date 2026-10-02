//! App version and updates (ARCHITECTURE.md «Versione e aggiornamenti»): the version in the
//! sidebar footer and in "Impostazioni app", the banner of an optional update, the blocking
//! modal of a required one. The shell checks (startup, every 6 h); here only `app_info`,
//! `check_update`, the `update_available` event and `install_update`.

use atm_types::{
    AppInfo, CheckUpdate, EVENT_UPDATE_AVAILABLE, Empty, GetAppInfo, InstallUpdate, OpenUrl,
    OpenUrlReq, RELEASES_URL, UpdateInfo,
};
use icons::{Download, X};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::spinner::Spinner;
use crate::widgets::status::FOCUS_RING;

/// Version and update state, provided by [`provide_update_ctx`] for the app's lifetime.
#[derive(Clone, Copy)]
pub struct UpdateCtx {
    /// `app_info`'s version, `None` until it arrives.
    pub version: RwSignal<Option<String>>,
    /// The update the shell found, if any.
    pub update: RwSignal<Option<UpdateInfo>>,
    /// The optional version whose banner was closed (in memory: it comes back at the next
    /// launch, or for a newer version).
    dismissed: RwSignal<Option<String>>,
    /// An `install_update` is running (it never returns on success: the app restarts).
    installing: RwSignal<bool>,
    /// The last install's error.
    error: RwSignal<Option<String>>,
    /// «Continua per ora» after a failed install of a required update: the modal gives way to
    /// the banner until the next launch.
    deferred: RwSignal<bool>,
}

pub fn use_update() -> UpdateCtx {
    expect_context()
}

/// Provides [`UpdateCtx`] and fills it: `app_info`, `check_update`, then every
/// `update_available`. Failures are only logged: an app that cannot tell is an app up to date.
pub fn provide_update_ctx() {
    let ctx = UpdateCtx {
        version: RwSignal::new(None),
        update: RwSignal::new(None),
        dismissed: RwSignal::new(None),
        installing: RwSignal::new(false),
        error: RwSignal::new(None),
        deferred: RwSignal::new(false),
    };
    provide_context(ctx);
    spawn_local(async move {
        match ipc::call::<GetAppInfo>(&Empty {}).await {
            Ok(AppInfo { version }) => {
                ctx.version.try_set(Some(version));
            }
            Err(e) => leptos::logging::warn!("app_info: {e}"),
        }
        // `None`: a later check found nothing (the release was pulled).
        match ipc::listen::<Option<UpdateInfo>>(EVENT_UPDATE_AVAILABLE, move |info| {
            ctx.update.try_set(info);
        })
        .await
        {
            // App-lifetime listener: never unlistened.
            Ok((closure, _unlisten)) => closure.forget(),
            Err(e) => leptos::logging::warn!("listen {EVENT_UPDATE_AVAILABLE}: {e}"),
        }
        match ipc::call::<CheckUpdate>(&Empty {}).await {
            Ok(Some(info)) => {
                ctx.update.try_set(Some(info));
            }
            Ok(None) => {}
            Err(e) => leptos::logging::warn!("check_update: {e}"),
        }
    });
}

impl UpdateCtx {
    /// `install_update`: on success the shell stops the agents and restarts the app, so the
    /// spinner stays until the window goes away.
    fn install(self) {
        if self.installing.get_untracked() {
            return;
        }
        self.installing.set(true);
        self.error.set(None);
        spawn_local(async move {
            if let Err(e) = ipc::call::<InstallUpdate>(&Empty {}).await {
                self.error.try_set(Some(e.message));
                self.installing.try_set(false);
            }
        });
    }
}

/// A required update blocks the app (the modal is up): the layout under it goes `inert`.
pub fn blocks_app(ctx: UpdateCtx) -> bool {
    !ctx.deferred.get() && ctx.update.with(|u| u.as_ref().is_some_and(|u| u.required))
}

/// The release page in the browser, for a manual download.
fn open_releases() {
    spawn_local(async move {
        let req = OpenUrlReq {
            url: RELEASES_URL.to_owned(),
        };
        if let Err(e) = ipc::call::<OpenUrl>(&req).await {
            leptos::logging::warn!("open_url: {e}");
        }
    });
}

/// `v0.1.0`, or nothing until `app_info` answers.
pub fn version_label(ctx: UpdateCtx) -> Option<String> {
    ctx.version.get().map(|v| format!("v{v}"))
}

/// Blocking modal of a required update: no way around it but «Aggiorna e riavvia», until an
/// install fails (here or before the last restart): then also the release page and «Continua
/// per ora». Mounted at the root, over the onboarding too.
#[component]
pub fn RequiredUpdate() -> impl IntoView {
    let ctx = use_update();
    let required = Memo::new(move |_| {
        ctx.update
            .with(|u| u.as_ref().filter(|u| u.required).cloned())
            .filter(|_| !ctx.deferred.get())
    });
    // Rebuilt only when the announced update changes.
    move || required.get().map(|info| view! { <RequiredModal info /> })
}

#[component]
fn RequiredModal(info: UpdateInfo) -> impl IntoView {
    let ctx = use_update();
    let button = NodeRef::<html::Button>::new();
    // This install's error, else the one of the install before the last restart.
    let before = info.install_error.clone();
    let error = Memo::new(move |_| ctx.error.get().or_else(|| before.clone()));
    // The only control: it takes the focus.
    Effect::new(move |_| {
        if let Some(b) = button.get() {
            let _ = b.focus();
        }
    });
    view! {
        <div
            class="fixed inset-0 z-[100] grid place-items-center bg-black/50 p-6 backdrop-blur-[2px]"
            data-testid="update-required"
        >
            <div
                role="alertdialog"
                aria-modal="true"
                aria-labelledby="update-required-title"
                aria-describedby="update-required-text"
                class="bg-background text-foreground border-border flex w-full max-w-md flex-col gap-4 rounded-xl border p-6 shadow-lg"
            >
                <div class="flex flex-col gap-2">
                    <h2 id="update-required-title" class="text-lg leading-none font-semibold">
                        "Aggiornamento richiesto"
                    </h2>
                    <p id="update-required-text" class="text-muted-foreground text-sm">
                        {format!(
                            "La v{} non è compatibile con la versione in uso (v{}): aggiorna per continuare. Gli agenti in esecuzione verranno fermati e l'app si riavvierà.",
                            info.latest,
                            info.current,
                        )}
                    </p>
                </div>
                {info
                    .notes
                    .map(|notes| {
                        view! {
                            <p class="bg-muted text-muted-foreground max-h-40 overflow-y-auto rounded-md p-3 text-xs whitespace-pre-wrap">
                                {notes}
                            </p>
                        }
                    })}
                <Show when=move || error.with(Option::is_some)>
                    <p class="text-destructive text-sm" role="alert" data-testid="update-error">
                        {move || error.get()}
                    </p>
                    <p class="text-muted-foreground text-xs">
                        "Puoi scaricarla dalla pagina delle release e installarla a mano, o continuare con questa versione per ora."
                    </p>
                </Show>
                <div class="flex flex-wrap items-center justify-end gap-3">
                    <Show when=move || error.with(Option::is_some) && !ctx.installing.get()>
                        <button
                            type="button"
                            class=format!(
                                "text-muted-foreground hover:text-foreground rounded-md px-2 text-sm underline-offset-2 hover:underline {FOCUS_RING}",
                            )
                            data-action="defer-update"
                            on:click=move |_| ctx.deferred.set(true)
                        >
                            "Continua per ora"
                        </button>
                        <button
                            type="button"
                            class=format!(
                                "border-border hover:bg-accent inline-flex h-9 items-center rounded-md border px-3 text-sm font-medium {FOCUS_RING}",
                            )
                            data-action="open-releases"
                            on:click=move |_| open_releases()
                        >
                            "Pagina delle release"
                        </button>
                    </Show>
                    <Show when=move || ctx.installing.get()>
                        <span class="text-muted-foreground flex items-center gap-2 text-sm" role="status">
                            <Spinner class="size-4" />
                            "Download e installazione…"
                        </span>
                    </Show>
                    <button
                        type="button"
                        node_ref=button
                        class=format!(
                            "bg-primary text-primary-foreground hover:bg-primary/90 inline-flex h-9 items-center gap-2 rounded-md px-4 text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 {FOCUS_RING}",
                        )
                        disabled=move || ctx.installing.get()
                        data-action="install-update"
                        on:click=move |_| ctx.install()
                    >
                        <Download />
                        {move || {
                            if error.with(Option::is_some) {
                                "Riprova"
                            } else {
                                "Aggiorna e riavvia"
                            }
                        }}
                    </button>
                </div>
            </div>
        </div>
    }
}

/// Banner of an optional update, under the topbar, or of a required one put off with
/// «Continua per ora»; closed per version.
#[component]
pub fn UpdateBanner() -> impl IntoView {
    let ctx = use_update();
    let latest = Memo::new(move |_| {
        let latest = ctx.update.with(|u| {
            u.as_ref()
                .filter(|u| !u.required || ctx.deferred.get())
                .map(|u| u.latest.clone())
        })?;
        let dismissed = ctx.dismissed.with(|d| d.as_ref() == Some(&latest));
        (!dismissed).then_some(latest)
    });
    // Built once behind the memoized `Show`: a rebuilt `<Button on:click>` would keep its old
    // handler next to the new one (Leptos 0.8).
    view! {
        <Show when=move || latest.with(Option::is_some)>
            <div
                class="bg-info/8 border-info/25 flex items-center gap-3 border-b px-5 py-2 text-[13px]"
                role="status"
                data-banner="update"
            >
                <Download class="text-info size-4 shrink-0" />
                <span class="flex-1">
                    {move || latest.get().map(|v| format!("Disponibile la v{v}, aggiorna"))}
                    " (gli agenti in esecuzione verranno fermati e l'app si riavvierà)."
                    {move || ctx.error.get().map(|e| format!(" {e}"))}
                </span>
                <Show when=move || ctx.installing.get()>
                    <Spinner class="size-4" />
                </Show>
                <Button
                    size=ButtonSize::Sm
                    variant=ButtonVariant::Outline
                    class="h-7 text-[13px]"
                    attr:disabled=move || ctx.installing.get()
                    attr:data-action="install-update"
                    on:click=move |_| ctx.install()
                >
                    "Aggiorna e riavvia"
                </Button>
                <button
                    type="button"
                    class=format!(
                        "text-muted-foreground hover:text-foreground rounded-sm p-1 [&_svg]:size-3.5 {FOCUS_RING}",
                    )
                    aria-label="Nascondi"
                    title="Nascondi"
                    data-action="dismiss-update"
                    on:click=move |_| ctx.dismissed.set(latest.get_untracked())
                >
                    <X />
                </button>
            </div>
        </Show>
    }
}

/// The version line of "Impostazioni app", with the update the shell found.
#[component]
pub fn SettingsVersion() -> impl IntoView {
    let ctx = use_update();
    view! {
        <p class="text-muted-foreground text-xs" data-testid="settings-app-version">
            {move || {
                let version = version_label(ctx).unwrap_or_else(|| "…".to_owned());
                match ctx.update.get() {
                    Some(u) => format!("AI Task Manager {version} · disponibile la v{}", u.latest),
                    None => format!("AI Task Manager {version}"),
                }
            }}
        </p>
    }
}
