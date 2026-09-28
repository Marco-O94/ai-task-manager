//! Live transcript fan-out (spec §6.5): one lossy `broadcast` per attempt, one forwarder
//! task per subscription writing into a [`TranscriptSink`]. Owner: M3-CORE.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use atm_types::{AppError, Entry, EntryPage, Id, TranscriptMsg};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until};

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
pub struct Live {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Only while the attempt has subscribers or a typing preview.
    attempts: HashMap<Id, Channel>,
    forwarders: HashMap<Id, AbortHandle>,
}

struct Channel {
    tx: broadcast::Sender<LiveMsg>,
    typing: Option<String>,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            typing: None,
        }
    }
}

impl Live {
    pub fn new() -> Live {
        Live::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Broadcasts to the attempt's subscribers without blocking; also records the latest
    /// typing preview. No subscriber = dropped.
    pub fn send(&self, attempt_id: &str, msg: LiveMsg) {
        let mut state = self.lock();
        if !state.attempts.contains_key(attempt_id) {
            // Nobody listens: only a typing preview is kept, for the next Snapshot.
            if !matches!(msg, LiveMsg::Typing(Some(_))) {
                return;
            }
            state.attempts.insert(attempt_id.to_owned(), Channel::new());
        }
        let Some(channel) = state.attempts.get_mut(attempt_id) else {
            return;
        };
        if let LiveMsg::Typing(text) = &msg {
            channel.typing.clone_from(text);
        }
        let _ = channel.tx.send(msg);
        if channel.tx.receiver_count() == 0 && channel.typing.is_none() {
            state.attempts.remove(attempt_id);
        }
    }

    /// Latest typing preview of the attempt.
    pub fn typing(&self, attempt_id: &str) -> Option<String> {
        self.lock()
            .attempts
            .get(attempt_id)
            .and_then(|c| c.typing.clone())
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
        let rx = self
            .lock()
            .attempts
            .entry(attempt_id.to_owned())
            .or_insert_with(Channel::new)
            .tx
            .subscribe();
        let page = db.entries_tail(attempt_id, SNAPSHOT_TAIL)?;
        let typing = self.typing(attempt_id);
        let id = crate::new_id();
        let forwarder = Forwarder {
            live: Arc::clone(self),
            db,
            attempt_id: attempt_id.to_owned(),
            id: id.clone(),
            sink,
        };
        // Registered under the lock: a forwarder that ends at once deregisters after this.
        let mut state = self.lock();
        let task = tokio::spawn(forwarder.run(rx, page, typing));
        state.forwarders.insert(id.clone(), task.abort_handle());
        Ok(id)
    }

    /// Stops one forwarder; `false` if unknown (already gone).
    pub fn unsubscribe(&self, subscription_id: &str) -> bool {
        let handle = self.lock().forwarders.remove(subscription_id);
        handle.map(|h| h.abort()).is_some()
    }

    /// Stops every forwarder (page reload, spec §6.5 "Pulizia").
    pub fn drop_all(&self) {
        for (_, handle) in self.lock().forwarders.drain() {
            handle.abort();
        }
    }

    /// Live forwarders (0 after a reload; checked in M4).
    pub fn forwarder_count(&self) -> usize {
        self.lock().forwarders.len()
    }
}

struct Forwarder {
    live: Arc<Live>,
    db: Arc<Db>,
    attempt_id: Id,
    id: Id,
    sink: TranscriptSink,
}

impl Forwarder {
    async fn run(
        self,
        mut rx: broadcast::Receiver<LiveMsg>,
        page: EntryPage,
        typing: Option<String>,
    ) {
        if self.snapshot(page, typing) {
            self.forward(&mut rx).await;
        }
        self.live.lock().forwarders.remove(&self.id);
    }

    fn snapshot(&self, page: EntryPage, typing: Option<String>) -> bool {
        (self.sink)(TranscriptMsg::Snapshot {
            entries: page.entries,
            has_more: page.has_more,
            typing,
        })
    }

    /// Returns when the view is gone, the channel closes or the DB fails a re-snapshot.
    async fn forward(&self, rx: &mut broadcast::Receiver<LiveMsg>) {
        let mut batch: Vec<Entry> = Vec::new();
        // `Some(text)`: a typing update to send with the next flush.
        let mut typing: Option<Option<String>> = None;
        let mut flush_at: Option<Instant> = None;
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(LiveMsg::Upsert(entry)) => {
                        push_latest(&mut batch, &entry);
                        if batch.len() >= BATCH_MAX {
                            if !self.flush(&mut batch, &mut typing) {
                                return;
                            }
                            flush_at = None;
                        } else {
                            flush_at.get_or_insert_with(|| Instant::now() + BATCH_WINDOW);
                        }
                    }
                    Ok(LiveMsg::Typing(text)) => {
                        typing = Some(text);
                        flush_at.get_or_insert_with(|| Instant::now() + BATCH_WINDOW);
                    }
                    // Messages were lost: the DB has every persisted entry, start over from it.
                    Err(RecvError::Lagged(_)) => {
                        (batch, typing, flush_at) = (Vec::new(), None, None);
                        let Ok(page) = self.db.entries_tail(&self.attempt_id, SNAPSHOT_TAIL)
                        else {
                            return;
                        };
                        if !self.snapshot(page, self.live.typing(&self.attempt_id)) {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => {
                        self.flush(&mut batch, &mut typing);
                        return;
                    }
                },
                () = sleep_until(flush_at.unwrap_or_else(Instant::now)), if flush_at.is_some() => {
                    if !self.flush(&mut batch, &mut typing) {
                        return;
                    }
                    flush_at = None;
                }
            }
        }
    }

    fn flush(&self, batch: &mut Vec<Entry>, typing: &mut Option<Option<String>>) -> bool {
        if !batch.is_empty()
            && !(self.sink)(TranscriptMsg::Upsert {
                entries: std::mem::take(batch),
            })
        {
            return false;
        }
        match typing.take() {
            Some(text) => (self.sink)(TranscriptMsg::Typing { text }),
            None => true,
        }
    }
}

/// Adds `entry` to the batch, keeping one copy per `idx` (the highest `rev`).
fn push_latest(batch: &mut Vec<Entry>, entry: &Entry) {
    match batch.iter_mut().find(|e| e.idx == entry.idx) {
        Some(e) if e.rev < entry.rev => *e = entry.clone(),
        Some(_) => {}
        None => batch.push(entry.clone()),
    }
}
