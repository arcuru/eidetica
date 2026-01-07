//! Sync queue for batching entries before sending to peers.
//!
//! The `SyncQueue` provides a thread-safe queue for entry IDs that need to be
//! synchronized to peers. Entries are stored by peer ID and can be drained
//! in batches for efficient sending.
//!
//! This is an in-memory queue - entries are lost on restart. The DAG comparison
//! mechanism handles re-synchronization after restart.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::entry::ID;

/// Thread-safe queue for entries pending synchronization.
///
/// Entries are grouped by peer ID and can be drained in batches.
/// The background sync thread wakes on a timer to process the queue.
#[derive(Debug)]
pub struct SyncQueue {
    /// Per-peer queues of (entry_id, tree_id) pairs
    queues: Mutex<HashMap<String, Vec<(ID, ID)>>>,
}

impl Default for SyncQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncQueue {
    /// Create a new empty sync queue.
    pub fn new() -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
        }
    }

    /// Add an entry to the queue for a specific peer.
    pub fn enqueue(&self, peer: &str, entry_id: ID, tree_id: ID) {
        let mut queues = self.queues.lock().unwrap();
        queues
            .entry(peer.to_string())
            .or_default()
            .push((entry_id, tree_id));
    }

    /// Take all queued entries, grouped by peer.
    ///
    /// Returns a map of peer ID to list of (entry_id, tree_id) pairs.
    /// The queue is emptied after this call.
    pub fn drain(&self) -> HashMap<String, Vec<(ID, ID)>> {
        let mut queues = self.queues.lock().unwrap();
        std::mem::take(&mut *queues)
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queues.lock().unwrap().is_empty()
    }

    /// Get the total number of entries across all peers.
    pub fn len(&self) -> usize {
        self.queues
            .lock()
            .unwrap()
            .values()
            .map(|v: &Vec<(ID, ID)>| v.len())
            .sum()
    }

    /// Get the number of peers with pending entries.
    pub fn peer_count(&self) -> usize {
        self.queues.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_and_drain() {
        let queue = SyncQueue::new();

        let entry1 = ID::new("entry1");
        let entry2 = ID::new("entry2");
        let tree = ID::new("tree1");

        queue.enqueue("peer1", entry1.clone(), tree.clone());
        queue.enqueue("peer1", entry2.clone(), tree.clone());
        queue.enqueue("peer2", entry1.clone(), tree.clone());

        assert_eq!(queue.len(), 3);
        assert_eq!(queue.peer_count(), 2);
        assert!(!queue.is_empty());

        let batches = queue.drain();

        assert_eq!(batches.len(), 2);
        assert_eq!(batches.get("peer1").unwrap().len(), 2);
        assert_eq!(batches.get("peer2").unwrap().len(), 1);

        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_empty_queue() {
        let queue = SyncQueue::new();

        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.peer_count(), 0);

        let batches = queue.drain();
        assert!(batches.is_empty());
    }
}
