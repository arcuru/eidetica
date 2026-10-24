//! In-process recency for the derived Store-state materialization cache.
//!
//! Both database backends keep exact access order in memory while open:
//! every derived-namespace hit assigns a fresh monotonically increasing
//! tick. Hits accumulate in memory rather than writing to storage each time.
//!
//! The SQL backend persists ticks in batches in `last_access_tick`: a hit
//! flushes once [`RECENCY_FLUSH_THRESHOLD`] distinct namespaces are dirty,
//! and eviction flushes any remaining tail in its write transaction.
//! Repeated reads of fewer namespaces stay write-free until eviction.
//! A crash loses recent recency metadata, never cached values. Eviction
//! combines persisted ticks with the live order, including hits whose
//! flushes are still in flight.
//!
//! The InMemory backend never persists derived namespaces at all, so its
//! live map is the whole recency story; the same struct keeps the two
//! backends' eviction order semantics aligned.

use std::collections::HashMap;

/// Distinct dirty namespaces accumulated before one batched durable flush.
/// Repeated hits on one namespace update its pending tick without growing
/// the batch; eviction also flushes batches below this threshold.
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
        self.tick = self.tick.saturating_add(1).min(i64::MAX as u64);
        self.order.insert(namespace_id.to_string(), self.tick);
        self.dirty.insert(namespace_id.to_string(), self.tick);
        (self.tick, self.dirty.len() >= RECENCY_FLUSH_THRESHOLD)
    }

    /// Record an already-accounted-for tick (e.g. committed by publication).
    /// A hit can arrive after commit but before this bookkeeping, so merge
    /// the live order and clear only dirty hits covered by the persisted tick.
    pub(crate) fn touch_with_tick(&mut self, namespace_id: &str, tick: u64) {
        self.tick = self.tick.max(tick);
        self.order
            .entry(namespace_id.to_string())
            .and_modify(|live| *live = (*live).max(tick))
            .or_insert(tick);
        if self
            .dirty
            .get(namespace_id)
            .is_some_and(|dirty| *dirty <= tick)
        {
            self.dirty.remove(namespace_id);
        }
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

    /// Copy pending ticks without losing them on SQL failure or cancellation.
    pub(crate) fn pending(&self) -> Vec<(String, u64)> {
        self.dirty
            .iter()
            .map(|(id, tick)| (id.clone(), *tick))
            .collect()
    }

    /// Acknowledge only ticks covered by a committed batch. Newer hits survive.
    pub(crate) fn acknowledge(&mut self, batch: &[(String, u64)]) {
        for (id, tick) in batch {
            if self.dirty.get(id).is_some_and(|dirty| dirty <= tick) {
                self.dirty.remove(id);
            }
        }
    }

    pub(crate) fn remove(&mut self, id: &str) {
        self.order.remove(id);
        self.dirty.remove(id);
    }

    /// Drop bookkeeping for namespaces that no longer exist.
    pub(crate) fn retain_existing(&mut self, live: &std::collections::HashSet<String>) {
        self.order.retain(|id, _| live.contains(id));
        self.dirty.retain(|id, _| live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::RecencyState;

    #[test]
    fn delayed_publication_preserves_live_order() {
        let mut recency = RecencyState::default();
        recency.advance_to(1); // Publication reserved tick 1 before its commit.
        let older_hit = recency.touch("older").0;
        let newer_hit = recency.touch("published").0;
        recency.touch_with_tick("published", 1);

        assert_eq!(recency.live_tick("published"), Some(newer_hit));
        assert!(recency.live_tick("published").unwrap() > older_hit);
        assert_eq!(recency.now(), newer_hit);
    }

    #[test]
    fn delayed_publication_preserves_newer_dirty_hit() {
        let mut recency = RecencyState::default();
        recency.advance_to(1);
        let hit = recency.touch("published").0;
        recency.touch_with_tick("published", 1);

        assert_eq!(recency.pending(), vec![("published".to_string(), hit)]);
    }

    #[test]
    fn fresh_publication_clears_only_covered_dirty_hits() {
        let mut recency = RecencyState::default();
        let other_hit = recency.touch("other").0;
        let old_hit = recency.touch("published").0;
        // InMemory assigns and records its publication tick under one lock.
        let published = recency.now() + 1;
        recency.touch_with_tick("published", published);
        assert_eq!(recency.live_tick("published"), Some(published));
        assert!(published > old_hit);
        assert_eq!(recency.pending(), vec![("other".to_string(), other_hit)]);

        recency.acknowledge(&[("other".to_string(), other_hit)]);
        let hit = recency.touch("published").0;
        recency.touch_with_tick("published", hit); // Equal is already covered too.
        assert!(recency.pending().is_empty());
        let next_hit = recency.touch("published").0;
        assert!(next_hit > hit);
        assert_eq!(recency.live_tick("published"), Some(next_hit));
        assert_eq!(recency.pending(), vec![("published".to_string(), next_hit)]);
    }
    #[test]
    fn clock_never_wraps_signed_sql_ticks() {
        let mut recency = RecencyState::default();
        recency.advance_to(i64::MAX as u64 - 1);
        assert_eq!(recency.touch("last").0, i64::MAX as u64);
        assert_eq!(recency.touch("saturated").0, i64::MAX as u64);
    }
}
