//! Settings dialog (`get_settings` / `update_settings`, spec §5.2 keys), with the selected
//! project's settings in a second tab (`project.rs`). Owner: M2-UI-BOARD.

mod project;

use atm_types::{Empty, EnvStatus, GetSettings, Settings, UpdateSettings};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::use_app;
use crate::ipc;
use crate::ui::button::{Button, ButtonVariant};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
};
use crate::ui::input::Input;
use crate::ui::label::Label;
use crate::ui::select_native::SelectNative;
use crate::ui::tabs::{Tabs, TabsContent, TabsList, TabsTrigger};
use project::ProjectSettings;

/// `--model` choices (spec §9.2); `""` = the CLI default.
const MODELS: &[(&str, &str)] = &[
    ("", "Predefinito"),
    ("opus", "opus"),
    ("sonnet", "sonnet"),
    ("fable", "fable"),
];

#[component]
pub fn SettingsDialog(open: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    view! {
        <Dialog open=open>
            <DialogContent class="overflow-y-auto sm:max-w-xl" data_name_prefix="Settings">
                <DialogBody>
                    <DialogHeader>
                        <DialogTitle>"Impostazioni"</DialogTitle>
                        <DialogDescription>
                            "Salvate sul Mac; valgono per tutti i progetti salvo dove indicato."
                        </DialogDescription>
                    </DialogHeader>
                    <Tabs default_value="app">
                        <TabsList>
                            <TabsTrigger value="app">"Generali"</TabsTrigger>
                            <TabsTrigger value="project">"Progetto"</TabsTrigger>
                        </TabsList>
                        <TabsContent value="app">
                            <AppSettings open />
                        </TabsContent>
                        <TabsContent value="project">
                            <Show
                                when=move || ctx.project.with(Option::is_some)
                                fallback=|| {
                                    view! {
                                        <p class="text-muted-foreground py-4">"Nessun progetto selezionato."</p>
                                    }
                                }
                            >
                                <ProjectSettings open />
                            </Show>
                        </TabsContent>
                    </Tabs>
                </DialogBody>
            </DialogContent>
        </Dialog>
    }
}

/// The `settings` table; reloaded every time the dialog opens.
#[component]
fn AppSettings(open: RwSignal<bool>) -> impl IntoView {
    let ctx = use_app();
    let loaded = RwSignal::new(None::<Settings>);
    let claude_path = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let max_running = RwSignal::new(String::new());
    let worktree_root = RwSignal::new(String::new());
    let editor_app = RwSignal::new(String::new());
    let remove_worktree = RwSignal::new(true);
    let allow_env_api_key = RwSignal::new(false);
    let saving = RwSignal::new(false);

    Effect::new(move |_| {
        if !open.get() {
            return;
        }
        spawn_local(async move {
            match ipc::call::<GetSettings>(&Empty {}).await {
                Ok(s) => {
                    claude_path.try_set(s.claude_path_override.clone().unwrap_or_default());
                    model.try_set(s.default_model.clone().unwrap_or_default());
                    max_running.try_set(s.max_running.to_string());
                    worktree_root.try_set(s.worktree_root.clone());
                    editor_app.try_set(s.editor_app.clone());
                    remove_worktree.try_set(s.remove_worktree_after_merge);
                    allow_env_api_key.try_set(s.allow_env_api_key);
                    loaded.try_set(Some(s));
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    });

    let save = move || {
        let Some(before) = loaded.get_untracked() else {
            return;
        };
        let settings = Settings {
            claude_path_override: non_empty(claude_path.get_untracked()),
            default_model: non_empty(model.get_untracked()),
            max_running: max_running
                .get_untracked()
                .parse()
                .unwrap_or(before.max_running),
            allow_env_api_key: allow_env_api_key.get_untracked(),
            worktree_root: worktree_root.get_untracked().trim().to_owned(),
            editor_app: editor_app.get_untracked().trim().to_owned(),
            remove_worktree_after_merge: remove_worktree.get_untracked(),
        };
        saving.set(true);
        spawn_local(async move {
            match ipc::call::<UpdateSettings>(&settings).await {
                Ok(saved) => {
                    ctx.toasts.success("Impostazioni salvate");
                    open.try_set(false);
                    // A new CLI path needs a fresh discovery.
                    ctx.refresh_env(saved.claude_path_override != before.claude_path_override);
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
            saving.try_set(false);
        });
    };

    let running_options: Vec<(String, String)> =
        (1..=6).map(|n| (n.to_string(), n.to_string())).collect();
    view! {
        <form
            class="flex flex-col gap-4 pt-2"
            data-testid="app-settings"
            on:submit=move |ev| {
                ev.prevent_default();
                save();
            }
        >
            <Field id="settings-claude-path" label="Percorso di claude" hint="Vuoto: ricerca automatica.">
                <Input
                    id="settings-claude-path"
                    bind_value=claude_path
                    placeholder="/Users/…/.local/bin/claude"
                />
            </Field>
            <p class="text-muted-foreground -mt-2 text-xs break-all" data-testid="settings-env">
                {move || ctx.env.with(|env| env.as_ref().map(env_summary))}
            </p>
            <div class="grid grid-cols-2 gap-4">
                <Field id="settings-model" label="Modello predefinito">
                    <Select id="settings-model" value=model options=owned(MODELS) />
                </Field>
                <Field id="settings-max-running" label="Agenti in parallelo">
                    <Select id="settings-max-running" value=max_running options=running_options />
                </Field>
            </div>
            <Field id="settings-worktree-root" label="Cartella dei worktree">
                <Input id="settings-worktree-root" bind_value=worktree_root />
            </Field>
            <Field id="settings-editor" label="Editor" hint="Applicazione usata da «Apri nell'editor».">
                <Input id="settings-editor" bind_value=editor_app />
            </Field>
            <Checkbox id="settings-remove-worktree" checked=remove_worktree>
                "Rimuovi il worktree dopo il merge (il branch resta)"
            </Checkbox>
            <Checkbox id="settings-api-key" checked=allow_env_api_key>
                "Passa agli agenti la chiave API dell'ambiente (fatturata via API, chiede conferma)"
            </Checkbox>
            <DialogFooter>
                <Button
                    variant=ButtonVariant::Outline
                    attr:r#type="button"
                    on:click=move |_| open.set(false)
                >
                    "Annulla"
                </Button>
                <Button
                    attr:r#type="submit"
                    attr:data-action="save-settings"
                    attr:disabled=move || saving.get() || loaded.with(Option::is_none)
                >
                    "Salva"
                </Button>
            </DialogFooter>
        </form>
    }
}

/// Label, control and optional hint.
#[component]
fn Field(
    id: &'static str,
    label: &'static str,
    #[prop(optional)] hint: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-2">
            <Label html_for=id>{label}</Label>
            {children()}
            {hint.map(|h| view! { <p class="text-muted-foreground text-xs">{h}</p> })}
        </div>
    }
}

#[component]
fn Checkbox(id: &'static str, checked: RwSignal<bool>, children: Children) -> impl IntoView {
    view! {
        <label for=id class="flex items-start gap-2 text-sm">
            <input
                type="checkbox"
                id=id
                class="accent-primary mt-0.5 size-4 shrink-0"
                prop:checked=move || checked.get()
                on:change=move |ev| checked.set(event_target_checked(&ev))
            />
            <span>{children()}</span>
        </label>
    }
}

/// `SelectNative` bound to `value`; `options` are `(value, label)`. Each option also carries
/// `selected`, so the choice shows even when the value is set before the options exist.
#[component]
fn Select(
    id: &'static str,
    value: RwSignal<String>,
    #[prop(into)] options: Signal<Vec<(String, String)>>,
    #[prop(optional)] disabled: Option<Signal<Vec<String>>>,
) -> impl IntoView {
    view! {
        <SelectNative
            id=id
            value=value.read_only()
            on_change=Callback::new(move |ev| value.set(event_target_value(&ev)))
        >
            <For each=move || options.get() key=|o| o.clone() let:option>
                {
                    let (v, label) = option;
                    let selected = {
                        let v = v.clone();
                        move || value.with(|current| *current == v)
                    };
                    let off = {
                        let v = v.clone();
                        move || disabled.is_some_and(|d| d.with(|d| d.contains(&v)))
                    };
                    view! {
                        <option value=v selected=selected disabled=off>
                            {label}
                        </option>
                    }
                }
            </For>
        </SelectNative>
    }
}

/// The CLI and the git the app found (spec §7.1, §8.1): an app launched from the Finder does not
/// have the Terminal's `PATH`, so this says which ones it is using.
fn env_summary(env: &EnvStatus) -> String {
    let claude = match (&env.claude.path, &env.claude.version) {
        (Some(path), Some(version)) => format!("Claude Code {version} ({path})"),
        (Some(path), None) => format!("Claude Code ({path})"),
        (None, _) => "Claude Code non trovato".to_owned(),
    };
    let git = env
        .git_version
        .as_ref()
        .map_or_else(|| "git non disponibile".to_owned(), |v| format!("git {v}"));
    format!("In uso: {claude}; {git}.")
}

fn owned(options: &[(&str, &str)]) -> Vec<(String, String)> {
    options
        .iter()
        .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
        .collect()
}

fn non_empty(s: String) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

#[cfg(test)]
mod tests {
    use atm_types::{AuthState, ClaudeInfo};

    use super::*;

    fn env(path: Option<&str>, version: Option<&str>, git: Option<&str>) -> EnvStatus {
        EnvStatus {
            claude: ClaudeInfo {
                path: path.map(Into::into),
                version: version.map(Into::into),
                supported: true,
                min_version: "2.1.0".into(),
                tested_version: "2.1.283".into(),
            },
            auth: AuthState::LoggedOut,
            git_version: git.map(Into::into),
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
    fn env_summary_names_the_cli_and_git_in_use() {
        let found = env(
            Some("/u/.local/bin/claude"),
            Some("2.1.284"),
            Some("2.54.0"),
        );
        assert_eq!(
            env_summary(&found),
            "In uso: Claude Code 2.1.284 (/u/.local/bin/claude); git 2.54.0."
        );
        assert_eq!(
            env_summary(&env(None, None, None)),
            "In uso: Claude Code non trovato; git non disponibile."
        );
    }
}
