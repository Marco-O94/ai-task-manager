//! Pure board reducers, host-tested with `cargo test -p atm-ui` (spec §9.3). Owner: M2-UI-BOARD.
#![allow(dead_code, unused_variables, clippy::ptr_arg)] // M1 stub: remove when implemented

use atm_types::{TaskCard, TaskStatus};

/// Cards of one column, in position order.
pub fn column(cards: &[TaskCard], status: TaskStatus) -> Vec<TaskCard> {
    todo!("M2-UI-BOARD: column")
}

/// Optimistic local reorder mirroring `move_task`: `id` goes into `status` before `before_id`
/// (at the end when `None`). The `changed` refetch that follows is authoritative.
pub fn apply_move(
    cards: &mut Vec<TaskCard>,
    id: &str,
    status: TaskStatus,
    before_id: Option<&str>,
) {
    todo!("M2-UI-BOARD: apply_move")
}
