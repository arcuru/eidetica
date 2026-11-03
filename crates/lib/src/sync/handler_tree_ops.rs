//! Verified-only tree enumeration for sync discovery and bootstrap.

use tracing::error;

use super::{handler::SyncHandlerImpl, protocol::TreeInfo};
use crate::{
    Result,
    entry::{Entry, ID},
};

impl SyncHandlerImpl {
    /// Get list of available trees for discovery.
    pub(super) async fn get_available_trees(&self) -> Vec<TreeInfo> {
        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => {
                error!(error = %e, "Failed to get instance");
                return Vec::new();
            }
        };
        match instance
            .require_local_engine()
            .expect("sync requires local backend")
            .all_roots()
            .await
        {
            Ok(roots) => {
                let mut trees = Vec::new();
                for tree in roots {
                    if let Ok(entries) = self.collect_all_entries_for_bootstrap(&tree).await
                        && !entries.is_empty()
                    {
                        trees.push(TreeInfo {
                            tree_id: tree,
                            name: None,
                            entry_count: entries.len(),
                            last_modified: 0,
                        });
                    }
                }
                trees
            }
            Err(e) => {
                error!(error = %e, "Failed to get available trees");
                Vec::new()
            }
        }
    }

    /// Collect the locally Verified closure, including its root, parents first.
    pub(super) async fn collect_all_entries_for_bootstrap(
        &self,
        tree_id: &ID,
    ) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        let engine = instance.require_local_engine()?;
        let snapshot = engine.verified_snapshot(tree_id).await?;
        let mut entries =
            super::utils::collect_ancestors_to_send(engine.as_ref(), snapshot.tips(), &[]).await?;
        // Verified ancestry requires strictly increasing heights.
        entries.sort_by_key(Entry::height);
        Ok(entries)
    }

    /// Find Verified entries that a peer is missing. Peer tips are only a
    /// traversal boundary, never a source of local serving authority.
    pub(super) async fn find_missing_entries_for_peer(
        &self,
        our_tips: &[ID],
        peer_tips: &[ID],
    ) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        let engine = instance.require_local_engine()?;
        super::utils::collect_ancestors_to_send(engine.as_ref(), our_tips, peer_tips).await
    }
}
