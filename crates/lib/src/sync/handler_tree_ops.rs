//! Tree operations for SyncHandlerImpl.
//!
//! This module contains methods for collecting and counting entries in trees,
//! used during sync operations like bootstrap and incremental sync.

use tracing::{error, warn};

use super::{handler::SyncHandlerImpl, protocol::TreeInfo};
use crate::{
    Result,
    entry::{Entry, ID},
};

impl SyncHandlerImpl {
    /// Bounded best-effort discovery. Counts use the native page producer,
    /// never an eager body walk independent of the peer resource/deadline caps.
    pub(super) async fn get_available_trees(&self) -> Result<Vec<TreeInfo>> {
        use crate::backend::history_page::{MAX_HISTORY_TIPS, resource};
        let instance = self.instance()?;
        let backend = instance
            .backend()
            .local_engine()
            .expect("sync requires local backend");
        let roots = backend.all_roots().await?;
        if roots.len() > MAX_HISTORY_TIPS {
            return Err(resource("peer discovery roots", MAX_HISTORY_TIPS));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut infos = Vec::new();
        for tree_id in roots {
            if tokio::time::Instant::now() >= deadline {
                return Err(resource("peer discovery deadline", 0));
            }
            // Preserve best-effort discovery: an unavailable/untransferable
            // history is not advertised as a complete count.
            if let Ok(entry_count) = self.count_tree_entries(&tree_id, deadline).await {
                infos.push(TreeInfo {
                    tree_id,
                    name: None,
                    entry_count,
                    last_modified: 0,
                });
            }
        }
        Ok(infos)
    }

    /// Collect all entries in a tree (excluding the root)
    #[allow(dead_code)]
    pub(super) async fn collect_all_tree_entries(&self, tree_id: &ID) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let snapshot = self.instance()?.backend().snapshot(tree_id).await?;
        to_visit.extend(snapshot.tips().iter().cloned());

        // Traverse the DAG depth-first
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) || entry_id == *tree_id {
                continue; // Skip root and already visited
            }
            visited.insert(entry_id.clone());

            match self.instance()?.backend().get(&entry_id).await {
                Ok(entry) => {
                    // Add parents to visit list
                    if let Ok(parent_ids) = entry.parents() {
                        for parent_id in parent_ids {
                            if !visited.contains(&parent_id) && parent_id != *tree_id {
                                to_visit.push_back(parent_id);
                            }
                        }
                    }
                    entries.push(entry);
                }
                Err(e) if e.is_not_found() => {
                    warn!(entry_id = %entry_id, "Entry not found during traversal");
                }
                Err(e) => {
                    error!(entry_id = %entry_id, error = %e, "Error during traversal");
                    return Err(e);
                }
            }
        }

        Ok(entries)
    }

    /// Collect ALL entries in a tree for bootstrap (including root)
    pub(super) async fn collect_all_entries_for_bootstrap(
        &self,
        tree_id: &ID,
    ) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let snapshot = self.instance()?.backend().snapshot(tree_id).await?;
        to_visit.extend(snapshot.tips().iter().cloned());

        // Traverse the DAG depth-first, INCLUDING the root
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) {
                continue; // Skip already visited (but don't skip root)
            }
            visited.insert(entry_id.clone());

            match self.instance()?.backend().get(&entry_id).await {
                Ok(entry) => {
                    // Add parents to visit list
                    if let Ok(parent_ids) = entry.parents() {
                        for parent_id in parent_ids {
                            if !visited.contains(&parent_id) {
                                to_visit.push_back(parent_id);
                            }
                        }
                    }
                    entries.push(entry);
                }
                Err(e) if e.is_not_found() => {
                    warn!(entry_id = %entry_id, "Entry not found during traversal");
                }
                Err(e) => {
                    error!(entry_id = %entry_id, error = %e, "Error during traversal");
                    return Err(e);
                }
            }
        }

        // IMPORTANT: Reverse the entries so parents come before children
        // The traversal collects children first (starting from tips), but we need
        // to store parents first for proper tip tracking
        entries.reverse();

        Ok(entries)
    }

    /// Find entries that peer is missing
    pub(super) async fn find_missing_entries_for_peer(
        &self,
        our_tips: &[ID],
        peer_tips: &[ID],
    ) -> Result<Vec<Entry>> {
        // Find tips they don't have
        let missing_tip_ids: Vec<_> = our_tips
            .iter()
            .filter(|tip_id| !peer_tips.contains(tip_id))
            .cloned()
            .collect();

        if missing_tip_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Collect ancestors
        let engine = self
            .instance()?
            .backend()
            .local_engine()
            .expect("sync requires local backend");
        super::utils::collect_ancestors_to_send(engine.as_ref(), &missing_tip_ids, peer_tips).await
    }

    /// Count through the same bounded native traversal used for serving.
    pub(super) async fn count_tree_entries(
        &self,
        tree_id: &ID,
        deadline: tokio::time::Instant,
    ) -> Result<usize> {
        let instance = self.instance()?;
        let backend = instance
            .backend()
            .local_engine()
            .expect("sync requires local backend");
        let target = backend.snapshot(tree_id).await?;
        let limits = crate::backend::HistoryPageLimits {
            max_entries: 1024,
            max_encoded_bytes: crate::backend::MAX_DATABASE_PAGE_ENCODING,
            allow_large_singleton: false,
        };
        let mut cursor = None;
        let mut count = 0;
        loop {
            let page = backend
                .database_history_page(
                    tree_id,
                    &target,
                    &crate::snapshot::Snapshot::EMPTY,
                    cursor.as_ref(),
                    limits,
                    deadline,
                )
                .await?;
            count += page.entries.len();
            cursor = page.next;
            if cursor.is_none() {
                return Ok(count);
            }
        }
    }
}
