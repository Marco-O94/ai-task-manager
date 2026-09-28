//! Transcript store of one attempt view (spec §9.4). Owner: M2-UI-TASK.
#![allow(dead_code, unused_variables)] // M1 stub: remove when implemented

use std::collections::HashMap;

use atm_types::{Entry, EntryPage, TranscriptMsg};
use leptos::prelude::*;

/// Rows kept in the DOM; older ones are dropped while the user is at the bottom.
pub const MAX_ROWS: usize = 300;

/// One rendered entry: an upsert with a higher `rev` sets `entry`, re-rendering only this row.
#[derive(Clone, Copy)]
pub struct Row {
    pub idx: u32,
    pub entry: RwSignal<Entry>,
}

#[derive(Clone, Copy)]
pub struct TranscriptStore {
    /// Ascending `idx`; rendered with `<For key=|r| r.idx>`.
    pub rows: RwSignal<Vec<Row>>,
    /// `idx` → position in `rows`.
    pub index: StoredValue<HashMap<u32, usize>>,
    /// Older entries exist ("Carica precedenti").
    pub has_more: RwSignal<bool>,
    pub typing: RwSignal<Option<String>>,
}

impl Default for TranscriptStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptStore {
    pub fn new() -> Self {
        Self {
            rows: RwSignal::new(Vec::new()),
            index: StoredValue::new(HashMap::new()),
            has_more: RwSignal::new(false),
            typing: RwSignal::new(None),
        }
    }

    /// `Snapshot` replaces the store; `Upsert` applies per `idx` only if `rev` is greater;
    /// `Typing` sets the preview.
    pub fn apply(&self, msg: TranscriptMsg) {
        todo!("M2-UI-TASK: TranscriptStore::apply")
    }

    /// Prepends an older page from `get_entries` ("Carica precedenti").
    pub fn prepend(&self, page: EntryPage) {
        todo!("M2-UI-TASK: TranscriptStore::prepend")
    }
}
