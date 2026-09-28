//! Transcript store of one attempt view (spec §9.4). Owner: M2-UI-TASK.
//!
//! The rows are a window of at most [`MAX_ROWS`] entries in ascending `idx`. It grows at the
//! bottom while the user follows the newest entries (dropping the oldest), and at the top on
//! "Carica precedenti" (dropping the newest, which the next `Snapshot` brings back).

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

impl Row {
    fn new(entry: Entry) -> Self {
        Self {
            idx: entry.idx,
            entry: RwSignal::new(entry),
        }
    }
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
    /// The view is within 48 px of the bottom: new rows past [`MAX_ROWS`] drop the oldest.
    /// Otherwise the window stops at its last row and [`Self::newer_hidden`] is set.
    pub at_bottom: RwSignal<bool>,
    /// Newer entries exist past the last row; cleared by the next `Snapshot`.
    pub newer_hidden: RwSignal<bool>,
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
            at_bottom: RwSignal::new(true),
            newer_hidden: RwSignal::new(false),
        }
    }

    /// `Snapshot` replaces the store; `Upsert` applies per `idx` only if `rev` is greater;
    /// `Typing` sets the preview.
    pub fn apply(&self, msg: TranscriptMsg) {
        match msg {
            TranscriptMsg::Snapshot {
                entries,
                has_more,
                typing,
            } => {
                let skip = entries.len().saturating_sub(MAX_ROWS);
                let rows = entries.into_iter().skip(skip).map(Row::new).collect();
                if let Some(old) = self.rows.try_get_untracked() {
                    dispose(old);
                }
                self.set_rows(rows);
                self.has_more.try_set(has_more || skip > 0);
                self.newer_hidden.try_set(false);
                self.typing.try_set(typing);
            }
            TranscriptMsg::Upsert { entries } => self.upsert(entries),
            TranscriptMsg::Typing { text } => {
                self.typing.try_set(text);
            }
        }
    }

    /// Prepends an older page from `get_entries` ("Carica precedenti").
    pub fn prepend(&self, page: EntryPage) {
        let Some(mut rows) = self.rows.try_get_untracked() else {
            return;
        };
        let first = rows.first().map(|r| r.idx);
        let older = page
            .entries
            .into_iter()
            .filter(|e| first.is_none_or(|first| e.idx < first))
            .map(Row::new);
        rows.splice(0..0, older);
        self.has_more.try_set(page.has_more);
        if rows.len() > MAX_ROWS {
            dispose(rows.drain(MAX_ROWS..));
            self.newer_hidden.try_set(true);
        }
        self.set_rows(rows);
    }

    fn upsert(&self, entries: Vec<Entry>) {
        let Some(mut rows) = self.rows.try_get_untracked() else {
            return;
        };
        let has_more = self.has_more.get_untracked();
        let newer_hidden = self.newer_hidden.get_untracked();
        let mut inserted = false;
        for entry in entries {
            // `index` is stale once this batch has inserted a row.
            let indexed = if inserted {
                None
            } else {
                self.index
                    .try_with_value(|index| index.get(&entry.idx).copied())
                    .flatten()
            };
            let pos = indexed.map_or_else(|| rows.binary_search_by_key(&entry.idx, |r| r.idx), Ok);
            match pos {
                Ok(pos) => {
                    let row = rows[pos];
                    if row.entry.try_with_untracked(|cur| entry.rev > cur.rev) == Some(true) {
                        row.entry.try_set(entry);
                    }
                }
                // Outside the window: older than its first row, or past a hidden tail.
                Err(0) if has_more && !rows.is_empty() => {}
                Err(pos) if newer_hidden && pos == rows.len() && !rows.is_empty() => {}
                Err(pos) => {
                    rows.insert(pos, Row::new(entry));
                    inserted = true;
                }
            }
        }
        if !inserted {
            return;
        }
        if rows.len() > MAX_ROWS {
            if self.at_bottom.get_untracked() {
                let excess = rows.len() - MAX_ROWS;
                dispose(rows.drain(..excess));
                self.has_more.try_set(true);
            } else {
                dispose(rows.drain(MAX_ROWS..));
                self.newer_hidden.try_set(true);
            }
        }
        self.set_rows(rows);
    }

    fn set_rows(&self, rows: Vec<Row>) {
        let index = rows.iter().enumerate().map(|(i, r)| (r.idx, i)).collect();
        self.index.try_set_value(index);
        self.rows.try_set(rows);
    }
}

/// Frees the entries of dropped rows; row views read them with `try_*`.
fn dispose(rows: impl IntoIterator<Item = Row>) {
    for row in rows {
        row.entry.dispose();
    }
}

#[cfg(test)]
mod tests {
    use atm_types::EntryBody;

    use super::*;

    fn entry(idx: u32, rev: u32) -> Entry {
        Entry {
            idx,
            rev,
            process_id: "p".into(),
            ts: 0,
            parent_tool_use_id: None,
            body: EntryBody::Stderr {
                text: format!("{idx}.{rev}"),
            },
        }
    }

    fn range(from: u32, to: u32) -> Vec<Entry> {
        (from..to).map(|i| entry(i, 1)).collect()
    }

    /// (first idx, last idx, rows)
    fn window(store: &TranscriptStore) -> (u32, u32, usize) {
        store.rows.with_untracked(|rows| {
            (
                rows.first().map_or(0, |r| r.idx),
                rows.last().map_or(0, |r| r.idx),
                rows.len(),
            )
        })
    }

    fn text(store: &TranscriptStore, idx: u32) -> String {
        let pos = store.index.with_value(|i| i[&idx]);
        store.rows.with_untracked(|rows| {
            rows[pos].entry.with_untracked(|e| match &e.body {
                EntryBody::Stderr { text } => text.clone(),
                _ => unreachable!(),
            })
        })
    }

    fn snapshot(store: &TranscriptStore, entries: Vec<Entry>, has_more: bool) {
        store.apply(TranscriptMsg::Snapshot {
            entries,
            has_more,
            typing: Some("…".into()),
        });
    }

    fn upsert(store: &TranscriptStore, entries: Vec<Entry>) {
        store.apply(TranscriptMsg::Upsert { entries });
    }

    #[test]
    fn snapshot_replaces_and_upsert_needs_a_higher_rev() {
        let store = TranscriptStore::new();
        snapshot(&store, range(0, 10), false);
        upsert(&store, vec![entry(3, 2), entry(4, 1), entry(3, 1)]);
        assert_eq!(text(&store, 3), "3.2");
        assert_eq!(text(&store, 4), "4.1");
        assert_eq!(store.typing.get_untracked().as_deref(), Some("…"));

        snapshot(&store, range(5, 7), true);
        assert_eq!(window(&store), (5, 6, 2));
        assert!(store.has_more.get_untracked());
    }

    #[test]
    fn new_entries_are_inserted_in_order_and_duplicates_ignored() {
        let store = TranscriptStore::new();
        snapshot(&store, range(0, 3), false);
        upsert(
            &store,
            vec![entry(5, 1), entry(4, 1), entry(5, 2), entry(4, 1)],
        );
        let order: Vec<u32> = store
            .rows
            .with_untracked(|rows| rows.iter().map(|r| r.idx).collect());
        assert_eq!(order, [0, 1, 2, 4, 5]);
        assert_eq!(text(&store, 5), "5.2");
        assert_eq!(text(&store, 4), "4.1");
    }

    #[test]
    fn at_bottom_drops_the_oldest_rows() {
        let store = TranscriptStore::new();
        snapshot(&store, range(0, 200), false);
        for batch in 0..49 {
            upsert(&store, range(200 + batch * 200, 400 + batch * 200));
        }
        assert_eq!(window(&store), (9_700, 9_999, MAX_ROWS));
        assert!(store.has_more.get_untracked());
        assert!(!store.newer_hidden.get_untracked());
        // An update of an entry that left the window is ignored.
        upsert(&store, vec![entry(3, 9)]);
        assert_eq!(window(&store), (9_700, 9_999, MAX_ROWS));
    }

    #[test]
    fn scrolled_up_keeps_the_window_and_hides_newer_rows() {
        let store = TranscriptStore::new();
        snapshot(&store, range(0, 250), false);
        store.at_bottom.set(false);
        upsert(&store, range(250, 400));
        assert_eq!(window(&store), (0, 299, MAX_ROWS));
        assert!(store.newer_hidden.get_untracked());
        upsert(&store, vec![entry(400, 1), entry(10, 2)]);
        assert_eq!(window(&store), (0, 299, MAX_ROWS));
        assert_eq!(text(&store, 10), "10.2");
    }

    #[test]
    fn prepend_adds_older_rows_and_trims_the_newest() {
        let store = TranscriptStore::new();
        snapshot(&store, range(9_800, 10_000), true);
        store.prepend(EntryPage {
            entries: range(9_700, 9_801),
            has_more: true,
        });
        assert_eq!(window(&store), (9_700, 9_999, MAX_ROWS));
        assert!(!store.newer_hidden.get_untracked());
        store.prepend(EntryPage {
            entries: range(9_600, 9_700),
            has_more: false,
        });
        assert_eq!(window(&store), (9_600, 9_899, MAX_ROWS));
        assert!(store.newer_hidden.get_untracked());
        assert!(!store.has_more.get_untracked());
        assert_eq!(text(&store, 9_600), "9600.1");
        // The next snapshot brings the tail back.
        snapshot(&store, range(9_800, 10_000), true);
        assert!(!store.newer_hidden.get_untracked());
    }
}
