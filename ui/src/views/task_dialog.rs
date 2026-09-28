//! Create/edit task dialog (spec §9.2). Owner: M2-UI-BOARD.

use std::time::Duration;

use atm_types::{
    AppError, CreateTask, CreateTaskReq, DeleteTask, IdReq, Task, TaskStatus, UpdateTask,
    UpdateTaskReq,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::use_app;
use crate::ipc;
use crate::ui::button::{Button, ButtonVariant};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
};
use crate::ui::input::Input;
use crate::ui::kbd::{Kbd, KbdGroup};
use crate::ui::label::Label;
use crate::ui::textarea::Textarea;
use crate::views::board::column_title;

const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 100_000;

/// What the dialog is doing: create in a column, or edit an existing task.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskDialogMode {
    Create(TaskStatus),
    Edit(Task),
}

/// Open while `mode` is `Some`; closing or saving sets it to `None`.
#[component]
pub fn TaskDialog(mode: RwSignal<Option<TaskDialogMode>>) -> impl IntoView {
    let ctx = use_app();
    let open = RwSignal::new(false);
    let title = RwSignal::new(String::new());
    let description = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let confirm_delete = RwSignal::new(false);
    let title_ref = NodeRef::<leptos::html::Input>::new();

    // `mode` drives the ported dialog's `open`; Esc and backdrop clicks come back through it.
    Effect::new(move |_| {
        let current = mode.get();
        if let Some(m) = &current {
            let (t, d) = match m {
                TaskDialogMode::Create(_) => Default::default(),
                TaskDialogMode::Edit(task) => (task.title.clone(), task.description.clone()),
            };
            title.set(t);
            description.set(d);
            error.set(None);
            confirm_delete.set(false);
            // After the dialog has focused its first control (the close button).
            set_timeout(
                move || {
                    if let Some(el) = title_ref.try_get_untracked().flatten() {
                        let _ = el.focus();
                    }
                },
                Duration::from_millis(20),
            );
        }
        open.set(current.is_some());
    });
    Effect::new(move |_| {
        if !open.get() && mode.get_untracked().is_some() {
            mode.set(None);
        }
    });

    let is_edit = move || mode.with(|m| matches!(m, Some(TaskDialogMode::Edit(_))));
    // Cmd/Ctrl+Enter bypasses the disabled submit button.
    let submit = move || {
        if busy.get_untracked() {
            return;
        }
        let Some(current) = mode.get_untracked() else {
            return;
        };
        let (t, d) = match validate(&title.get_untracked(), description.get_untracked()) {
            Ok(v) => v,
            Err(message) => {
                error.set(Some(message.into()));
                if let Some(el) = title_ref.get_untracked() {
                    let _ = el.focus();
                }
                return;
            }
        };
        let project_id = ctx.project.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let res = match current {
                TaskDialogMode::Create(status) => match project_id {
                    Some(project_id) => ipc::call::<CreateTask>(&CreateTaskReq {
                        project_id,
                        title: t,
                        description: d,
                        status: Some(status),
                    })
                    .await
                    .map(|_| ()),
                    None => Err(AppError::invalid("Nessun progetto selezionato")),
                },
                TaskDialogMode::Edit(task) => ipc::call::<UpdateTask>(&UpdateTaskReq {
                    id: task.id,
                    title: t,
                    description: d,
                })
                .await
                .map(|_| ()),
            };
            busy.try_set(false);
            match res {
                Ok(()) => {
                    mode.try_set(None);
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    };
    let delete = move |_| {
        let Some(TaskDialogMode::Edit(task)) = mode.get_untracked() else {
            return;
        };
        if !confirm_delete.get_untracked() {
            confirm_delete.set(true);
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let res = ipc::call::<DeleteTask>(&IdReq {
                id: task.id.clone(),
            })
            .await;
            busy.try_set(false);
            match res {
                Ok(()) => {
                    if ctx.open_task.get_untracked().as_ref() == Some(&task.id) {
                        ctx.open_task.try_set(None);
                    }
                    ctx.toasts.success("Task eliminato");
                    mode.try_set(None);
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    };

    view! {
        <Dialog open=open>
            <DialogContent class="sm:max-w-lg" data_name_prefix="TaskDialog">
                <form
                    class="flex flex-col gap-4"
                    data-testid="task-dialog"
                    on:submit=move |ev| {
                        ev.prevent_default();
                        submit();
                    }
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" && (ev.meta_key() || ev.ctrl_key()) {
                            ev.prevent_default();
                            submit();
                        }
                    }
                >
                    <DialogBody>
                        <DialogHeader>
                            <DialogTitle>
                                {move || if is_edit() { "Modifica task" } else { "Nuovo task" }}
                            </DialogTitle>
                            <DialogDescription>
                                {move || match mode.get() {
                                    Some(TaskDialogMode::Create(status)) => {
                                        format!("Verrà aggiunto in fondo a «{}».", column_title(status))
                                    }
                                    _ => "Titolo e descrizione diventano il prompt dell'agente.".into(),
                                }}
                            </DialogDescription>
                        </DialogHeader>
                        <div class="flex flex-col gap-2">
                            <Label html_for="task-title">"Titolo"</Label>
                            <Input
                                id="task-title"
                                bind_value=title
                                node_ref=title_ref
                                placeholder="Es. Aggiungi la paginazione alla lista utenti"
                                autocomplete="off"
                            />
                        </div>
                        <div class="flex flex-col gap-2">
                            <Label html_for="task-description">"Descrizione"</Label>
                            <Textarea
                                id="task-description"
                                bind_value=description
                                rows=6u32
                                class="max-h-80"
                                placeholder="Cosa deve fare l'agente, vincoli, file da guardare…"
                            />
                        </div>
                        {move || {
                            error
                                .get()
                                .map(|e| {
                                    view! {
                                        <p class="text-destructive text-sm" role="alert">
                                            {e}
                                        </p>
                                    }
                                })
                        }}
                        <DialogFooter class="items-center">
                            <Show when=is_edit>
                                <Button
                                    variant=ButtonVariant::Destructive
                                    class="sm:mr-auto"
                                    attr:r#type="button"
                                    attr:disabled=move || busy.get()
                                    on:click=delete
                                >
                                    {move || {
                                        if confirm_delete.get() { "Conferma eliminazione" } else { "Elimina" }
                                    }}
                                </Button>
                            </Show>
                            <KbdGroup class="text-muted-foreground hidden sm:inline-flex">
                                <Kbd>"⌘"</Kbd>
                                <Kbd>"↩"</Kbd>
                            </KbdGroup>
                            <Button
                                variant=ButtonVariant::Outline
                                attr:r#type="button"
                                on:click=move |_| mode.set(None)
                            >
                                "Annulla"
                            </Button>
                            <Button attr:r#type="submit" attr:disabled=move || busy.get()>
                                {move || if is_edit() { "Salva" } else { "Crea task" }}
                            </Button>
                        </DialogFooter>
                    </DialogBody>
                </form>
            </DialogContent>
        </Dialog>
    }
}

/// Trimmed title of 1..=200 characters, description up to 100 000 (spec §5.2 CHECKs).
fn validate(title: &str, description: String) -> Result<(String, String), &'static str> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Il titolo è obbligatorio.");
    }
    if title.chars().count() > MAX_TITLE {
        return Err("Il titolo supera i 200 caratteri.");
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err("La descrizione supera i 100 000 caratteri.");
    }
    Ok((title.to_owned(), description))
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn titles_are_trimmed_and_bounded() {
        assert_eq!(
            validate("  Ciao  ", "d".into()),
            Ok(("Ciao".into(), "d".into()))
        );
        assert!(validate("   ", String::new()).is_err());
        assert!(validate(&"è".repeat(200), String::new()).is_ok());
        assert!(validate(&"è".repeat(201), String::new()).is_err());
        assert!(validate("t", "x".repeat(100_001)).is_err());
    }
}
