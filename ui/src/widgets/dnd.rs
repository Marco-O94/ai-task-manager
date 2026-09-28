//! HTML5 drag-and-drop of the kanban (spec §9.3). Owner: M2-UI-BOARD.
#![allow(dead_code, unused_variables)] // M1 stub: remove when implemented

use atm_types::{Id, TaskStatus};
use leptos::prelude::*;

/// Shared by the columns of one board.
#[derive(Clone, Copy)]
pub struct DragCtx {
    /// Task being dragged.
    pub dragging: RwSignal<Option<Id>>,
    /// Column and insertion index under the pointer (the placeholder bar).
    pub drop: RwSignal<Option<(TaskStatus, usize)>>,
}

/// Insertion index for a pointer at `client_y` among cards whose vertical midpoints are
/// `mids` (ascending): the number of midpoints above the pointer.
pub fn insertion_index(mids: &[f64], client_y: f64) -> usize {
    todo!("M2-UI-BOARD: insertion_index")
}
