//! Request-local limits for the serving adapter. No bulk backend call may run
//! under this budget: a rejected batch could already have been materialized.

use std::{
    collections::{BTreeMap, HashSet},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

use tokio::time::Instant;

use crate::{
    Entry, ID, Result, Snapshot,
    backend::BackendImpl,
    constants::SETTINGS,
    crdt::{Codec, Doc},
};

use super::StateError;

pub(crate) const MAX_TIPS: usize = 64;

pub(crate) struct ReadWork {
    remaining_entries: AtomicUsize,
    remaining_bytes: AtomicUsize,
    max_object: usize,
    deadline: Instant,
}

impl ReadWork {
    #[cfg(all(unix, feature = "service"))]
    pub(crate) fn new(
        entries: usize,
        bytes: usize,
        max_object: usize,
        duration: std::time::Duration,
    ) -> Self {
        Self {
            remaining_entries: AtomicUsize::new(entries),
            remaining_bytes: AtomicUsize::new(bytes),
            max_object,
            deadline: Instant::now() + duration,
        }
    }

    pub(crate) async fn checkpoint(&self) -> Result<()> {
        // Point reads on the in-memory backend may otherwise never yield.
        tokio::task::yield_now().await;
        if Instant::now() >= self.deadline {
            return Err(StateError::Limit.into());
        }
        Ok(())
    }

    async fn bounded<T>(&self, read: impl Future<Output = Result<T>>) -> Result<T> {
        self.checkpoint().await?;
        tokio::time::timeout_at(self.deadline, read)
            .await
            .map_err(|_| StateError::Limit)?
    }

    pub(crate) async fn verified(&self, backend: &dyn BackendImpl, tree: &ID) -> Result<Snapshot> {
        self.bounded(backend.verified_snapshot_bounded(tree, MAX_TIPS))
            .await
    }

    pub(crate) async fn entry(&self, backend: &dyn BackendImpl, id: &ID) -> Result<Entry> {
        self.remaining_entries
            .fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1))
            .map_err(|_| StateError::Limit)?;
        let max_bytes = self.remaining_bytes.load(Relaxed).min(self.max_object);
        let (entry, bytes) = self
            .bounded(backend.get_bounded(id, max_bytes))
            .await
            .map_err(|e| {
                if e.is_not_found() {
                    StateError::UnavailableHistory.into()
                } else {
                    e
                }
            })?;
        if bytes > max_bytes {
            return Err(StateError::Limit.into());
        }
        self.remaining_bytes
            .fetch_update(Relaxed, Relaxed, |n| n.checked_sub(bytes))
            .map_err(|_| StateError::Limit)?;
        self.checkpoint().await?;
        Ok(entry)
    }

    pub(crate) async fn require_reachable(
        &self,
        backend: &dyn BackendImpl,
        tree: &ID,
        from: &Snapshot,
        targets: &[ID],
    ) -> Result<()> {
        // Preserve missing history vs. an existing but foreign/non-ancestor
        // boundary. Targets are few and charging their reads is intentional.
        for id in targets.iter().filter(|id| !from.tips().contains(id)) {
            let entry = self.entry(backend, id).await?;
            if entry.id_ref() != id || !entry.in_tree(tree) {
                return Err(StateError::InvalidBoundary.into());
            }
        }
        let mut remaining: HashSet<_> = targets.iter().collect();
        remaining.retain(|id| !from.tips().contains(id));
        let mut walk = Walk::new(from);
        while !remaining.is_empty() {
            let Some(entry) = walk.next(self, backend, tree).await? else {
                return Err(StateError::InvalidBoundary.into());
            };
            remaining.remove(entry.id_ref());
        }
        Ok(())
    }

    // The bounded cold path reads the immutable main ancestry once per fold.
    // Retain only selected deltas (in the existing fold order), not the Entries.
    // The exact current Settings index avoids a database-wide read on capture.
    pub(crate) async fn fold(
        &self,
        backend: &dyn BackendImpl,
        tree: &ID,
        at: &Snapshot,
        store: &str,
        main_order: bool,
    ) -> Result<Doc> {
        let indexed = if store == SETTINGS {
            self.bounded(backend.indexed_store_tips_at(tree, store, at, MAX_TIPS))
                .await?
        } else {
            None
        };
        let mut walk = Walk::new(indexed.as_ref().unwrap_or(at));
        walk.store = indexed.as_ref().map(|_| store);
        let mut needed = HashSet::new();
        let mut contributions = HashSet::new();
        let mut deltas = BTreeMap::new();
        while let Some(entry) = walk.next(self, backend, tree).await? {
            if entry.in_subtree(store) {
                let id = entry.id();
                contributions.insert(id.clone());
                let parents = entry.subtree_parents(store)?;
                if needed.len().saturating_add(parents.len()) > Walk::MAX_DISCOVERED {
                    return Err(StateError::Limit.into());
                }
                needed.extend(parents);
                if let Ok(data) = entry.data(store) {
                    let height = if main_order {
                        entry.height()
                    } else {
                        entry.subtree_height(store)?
                    };
                    deltas.insert((height, id), data.to_vec());
                }
            } else if indexed.is_some() {
                return Err(StateError::InvalidBoundary.into());
            }
        }
        if !needed.is_subset(&contributions) {
            return Err(StateError::UnavailableHistory.into());
        }
        let mut result = Doc::new();
        for data in deltas.into_values() {
            self.checkpoint().await?;
            result.merge_from(&Doc::decode(&data)?);
        }
        Ok(result)
    }
}

/// A pull-driven ancestry walk. Descendants can arrive before their parents;
/// keep only IDs between items, and validate advertised coverage separately.
pub(crate) struct Walk<'a> {
    stack: Vec<ID>,
    discovered: HashSet<ID>,
    store: Option<&'a str>,
}

impl Walk<'_> {
    pub(crate) const MAX_DISCOVERED: usize = 8192;

    pub(crate) fn new(at: &Snapshot) -> Self {
        Self {
            stack: at.tips().to_vec(),
            discovered: at.iter().cloned().collect(),
            store: None,
        }
    }

    pub(crate) async fn next(
        &mut self,
        work: &ReadWork,
        backend: &dyn BackendImpl,
        tree: &ID,
    ) -> Result<Option<Entry>> {
        let Some(id) = self.stack.pop() else {
            return Ok(None);
        };
        let entry = work.entry(backend, &id).await?;
        if entry.id_ref() != &id || !entry.in_tree(tree) {
            return Err(StateError::InvalidBoundary.into());
        }
        let parents = if let Some(store) = self.store {
            entry
                .subtree_parents(store)
                .map_err(|_| StateError::InvalidBoundary)?
        } else {
            entry.parents()?
        };
        for id in parents {
            if self.discovered.insert(id.clone()) {
                if self.discovered.len() > Self::MAX_DISCOVERED {
                    return Err(StateError::Limit.into());
                }
                self.stack.push(id);
            }
        }
        Ok(Some(entry))
    }
}
