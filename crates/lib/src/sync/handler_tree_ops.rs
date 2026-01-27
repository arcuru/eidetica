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
    /// Get list of available trees for discovery
    pub(super) async fn get_available_trees(&self) -> Vec<TreeInfo> {
        // Get all root entries in the backend
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => {
                error!(error = %e, "Failed to get instance");
                return Vec::new();
            }
        };
        match instance.backend().all_roots().await {
            Ok(roots) => {
                let mut tree_infos = Vec::new();
                for root_id in roots {
                    // Get basic tree info
                    if let Ok(entry_count) = self.count_tree_entries(&root_id).await {
                        tree_infos.push(TreeInfo {
                            tree_id: root_id,
                            name: None, // Could extract from tree metadata in the future
                            entry_count,
                            last_modified: 0, // Could track modification times in the future
                        });
                    }
                }
                tree_infos
            }
            Err(e) => {
                error!(error = %e, "Failed to get available trees");
                Vec::new()
            }
        }
    }

    /// Collect all entries in a tree (excluding the root)
    #[allow(dead_code)]
    pub(super) async fn collect_all_tree_entries(&self, tree_id: &ID) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.instance()?.backend().get_tips(tree_id).await?;
        to_visit.extend(tips);

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
        let tips = self.instance()?.backend().get_tips(tree_id).await?;
        to_visit.extend(tips);

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
        super::utils::collect_ancestors_to_send(
            self.instance()?.backend().as_backend_impl(),
            &missing_tip_ids,
            peer_tips,
        )
        .await
    }

    /// Count entries in a tree
    pub(super) async fn count_tree_entries(&self, tree_id: &ID) -> Result<usize> {
        let mut count = 1; // Include root
        let mut visited = std::collections::HashSet::new();
        let mut to_visit = std::collections::VecDeque::new();

        // Get tips to start traversal
        let tips = self.instance()?.backend().get_tips(tree_id).await?;
        to_visit.extend(tips);

        // Count all entries
        while let Some(entry_id) = to_visit.pop_front() {
            if visited.contains(&entry_id) || entry_id == *tree_id {
                continue;
            }
            visited.insert(entry_id.clone());
            count += 1;

            if let Ok(entry) = self.instance()?.backend().get(&entry_id).await
                && let Ok(parent_ids) = entry.parents()
            {
                for parent_id in parent_ids {
                    if !visited.contains(&parent_id) && parent_id != *tree_id {
                        to_visit.push_back(parent_id);
                    }
                }
            }
        }

        Ok(count)
    }
}
