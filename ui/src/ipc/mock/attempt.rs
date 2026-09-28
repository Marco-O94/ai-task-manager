//! Mock of the task panel commands: detail, attempts, approvals, transcript, diff, merge.
//! Owner: M2-UI-TASK (with `fixtures/*.json`: simple, approval, flood). M1 stub: every
//! command answers `NotImplemented` and subscriptions receive nothing. Tasks come from
//! `board::task`; env changes (pause, logout, running count) go through `board::update_env`.

use atm_types::{AppError, TaskCard};
use serde_json::Value;

/// Serves one attempt-side command: `req` is the serialized `C::Req`, the result the
/// serialized `C::Res`.
pub async fn handle(cmd: &str, req: Value) -> Result<Value, AppError> {
    let _ = req;
    Err(AppError::not_implemented(cmd))
}

/// A view subscribed to `attempt_id`'s transcript: replay with `super::send_transcript`
/// (a `Snapshot` first), stopping when it returns `false`.
pub fn on_subscribe(sub_id: &str, attempt_id: &str) {
    let _ = (sub_id, attempt_id);
}

/// `unsubscribe_transcript` (the channel is already detached).
pub fn on_unsubscribe(sub_id: &str) {
    let _ = sub_id;
}

/// For `board.rs`: fills the attempt fields of a card (`attempt_id`, `attempt_state`,
/// `branch`, `running`, `pending_approvals`, `last_status`, `last_stop_reason`,
/// `worktree_state`).
pub fn decorate(card: &mut TaskCard) {
    let _ = card;
}

/// For `board.rs`: the task was deleted, or its project removed; drop its attempts.
#[allow(dead_code)] // M1 stub
pub fn forget_task(task_id: &str) {
    let _ = task_id;
}
