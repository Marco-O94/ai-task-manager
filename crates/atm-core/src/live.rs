//! Live transcript fan-out (spec §6.5): one lossy `broadcast` per attempt, one forwarder
//! task per subscription writing into a [`TranscriptSink`]. Owner: M3-CORE.
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code)]

use std::sync::Arc;
use std::time::Duration;

use atm_types::{AppError, Entry, Id, TranscriptMsg};

use crate::db::Db;

/// Delivers one message to a UI view; returns `false` once the view is gone (the forwarder
/// then stops and deregisters). The Tauri shell wraps `Channel::send` in it.
pub type TranscriptSink = Box<dyn Fn(TranscriptMsg) -> bool + Send + Sync>;

/// Per-attempt broadcast capacity; slow subscribers lag instead of blocking stdout.
pub const BROADCAST_CAPACITY: usize = 1024;
/// Forwarder batching of upserts: whichever comes first.
pub const BATCH_WINDOW: Duration = Duration::from_millis(50);
pub const BATCH_MAX: usize = 200;
/// Entries in a `Snapshot` (the UI pages further back with `get_entries`).
pub const SNAPSHOT_TAIL: u32 = 200;

/// Payload of the per-attempt broadcast.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveMsg {
    /// Already persisted (`Db::upsert_entries` runs first); shared, since every subscriber
    /// receives its own clone of the message.
    Upsert(Arc<Entry>),
    Typing(Option<String>),
}

/// Registry of broadcasts, latest typing previews and forwarders.
#[derive(Default)]
pub struct Live {}

impl Live {
    pub fn new() -> Live {
        Live::default()
    }

    /// Broadcasts to the attempt's subscribers without blocking; also records the latest
    /// typing preview. No subscriber = dropped.
    pub fn send(&self, attempt_id: &str, msg: LiveMsg) {
        todo!("M3-CORE: Live::send")
    }

    /// Latest typing preview of the attempt.
    pub fn typing(&self, attempt_id: &str) -> Option<String> {
        todo!("M3-CORE: Live::typing")
    }

    /// Spec §6.5 steps 1–4: subscribe to the broadcast first, read the tail from `db`, spawn
    /// the forwarder (`Snapshot`, then upserts batched by [`BATCH_WINDOW`]/[`BATCH_MAX`],
    /// only the last `Typing`, a fresh `Snapshot` after `Lagged`), return the subscription
    /// id at once. Must run inside a tokio runtime.
    pub fn subscribe(
        self: &Arc<Self>,
        db: Arc<Db>,
        attempt_id: &str,
        sink: TranscriptSink,
    ) -> Result<Id, AppError> {
        Err(AppError::not_implemented("Live::subscribe"))
    }

    /// Stops one forwarder; `false` if unknown (already gone).
    pub fn unsubscribe(&self, subscription_id: &str) -> bool {
        todo!("M3-CORE: Live::unsubscribe")
    }

    /// Stops every forwarder (page reload, spec §6.5 "Pulizia").
    pub fn drop_all(&self) {
        todo!("M3-CORE: Live::drop_all")
    }

    /// Live forwarders (0 after a reload; checked in M4).
    pub fn forwarder_count(&self) -> usize {
        todo!("M3-CORE: Live::forwarder_count")
    }
}
