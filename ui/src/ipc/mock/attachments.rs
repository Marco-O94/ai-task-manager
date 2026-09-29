//! Mock of the task attachments (spec F5): `pick_attachment_files`, `add_task_attachments`,
//! `remove_task_attachment`, and the list `attempt.rs` puts in `TaskDetail`. Owner: UI-TASKS.
//! Attachments live in memory; `task-inreview` starts with two, so the panel's chips show at
//! once (`/?task=task-inreview`). The picker cycles through [`PICKS`], so repeated clicks on
//! "Aggiungi file…" cover one file, several files, a cancelled picker and a file too large.

use std::cell::RefCell;
use std::collections::HashMap;

use atm_types::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{board, emit, new_id, now_ms};

/// Where the app keeps the copies (`app_data_dir` of the bundle).
const DATA_DIR: &str = "/Users/demo/Library/Application Support/dev.aitaskmanager.desktop";

/// Successive answers of the picker `(name, bytes)`, then again from the first: one image,
/// two documents, a cancelled picker (no file), one file over [`MAX_ATTACHMENT_BYTES`]
/// (`Invalid`, as the core's staging refuses it).
const PICKS: &[&[(&str, u64)]] = &[
    &[("schema-db.png", 48_213)],
    &[
        ("specifiche-api.pdf", 1_254_877),
        ("errori-console.txt", 3_412),
    ],
    &[],
    &[("registrazione-demo.mov", 41_943_040)],
];

/// `(task, name, bytes)` attached at startup.
const SEED: &[(&str, &str, u64)] = &[
    ("task-inreview", "stacktrace.txt", 6_120),
    ("task-inreview", "screenshot-errore.png", 312_874),
];
/// The seeded tasks' project (the mock's ids are fixed; `board.rs` is not called while
/// this state is being built).
const SEED_PROJECT: &str = "project-demo";

struct State {
    /// Files of the last picks by token, each redeemable once.
    picked: HashMap<Id, PickedFile>,
    /// Every attachment, oldest first.
    attachments: Vec<Attachment>,
    /// Picks so far: the next answer is `PICKS[picks % PICKS.len()]`.
    picks: usize,
}

impl State {
    fn seed() -> Self {
        let attachments = SEED
            .iter()
            .map(|&(task_id, name, size)| attachment(SEED_PROJECT, task_id, name, size))
            .collect();
        Self {
            picked: HashMap::new(),
            attachments,
            picks: 0,
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::seed());
}

pub fn handle(cmd: &str, req: Value) -> Result<Value, AppError> {
    match cmd {
        PickAttachmentFiles::NAME => reply(pick()),
        AddTaskAttachments::NAME => reply(add(parse(req)?)),
        RemoveTaskAttachment::NAME => reply(remove(&parse::<IdReq>(req)?.id)),
        _ => Err(AppError::not_implemented(cmd)),
    }
}

/// For `attempt.rs`: the task's attachments, oldest first.
pub fn of_task(task_id: &str) -> Vec<Attachment> {
    STATE.with_borrow(|s| {
        s.attachments
            .iter()
            .filter(|a| a.task_id == task_id)
            .cloned()
            .collect()
    })
}

/// For `board.rs`: drops a deleted task's attachments, as the core removes their folder with
/// the task (`delete_task`, `remove_project`). Emits nothing: the caller does.
pub fn forget_task(task_id: &str) {
    STATE.with_borrow_mut(|s| s.attachments.retain(|a| a.task_id != task_id));
}

fn parse<T: DeserializeOwned>(req: Value) -> Result<T, AppError> {
    Ok(serde_json::from_value(req)?)
}

fn reply<T: Serialize>(res: Result<T, AppError>) -> Result<Value, AppError> {
    Ok(serde_json::to_value(res?)?)
}

fn attachment(project_id: &str, task_id: &str, name: &str, size: u64) -> Attachment {
    let id = new_id();
    Attachment {
        path: format!("{DATA_DIR}/attachments/{project_id}/{task_id}/{id}/{name}"),
        id,
        task_id: task_id.into(),
        name: name.into(),
        size,
        created_at: now_ms(),
    }
}

fn pick() -> Result<Vec<PickedFile>, AppError> {
    STATE.with_borrow_mut(|s| {
        let files = PICKS[s.picks % PICKS.len()];
        s.picks += 1;
        if let Some((name, _)) = files.iter().find(|(_, size)| *size > MAX_ATTACHMENT_BYTES) {
            return Err(AppError::invalid(format!(
                "«{name}» è troppo grande: il massimo è {} MB",
                MAX_ATTACHMENT_BYTES >> 20
            )));
        }
        let picked: Vec<PickedFile> = files
            .iter()
            .map(|&(name, size)| PickedFile {
                token: new_id(),
                name: name.into(),
                size,
            })
            .collect();
        for file in &picked {
            s.picked.insert(file.token.clone(), file.clone());
        }
        Ok(picked)
    })
}

fn add(req: AddTaskAttachmentsReq) -> Result<Vec<Attachment>, AppError> {
    let task = board::task(&req.task_id)
        .ok_or_else(|| AppError::not_found(format!("Task {} non trovato.", req.task_id)))?;
    let added = STATE.with_borrow_mut(|s| {
        let files =
            req.tokens
                .iter()
                .map(|t| {
                    s.picked.get(t).cloned().ok_or_else(|| {
                        AppError::invalid("File non più disponibile: sceglilo di nuovo")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
        let count = s
            .attachments
            .iter()
            .filter(|a| a.task_id == task.id)
            .count();
        if count + files.len() > MAX_ATTACHMENTS_PER_TASK {
            return Err(AppError::invalid(format!(
                "Un task può avere al massimo {MAX_ATTACHMENTS_PER_TASK} allegati"
            )));
        }
        let added: Vec<Attachment> = files
            .into_iter()
            .map(|f| attachment(&task.project_id, &task.id, &f.name, f.size))
            .collect();
        for t in &req.tokens {
            s.picked.remove(t);
        }
        s.attachments.extend(added.iter().cloned());
        Ok(added)
    })?;
    changed(task);
    Ok(added)
}

fn remove(id: &str) -> Result<(), AppError> {
    let removed = STATE.with_borrow_mut(|s| {
        let at = s.attachments.iter().position(|a| a.id == id)?;
        Some(s.attachments.remove(at))
    });
    let removed =
        removed.ok_or_else(|| AppError::not_found(format!("Allegato non trovato: {id}")))?;
    if let Some(task) = board::task(&removed.task_id) {
        changed(task);
    }
    Ok(())
}

fn changed(task: Task) {
    let payload = Changed {
        project_id: Some(task.project_id),
        task_id: Some(task.id),
    };
    emit(EVENT_CHANGED, &payload);
}
