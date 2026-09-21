//! In-process recency for the derived Store-state materialization cache.
//!
//! Both database backends keep exact access order in memory while open:
//! every derived-namespace hit assigns a fresh monotonically increasing
//! tick. Reads touch memory only — they never issue a storage write.
//!
//! The SQL backend additionally persists ticks durably in batches (see the
//! `last_access_tick` column): touches accumulate in `dirty` and are
//! flushed as one statement once [`RECENCY_FLUSH_THRESHOLD`] touches are
//! dirty, inside an already-open write transaction (publish, eviction), or
//! not at all on a read-only workload. A crash therefore loses only recent
//! recency metadata, never cached values. Eviction flushes first, so it
//! always decides on the best persisted recency plus the live order —
//! steady state is approximate durable LRU, never insertion-order eviction.
//!
//! The InMemory backend never persists derived namespaces at all, so its
//! live map is the whole recency story; the same struct keeps the two
//! backends' eviction order semantics aligned.

use std::collections::HashMap;

/// Touches accumulated before one batched durable flush.
///
/// Reads stay write-free: only every 64th hit (amortized) performs a single
/// batched persistence write, and publish/eviction transactions piggyback
/// whatever is dirty. This is the "bounded cadence" half of the no-write-
/// per-hit contract.
pub(crate) const RECENCY_FLUSH_THRESHOLD: usize = 64;

/// Exact in-process access order plus the not-yet-flushed durable tail.
#[derive(Debug, Default)]
pub(crate) struct RecencyState {
    /// Last tick assigned; the next touch uses `tick + 1`.
    tick: u64,
    /// Live access order: namespace id -> tick. Exact while open.
    order: HashMap<String, u64>,
    /// Touches not yet persisted (SQL only): namespace id -> tick.
    dirty: HashMap<String, u64>,
}

impl RecencyState {
    /// Record a hit. Returns the assigned tick and whether the dirty set
    /// reached the flush threshold.
    pub(crate) fn touch(&mut self, namespace_id: &str) -> (u64, bool) {
        self.tick = self.tick.saturating_add(1);
        self.order.insert(namespace_id.to_string(), self.tick);
        self.dirty.insert(namespace_id.to_string(), self.tick);
        (self.tick, self.dirty.len() >= RECENCY_FLUSH_THRESHOLD)
    }

    /// Record a hit with a predetermined tick (e.g. assigned before opening
    /// a publish transaction so the tick can be written atomically with the
    /// publish itself).
    pub(crate) fn touch_with_tick(&mut self, namespace_id: &str, tick: u64) {
        self.tick = self.tick.max(tick);
        self.order.insert(namespace_id.to_string(), tick);
        self.dirty.remove(namespace_id);
    }

    /// Advance the clock without attributing the tick (used to seed from
    /// persisted state on first use).
    pub(crate) fn advance_to(&mut self, tick: u64) {
        self.tick = self.tick.max(tick);
    }

    /// Current clock value.
    pub(crate) fn now(&self) -> u64 {
        self.tick
    }

    /// Live tick for eviction ordering, if the namespace was touched while open.
    pub(crate) fn live_tick(&self, namespace_id: &str) -> Option<u64> {
        self.order.get(namespace_id).copied()
    }

    /// Drain the dirty set for one batched durable flush.
    pub(crate) fn drain_dirty(&mut self) -> Vec<(String, u64)> {
        self.dirty.drain().collect()
    }

    /// Drop bookkeeping for namespaces that no longer exist.
    pub(crate) fn retain_existing(&mut self, live: &std::collections::HashSet<String>) {
        self.order.retain(|id, _| live.contains(id));
        self.dirty.retain(|id, _| live.contains(id));
    }
}
