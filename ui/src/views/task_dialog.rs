//! Create/edit task dialog (spec §9.2), with the task's attachments (spec F5). Owner:
//! M2-UI-BOARD, attachments UI-TASKS.

use std::time::Duration;

use atm_types::{
    AddTaskAttachments, AddTaskAttachmentsReq, AppError, Attachment, CreateTask, CreateTaskReq,
    DeleteTask, Empty, GetTaskDetail, Id, IdReq, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_TASK,
    PickAttachmentFiles, PickedFile, RemoveTaskAttachment, Task, TaskStatus, UpdateTask,
    UpdateTaskReq,
};
use icons::{FileText, Paperclip, X};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
};
use crate::ui::input::Input;
use crate::ui::kbd::{Kbd, KbdGroup};
use crate::ui::label::Label;
use crate::ui::skeleton::Skeleton;
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
    let files = AttachmentsState::new();

    // `mode` drives the ported dialog's `open`; Esc and backdrop clicks come back through it.
    // Each opening (or closing) starts a new session of `files`: a create, save or delete
    // still in flight for an earlier opening neither clears `busy` nor closes this one.
    Effect::new(move |_| {
        let current = mode.get();
        files.reset(ctx, current.as_ref());
        busy.set(false);
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
        if busy.get_untracked() || files.busy.get_untracked() {
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
        let tokens = files.staged_tokens();
        let session = files.session.get_value();
        busy.set(true);
        spawn_local(async move {
            let res = match current {
                TaskDialogMode::Create(status) => match project_id {
                    Some(project_id) => {
                        let created = ipc::call::<CreateTask>(&CreateTaskReq {
                            project_id,
                            title: t,
                            description: d,
                            status: Some(status),
                        })
                        .await;
                        if let Ok(card) = &created {
                            add_staged(ctx, card.task.id.clone(), tokens).await;
                        }
                        created.map(|_| ())
                    }
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
            let current = files.current(session);
            if current {
                busy.try_set(false);
            }
            match res {
                Ok(()) if current => {
                    mode.try_set(None);
                }
                Ok(()) => {}
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
        let session = files.session.get_value();
        busy.set(true);
        spawn_local(async move {
            let res = ipc::call::<DeleteTask>(&IdReq {
                id: task.id.clone(),
            })
            .await;
            let current = files.current(session);
            if current {
                busy.try_set(false);
            }
            match res {
                Ok(()) => {
                    if ctx.open_task.get_untracked().as_ref() == Some(&task.id) {
                        ctx.open_task.try_set(None);
                    }
                    ctx.toasts.success("Task eliminato");
                    if current {
                        mode.try_set(None);
                    }
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    };

    view! {
        <Dialog open=open>
            <DialogContent class="overflow-y-auto sm:max-w-lg" data_name_prefix="TaskDialog">
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
                                    _ => "Titolo, descrizione e allegati diventano il prompt dell'agente.".into(),
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
                        <AttachmentsField mode files saving=busy />
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
                            <Button
                                attr:r#type="submit"
                                attr:disabled=move || busy.get() || files.busy.get()
                            >
                                {move || if is_edit() { "Salva" } else { "Crea task" }}
                            </Button>
                        </DialogFooter>
                    </DialogBody>
                </form>
            </DialogContent>
        </Dialog>
    }
}

/// Attachments of the dialog (spec F5). Create: the picks are only staged, since the task id
/// does not exist yet, and added right after `create_task`. Edit: adds and removes apply at
/// once, on the list `get_task_detail` returned when the dialog opened.
#[derive(Clone, Copy)]
struct AttachmentsState {
    staged: RwSignal<Vec<PickedFile>>,
    /// `None` while `get_task_detail` loads.
    saved: RwSignal<Option<Vec<Attachment>>>,
    /// The task has an active attempt: its agent sees new files only if a message names them.
    active_attempt: RwSignal<bool>,
    /// Files a pick left out, past the per-task limit.
    note: RwSignal<Option<String>>,
    /// A pick, add or remove in flight.
    busy: RwSignal<bool>,
    /// Bumped whenever the dialog opens or closes: replies for an earlier opening are dropped.
    session: StoredValue<u64>,
}

/// One row of the section: a staged pick (keyed by token) or a saved attachment (by id).
#[derive(Debug, Clone, PartialEq)]
struct FileRow {
    key: Id,
    name: String,
    size: u64,
}

impl AttachmentsState {
    fn new() -> Self {
        Self {
            staged: RwSignal::new(Vec::new()),
            saved: RwSignal::new(None),
            active_attempt: RwSignal::new(false),
            note: RwSignal::new(None),
            busy: RwSignal::new(false),
            session: StoredValue::new(0),
        }
    }

    fn current(self, session: u64) -> bool {
        self.session.try_get_value() == Some(session)
    }

    /// Empty state for `mode` (`None` = closed); loads the attachments of a task being edited.
    fn reset(self, ctx: AppCtx, mode: Option<&TaskDialogMode>) {
        let session = self
            .session
            .try_update_value(|s| {
                *s += 1;
                *s
            })
            .unwrap_or_default();
        self.staged.set(Vec::new());
        self.saved.set(None);
        self.active_attempt.set(false);
        self.note.set(None);
        self.busy.set(false);
        let Some(TaskDialogMode::Edit(task)) = mode else {
            return;
        };
        let req = IdReq {
            id: task.id.clone(),
        };
        spawn_local(async move {
            let res = ipc::call::<GetTaskDetail>(&req).await;
            if !self.current(session) {
                return;
            }
            match res {
                Ok(detail) => {
                    self.saved.try_set(Some(detail.attachments));
                    self.active_attempt.try_set(detail.attempt.is_some());
                }
                Err(e) => {
                    self.saved.try_set(Some(Vec::new()));
                    ctx.toasts.app_error(&e);
                }
            }
        });
    }

    fn rows(self, edit: bool) -> Vec<FileRow> {
        if edit {
            self.saved.with(|saved| {
                saved
                    .iter()
                    .flatten()
                    .map(|a| FileRow {
                        key: a.id.clone(),
                        name: a.name.clone(),
                        size: a.size,
                    })
                    .collect()
            })
        } else {
            self.staged.with(|staged| {
                staged
                    .iter()
                    .map(|f| FileRow {
                        key: f.token.clone(),
                        name: f.name.clone(),
                        size: f.size,
                    })
                    .collect()
            })
        }
    }

    fn staged_tokens(self) -> Vec<Id> {
        self.staged
            .with_untracked(|staged| staged.iter().map(|f| f.token.clone()).collect())
    }

    /// "Aggiungi file…": the native picker, then staged (create) or added at once (edit).
    /// The files over the per-task limit are left out, with a note.
    fn add(self, ctx: AppCtx, mode: RwSignal<Option<TaskDialogMode>>) {
        let Some(current) = mode.get_untracked() else {
            return;
        };
        if self.busy.get_untracked() {
            return;
        }
        let session = self.session.get_value();
        self.busy.set(true);
        self.note.set(None);
        spawn_local(async move {
            let picked = ipc::call::<PickAttachmentFiles>(&Empty {}).await;
            if !self.current(session) {
                return;
            }
            let picked = match picked {
                Ok(picked) => picked,
                Err(e) => {
                    ctx.toasts.app_error(&e);
                    self.busy.try_set(false);
                    return;
                }
            };
            let existing = match &current {
                TaskDialogMode::Create(_) => self.staged.with_untracked(Vec::len),
                TaskDialogMode::Edit(_) => self
                    .saved
                    .with_untracked(|s| s.as_ref().map_or(0, Vec::len)),
            };
            let (kept, dropped) = within_limit(existing, picked);
            if dropped > 0 {
                self.note.try_set(Some(format!(
                    "Al massimo {MAX_ATTACHMENTS_PER_TASK} allegati per task: {dropped} file non aggiunti."
                )));
            }
            match current {
                TaskDialogMode::Create(_) => {
                    self.staged.try_update(|staged| staged.extend(kept));
                }
                TaskDialogMode::Edit(task) if !kept.is_empty() => {
                    let req = AddTaskAttachmentsReq {
                        task_id: task.id,
                        tokens: kept.into_iter().map(|f| f.token).collect(),
                    };
                    match ipc::call::<AddTaskAttachments>(&req).await {
                        Ok(added) if self.current(session) => {
                            self.saved
                                .try_update(|saved| saved.get_or_insert_default().extend(added));
                        }
                        Ok(_) => return,
                        Err(e) => ctx.toasts.app_error(&e),
                    }
                }
                TaskDialogMode::Edit(_) => {}
            }
            if self.current(session) {
                self.busy.try_set(false);
            }
        });
    }

    /// "×" of a row: unstaged (create) or deleted at once (edit).
    fn remove(self, ctx: AppCtx, mode: RwSignal<Option<TaskDialogMode>>, key: Id) {
        match mode.get_untracked() {
            Some(TaskDialogMode::Create(_)) => {
                self.staged
                    .update(|staged| staged.retain(|f| f.token != key));
            }
            Some(TaskDialogMode::Edit(_)) if !self.busy.get_untracked() => {
                let session = self.session.get_value();
                self.busy.set(true);
                spawn_local(async move {
                    let res = ipc::call::<RemoveTaskAttachment>(&IdReq { id: key.clone() }).await;
                    match res {
                        Ok(()) if self.current(session) => {
                            self.saved.try_update(|saved| {
                                if let Some(saved) = saved {
                                    saved.retain(|a| a.id != key);
                                }
                            });
                        }
                        Ok(()) => {}
                        Err(e) => ctx.toasts.app_error(&e),
                    }
                    if self.current(session) {
                        self.busy.try_set(false);
                    }
                });
            }
            _ => {}
        }
    }
}

/// The attachments of a task just created from the staged `tokens`. A failure keeps the task
/// (it exists already): the toast says so.
async fn add_staged(ctx: AppCtx, task_id: Id, tokens: Vec<Id>) {
    if tokens.is_empty() {
        return;
    }
    let req = AddTaskAttachmentsReq { task_id, tokens };
    if let Err(e) = ipc::call::<AddTaskAttachments>(&req).await {
        ctx.toasts
            .error(format!("Task creato, allegati non aggiunti: {}", e.message));
    }
}

/// The section's rows are keyed (`For`): a row never changes file, so its "×" may hold the
/// key; the mode is read at click time. While the dialog `saving` (create: its staged tokens
/// are already taken) the section cannot change.
#[component]
fn AttachmentsField(
    mode: RwSignal<Option<TaskDialogMode>>,
    files: AttachmentsState,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let ctx = use_app();
    let is_edit = move || mode.with(|m| matches!(m, Some(TaskDialogMode::Edit(_))));
    let rows = Memo::new(move |_| files.rows(is_edit()));
    let loading = move || is_edit() && files.saved.with(Option::is_none);
    let limits = format!(
        "Fino a {MAX_ATTACHMENTS_PER_TASK} file, {} ciascuno",
        format_size(MAX_ATTACHMENT_BYTES)
    );

    view! {
        <div class="flex flex-col gap-2" data-testid="attachments">
            <div class="flex items-center gap-2">
                <span id="task-attachments" class="text-sm leading-none font-medium">
                    "Allegati"
                </span>
                <span class="text-muted-foreground text-xs">{limits}</span>
                <Button
                    variant=ButtonVariant::Outline
                    size=ButtonSize::Sm
                    class="ml-auto"
                    attr:r#type="button"
                    attr:data-action="add-attachment"
                    attr:disabled=move || files.busy.get() || saving.get() || loading()
                    on:click=move |_| files.add(ctx, mode)
                >
                    <Paperclip />
                    "Aggiungi file…"
                </Button>
            </div>
            <Show when=loading>
                <Skeleton class="h-8 w-full" />
            </Show>
            <Show when=move || rows.with(|r| !r.is_empty())>
                <ul class="flex max-h-40 flex-col gap-1 overflow-y-auto" aria-labelledby="task-attachments">
                    <For each=move || rows.get() key=|r| r.key.clone() let:row>
                        <li
                            class="bg-muted/50 flex items-center gap-2 rounded-md py-1 pr-1 pl-2 text-sm"
                            data-attachment=row.name.clone()
                        >
                            <FileText class="text-muted-foreground size-3.5 shrink-0" />
                            <span class="min-w-0 flex-1 truncate" title=row.name.clone()>
                                {row.name.clone()}
                            </span>
                            <span class="text-muted-foreground shrink-0 text-xs">{format_size(row.size)}</span>
                            <button
                                type="button"
                                class="text-muted-foreground hover:text-foreground rounded p-0.5 disabled:opacity-50"
                                title="Rimuovi"
                                aria-label=format!("Rimuovi {}", row.name)
                                data-action="remove-attachment"
                                disabled=move || files.busy.get() || saving.get()
                                on:click=move |_| files.remove(ctx, mode, row.key.clone())
                            >
                                <X class="size-3.5" />
                            </button>
                        </li>
                    </For>
                </ul>
            </Show>
            {move || {
                files
                    .note
                    .get()
                    .map(|note| {
                        view! {
                            <p class="text-destructive text-xs" role="alert">
                                {note}
                            </p>
                        }
                    })
            }}
            <Show when=move || is_edit() && files.active_attempt.get()>
                <p class="text-muted-foreground text-xs" data-testid="attachments-hint">
                    "La cartella allegati è visibile all'agente a ogni turno: cita i nuovi file nel messaggio."
                </p>
            </Show>
        </div>
    }
}

/// The picked files that still fit next to `existing` attachments ([`MAX_ATTACHMENTS_PER_TASK`]),
/// and how many do not.
fn within_limit<T>(existing: usize, mut picked: Vec<T>) -> (Vec<T>, usize) {
    let room = MAX_ATTACHMENTS_PER_TASK.saturating_sub(existing);
    let dropped = picked.len().saturating_sub(room);
    picked.truncate(room);
    (picked, dropped)
}

/// "812 B", "47 KB", "1,2 MB", "25 MB" (binary units, Italian decimal comma). Also used by
/// the task panel's attachment chips.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    if bytes < KB {
        return format!("{bytes} B");
    }
    let kb = (bytes + KB / 2) / KB;
    if kb < KB {
        return format!("{kb} KB");
    }
    let tenths = (bytes * 10 + MB / 2) / MB;
    match tenths % 10 {
        0 => format!("{} MB", tenths / 10),
        d => format!("{},{d} MB", tenths / 10),
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
    use super::{MAX_ATTACHMENT_BYTES, format_size, validate, within_limit};

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

    #[test]
    fn sizes_use_binary_units_and_a_decimal_comma() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1 KB");
        assert_eq!(format_size(48_213), "47 KB");
        // Just under 1 MiB rounds up to a whole MB, never "1024 KB".
        assert_eq!(format_size((1 << 20) - 1), "1 MB");
        assert_eq!(format_size(1_254_877), "1,2 MB");
        assert_eq!(format_size(MAX_ATTACHMENT_BYTES), "25 MB");
    }

    #[test]
    fn picks_past_twenty_attachments_are_left_out() {
        assert_eq!(within_limit(0, vec![1, 2, 3]), (vec![1, 2, 3], 0));
        assert_eq!(within_limit(18, vec![1, 2, 3]), (vec![1, 2], 1));
        assert_eq!(within_limit(20, vec![1]), (vec![], 1));
        // More than the limit already (e.g. added elsewhere): nothing fits.
        assert_eq!(within_limit(25, vec![1, 2]), (vec![], 2));
        assert_eq!(within_limit(3, Vec::<u8>::new()), (vec![], 0));
    }
}
