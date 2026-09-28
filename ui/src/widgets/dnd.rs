//! HTML5 drag-and-drop of the kanban (spec §9.3), hand-written and signal-driven: the DOM is
//! only read (card rectangles), never moved. Owner: M2-UI-BOARD.

use std::time::Duration;

use atm_types::{Id, TaskStatus};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{DragEvent, Element, Node};

/// Shared by the columns of one board.
#[derive(Clone, Copy)]
pub struct DragCtx {
    /// Task being dragged.
    pub dragging: RwSignal<Option<Id>>,
    /// Column and insertion index under the pointer (the placeholder bar).
    pub drop: RwSignal<Option<(TaskStatus, usize)>>,
}

impl Default for DragCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl DragCtx {
    pub fn new() -> Self {
        Self {
            dragging: RwSignal::new(None),
            drop: RwSignal::new(None),
        }
    }

    /// `dragstart` on a card.
    pub fn start(self, ev: &DragEvent, id: Id) {
        if let Some(dt) = ev.data_transfer() {
            let _ = dt.set_data("text/plain", &id);
            dt.set_effect_allowed("move");
        }
        // Deferred: the browser snapshots the drag image after this handler, so the card
        // dims only once the image has been taken.
        set_timeout(
            move || {
                self.dragging.try_set(Some(id));
            },
            Duration::ZERO,
        );
    }

    /// `dragover` on a column: allows the drop and moves the placeholder to the insertion
    /// index among the `[data-task-id]` cards inside `list`.
    pub fn over(self, ev: &DragEvent, status: TaskStatus, list: &Element) {
        ev.prevent_default();
        let index = insertion_index(&card_midpoints(list), f64::from(ev.client_y()));
        self.set_drop(Some((status, index)));
    }

    /// `dragover` on a column without a card list (collapsed): the drop goes at the end.
    pub fn over_end(self, ev: &DragEvent, status: TaskStatus) {
        ev.prevent_default();
        self.set_drop(Some((status, usize::MAX)));
    }

    /// `dragleave`: hides the placeholder once the pointer leaves `target` for good (moving
    /// onto one of its children also fires `dragleave`).
    pub fn leave(self, ev: &DragEvent, target: &Element) {
        let inside = ev
            .related_target()
            .and_then(|t| t.dyn_into::<Node>().ok())
            .is_some_and(|n| target.contains(Some(&n)));
        if !inside {
            self.set_drop(None);
        }
    }

    /// `drop`: the dragged task, its column and insertion index; ends the drag.
    pub fn finish(self, ev: &DragEvent) -> Option<(Id, TaskStatus, usize)> {
        ev.prevent_default();
        let id = self
            .dragging
            .get_untracked()
            .or_else(|| ev.data_transfer()?.get_data("text/plain").ok())
            .filter(|id| !id.is_empty());
        let target = self.drop.get_untracked();
        self.end();
        let (status, index) = target?;
        Some((id?, status, index))
    }

    /// `dragend`, also fired when the drag is cancelled.
    pub fn end(self) {
        self.dragging.try_set(None);
        self.drop.try_set(None);
    }

    /// Writes only on change: `dragover` fires every few milliseconds.
    fn set_drop(self, drop: Option<(TaskStatus, usize)>) {
        if self.drop.get_untracked() != drop {
            self.drop.set(drop);
        }
    }
}

/// Insertion index for a pointer at `client_y` among cards whose vertical midpoints are
/// `mids` (ascending): the number of midpoints above the pointer.
pub fn insertion_index(mids: &[f64], client_y: f64) -> usize {
    mids.partition_point(|&mid| mid < client_y)
}

/// Vertical midpoints of the `[data-task-id]` elements inside `list`, in DOM order.
fn card_midpoints(list: &Element) -> Vec<f64> {
    let Ok(cards) = list.query_selector_all("[data-task-id]") else {
        return Vec::new();
    };
    (0..cards.length())
        .filter_map(|i| cards.item(i)?.dyn_into::<Element>().ok())
        .map(|card| {
            let rect = card.get_bounding_client_rect();
            rect.top() + rect.height() / 2.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::insertion_index;

    #[test]
    fn insertion_index_counts_the_midpoints_above_the_pointer() {
        let mids = [10.0, 50.0, 90.0];
        assert_eq!(insertion_index(&mids, 0.0), 0);
        assert_eq!(insertion_index(&mids, 10.0), 0);
        assert_eq!(insertion_index(&mids, 11.0), 1);
        assert_eq!(insertion_index(&mids, 70.0), 2);
        assert_eq!(insertion_index(&mids, 500.0), 3);
        assert_eq!(insertion_index(&[], 42.0), 0);
    }
}
