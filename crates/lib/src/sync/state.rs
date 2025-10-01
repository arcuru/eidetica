//! Sync state tracking for managing synchronization progress and metadata.
//!
//! This module provides structures and functionality for tracking sync state
//! between peers, including sync cursors, metadata, and history.

use serde::{Deserialize, Serialize};

use crate::{
    Result, Transaction,
    crdt::doc::{Value, path},
    entry::ID,
    store::DocStore,
};

/// Tracks the synchronization position for a specific peer-tree relationship.
///
/// A sync cursor represents how far synchronization has progressed between
/// this database and a specific peer for a specific tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncCursor {
    /// The peer's public key
    pub peer_pubkey: String,
    /// The tree ID this cursor applies to
    pub tree_id: ID,
    /// The last entry ID that was successfully synchronized
    pub last_synced_entry: Option<ID>,
    /// Timestamp of the last successful sync
    pub last_sync_time: String,
    /// Number of entries synchronized in the last session
    pub last_sync_count: u64,
    /// Total number of entries synchronized with this peer for this tree
    pub total_synced_count: u64,
}

impl SyncCursor {
    /// Create a new sync cursor for a peer-tree relationship.
    pub fn new(peer_pubkey: String, tree_id: ID) -> Self {
        Self {
            peer_pubkey,
            tree_id,
            last_synced_entry: None,
            last_sync_time: chrono::Utc::now().to_rfc3339(),
            last_sync_count: 0,
            total_synced_count: 0,
        }
    }

    /// Update the cursor with a successful sync operation.
    pub fn update_sync(&mut self, last_entry: ID, count: u64) {
        self.last_synced_entry = Some(last_entry);
        self.last_sync_time = chrono::Utc::now().to_rfc3339();
        self.last_sync_count = count;
        self.total_synced_count += count;
    }

    /// Check if this cursor has any sync history.
    pub fn has_sync_history(&self) -> bool {
        self.last_synced_entry.is_some()
    }
}

/// Metadata about synchronization operations for a peer.
///
/// This tracks overall sync statistics and health information for a peer
/// relationship across all trees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncMetadata {
    /// The peer's public key
    pub peer_pubkey: String,
    /// Timestamp when sync relationship was first established
    pub sync_established: String,
    /// Timestamp of the last sync attempt (successful or failed)
    pub last_sync_attempt: String,
    /// Timestamp of the last successful sync
    pub last_successful_sync: Option<String>,
    /// Total number of successful sync operations
    pub successful_sync_count: u64,
    /// Total number of failed sync operations
    pub failed_sync_count: u64,
    /// Total number of entries synchronized
    pub total_entries_synced: u64,
    /// Estimated total bytes synchronized
    pub total_bytes_synced: u64,
    /// Average sync duration in milliseconds
    pub average_sync_duration_ms: f64,
    /// List of trees being synchronized with this peer
    pub synced_trees: Vec<ID>,
}

impl SyncMetadata {
    /// Create new sync metadata for a peer.
    pub fn new(peer_pubkey: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            peer_pubkey,
            sync_established: now.clone(),
            last_sync_attempt: now,
            last_successful_sync: None,
            successful_sync_count: 0,
            failed_sync_count: 0,
            total_entries_synced: 0,
            total_bytes_synced: 0,
            average_sync_duration_ms: 0.0,
            synced_trees: Vec::new(),
        }
    }

    /// Record a successful sync operation.
    pub fn record_successful_sync(&mut self, entries_count: u64, bytes: u64, duration_ms: f64) {
        let now = chrono::Utc::now().to_rfc3339();
        self.last_sync_attempt = now.clone();
        self.last_successful_sync = Some(now);
        self.successful_sync_count += 1;
        self.total_entries_synced += entries_count;
        self.total_bytes_synced += bytes;

        // Update average duration (simple running average)
        let total_ops = self.successful_sync_count as f64;
        self.average_sync_duration_ms =
            (self.average_sync_duration_ms * (total_ops - 1.0) + duration_ms) / total_ops;
    }

    /// Record a failed sync operation.
    pub fn record_failed_sync(&mut self) {
        self.last_sync_attempt = chrono::Utc::now().to_rfc3339();
        self.failed_sync_count += 1;
    }

    /// Add a tree to the list of synced trees if not already present.
    pub fn add_synced_tree(&mut self, tree_id: ID) {
        if !self.synced_trees.contains(&tree_id) {
            self.synced_trees.push(tree_id);
        }
    }

    /// Remove a tree from the list of synced trees.
    pub fn remove_synced_tree(&mut self, tree_id: &ID) {
        self.synced_trees.retain(|id| id != tree_id);
    }

    /// Calculate the success rate of sync operations.
    pub fn sync_success_rate(&self) -> f64 {
        let total = self.successful_sync_count + self.failed_sync_count;
        if total == 0 {
            0.0
        } else {
            self.successful_sync_count as f64 / total as f64
        }
    }
}

/// Record of a single sync operation for audit and debugging purposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncHistoryEntry {
    /// Unique ID for this sync operation
    pub sync_id: String,
    /// The peer involved in this sync
    pub peer_pubkey: String,
    /// The tree that was synchronized
    pub tree_id: ID,
    /// Timestamp when sync started
    pub started_at: String,
    /// Timestamp when sync completed (or failed)
    pub completed_at: String,
    /// Whether the sync was successful
    pub success: bool,
    /// Number of entries synchronized
    pub entries_count: u64,
    /// Estimated bytes transferred
    pub bytes_transferred: u64,
    /// Duration in milliseconds
    pub duration_ms: f64,
    /// Error message if sync failed
    pub error_message: Option<String>,
}

impl SyncHistoryEntry {
    /// Create a new sync history entry.
    pub fn new(peer_pubkey: String, tree_id: ID) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            sync_id: uuid::Uuid::new_v4().to_string(),
            peer_pubkey,
            tree_id,
            started_at: now.clone(),
            completed_at: now,
            success: false,
            entries_count: 0,
            bytes_transferred: 0,
            duration_ms: 0.0,
            error_message: None,
        }
    }

    /// Mark the sync as completed successfully.
    pub fn complete_success(&mut self, entries_count: u64, bytes: u64) {
        self.completed_at = chrono::Utc::now().to_rfc3339();
        self.success = true;
        self.entries_count = entries_count;
        self.bytes_transferred = bytes;
        self.calculate_duration();
    }

    /// Mark the sync as failed.
    pub fn complete_failure(&mut self, error: String) {
        self.completed_at = chrono::Utc::now().to_rfc3339();
        self.success = false;
        self.error_message = Some(error);
        self.calculate_duration();
    }

    /// Calculate the duration based on start and end times.
    fn calculate_duration(&mut self) {
        if let (Ok(start), Ok(end)) = (
            chrono::DateTime::parse_from_rfc3339(&self.started_at),
            chrono::DateTime::parse_from_rfc3339(&self.completed_at),
        ) {
            self.duration_ms = (end - start).num_milliseconds() as f64;
        }
    }
}

/// Manages sync state persistence in the sync tree.
pub struct SyncStateManager<'a> {
    /// The atomic operation for modifying the sync tree
    op: &'a Transaction,
}

impl<'a> SyncStateManager<'a> {
    /// Create a new sync state manager.
    pub fn new(op: &'a Transaction) -> Self {
        Self { op }
    }

    /// Get or create a sync cursor for a peer-tree relationship.
    pub fn get_sync_cursor(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_id: &ID,
    ) -> Result<SyncCursor> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let cursor_path = path!("cursors", peer_pubkey.as_ref(), tree_id.as_str());

        match sync_state.get_path_as::<String>(&cursor_path) {
            Ok(json) => serde_json::from_str(&json).map_err(|e| {
                crate::Error::Store(crate::store::StoreError::SerializationFailed {
                    store: "sync_state".to_string(),
                    reason: format!("Invalid cursor JSON: {e}"),
                })
            }),
            Err(_) => {
                // Create new cursor
                Ok(SyncCursor::new(
                    peer_pubkey.as_ref().to_string(),
                    tree_id.clone(),
                ))
            }
        }
    }

    /// Update a sync cursor.
    pub fn update_sync_cursor(&self, cursor: &SyncCursor) -> Result<()> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let cursor_path = path!("cursors", cursor.peer_pubkey, cursor.tree_id.as_str());
        let cursor_json = serde_json::to_string(cursor)?;
        sync_state.set_path(&cursor_path, cursor_json)?;
        Ok(())
    }

    /// Get or create sync metadata for a peer.
    pub fn get_sync_metadata(&self, peer_pubkey: impl AsRef<str>) -> Result<SyncMetadata> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let metadata_path = path!("metadata", peer_pubkey.as_ref());

        match sync_state.get_path_as::<String>(&metadata_path) {
            Ok(json) => serde_json::from_str(&json).map_err(|e| {
                crate::Error::Store(crate::store::StoreError::SerializationFailed {
                    store: "sync_state".to_string(),
                    reason: format!("Invalid metadata JSON: {e}"),
                })
            }),
            Err(_) => {
                // Create new metadata
                Ok(SyncMetadata::new(peer_pubkey.as_ref().to_string()))
            }
        }
    }

    /// Update sync metadata for a peer.
    pub fn update_sync_metadata(&self, metadata: &SyncMetadata) -> Result<()> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let metadata_path = path!("metadata", metadata.peer_pubkey);
        let metadata_json = serde_json::to_string(metadata)?;
        sync_state.set_path(&metadata_path, metadata_json)?;
        Ok(())
    }

    /// Add a sync history entry.
    pub fn add_sync_history(&self, history_entry: &SyncHistoryEntry) -> Result<()> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let history_path = path!("history", history_entry.sync_id);
        let history_json = serde_json::to_string(history_entry)?;
        sync_state.set_path(&history_path, history_json)?;
        Ok(())
    }

    /// Get sync history for a peer, optionally limited to recent entries.
    ///
    /// # Implementation Note
    /// This method navigates the nested map structure created by `DocStore::set_path()`.
    /// When using `set_path("history.sync_id", data)`, it creates a nested structure
    /// `{ "history": { "sync_id": data } }` rather than a flat key with dots.
    pub fn get_sync_history(
        &self,
        peer_pubkey: impl AsRef<str>,
        limit: Option<usize>,
    ) -> Result<Vec<SyncHistoryEntry>> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let all_data = sync_state.get_all()?;

        let mut history_entries = Vec::new();

        // The history data is stored as nested structure under the "history" key
        if let Some(Value::Doc(history_node)) = all_data.get("history") {
            // Iterate through all history entries (each is stored under its sync_id)
            for (_sync_id, value) in history_node.iter() {
                if let Value::Text(json_str) = value
                    && let Ok(history_entry) = serde_json::from_str::<SyncHistoryEntry>(json_str)
                    && history_entry.peer_pubkey == peer_pubkey.as_ref()
                {
                    history_entries.push(history_entry);
                }
            }
        }

        // Sort by start time (most recent first)
        history_entries.sort_by(|a, b| b.started_at.cmp(&a.started_at));

        // Apply limit if specified
        if let Some(limit) = limit {
            history_entries.truncate(limit);
        }

        Ok(history_entries)
    }

    /// Get all peers with sync state.
    ///
    /// # Implementation Note
    /// This method navigates the nested map structure created by `DocStore::set_path()`.
    /// The data is organized in nested maps like `{ "metadata": { "peer_key": data } }`
    /// and `{ "cursors": { "peer_key": { "tree_id": data } } }`.
    pub fn get_peers_with_sync_state(&self) -> Result<Vec<String>> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let all_data = sync_state.get_all()?;

        let mut peers = std::collections::HashSet::new();

        // Check metadata node for peers
        if let Some(Value::Doc(metadata_node)) = all_data.get("metadata") {
            for (peer_key, _) in metadata_node.iter() {
                peers.insert(peer_key.to_string());
            }
        }

        // Check cursors node for peers
        if let Some(Value::Doc(cursors_node)) = all_data.get("cursors") {
            for (peer_key, _) in cursors_node.iter() {
                peers.insert(peer_key.to_string());
            }
        }

        Ok(peers.into_iter().collect())
    }

    /// Clean up old sync history entries.
    ///
    /// # Implementation Note
    /// This method navigates the nested map structure created by `DocStore::set_path()`.
    /// History entries are stored as `{ "history": { "sync_id": data } }` and the
    /// method properly navigates this structure to find and clean old entries.
    pub fn cleanup_old_history(&self, max_age_days: u32) -> Result<usize> {
        let sync_state = self.op.get_store::<DocStore>("sync_state")?;
        let all_data = sync_state.get_all()?;

        let cutoff_time = chrono::Utc::now() - chrono::Duration::days(max_age_days as i64);
        let cutoff_str = cutoff_time.to_rfc3339();

        let mut cleaned_count = 0;

        // The history data is stored as nested structure under the "history" key
        if let Some(Value::Doc(history_node)) = all_data.get("history") {
            for (sync_id, value) in history_node.iter() {
                if let Value::Text(json_str) = value
                    && let Ok(history_entry) = serde_json::from_str::<SyncHistoryEntry>(json_str)
                    && history_entry.started_at < cutoff_str
                {
                    // Delete using the path format
                    sync_state.delete(format!("history.{sync_id}"))?;
                    cleaned_count += 1;
                }
            }
        }

        Ok(cleaned_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entry, Instance, backend::database::InMemory};

    #[test]
    fn test_sync_cursor() {
        let peer_pubkey = "test_peer".to_string();
        let tree_id = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully")
            .id()
            .clone();

        let mut cursor = SyncCursor::new(peer_pubkey.clone(), tree_id.clone());
        assert_eq!(cursor.peer_pubkey, peer_pubkey);
        assert_eq!(cursor.tree_id, tree_id);
        assert!(!cursor.has_sync_history());

        let entry_id = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully")
            .id()
            .clone();
        cursor.update_sync(entry_id.clone(), 5);
        assert!(cursor.has_sync_history());
        assert_eq!(cursor.last_synced_entry.unwrap(), entry_id);
        assert_eq!(cursor.last_sync_count, 5);
        assert_eq!(cursor.total_synced_count, 5);
    }

    #[test]
    fn test_sync_metadata() {
        let peer_pubkey = "test_peer".to_string();
        let mut metadata = SyncMetadata::new(peer_pubkey.clone());

        assert_eq!(metadata.peer_pubkey, peer_pubkey);
        assert_eq!(metadata.successful_sync_count, 0);
        assert_eq!(metadata.sync_success_rate(), 0.0);

        metadata.record_successful_sync(10, 1024, 100.0);
        assert_eq!(metadata.successful_sync_count, 1);
        assert_eq!(metadata.total_entries_synced, 10);
        assert_eq!(metadata.average_sync_duration_ms, 100.0);
        assert_eq!(metadata.sync_success_rate(), 1.0);

        metadata.record_failed_sync();
        assert_eq!(metadata.failed_sync_count, 1);
        assert_eq!(metadata.sync_success_rate(), 0.5);
    }

    #[tokio::test]
    async fn test_sync_state_manager() {
        let backend = InMemory::new();
        let db = Instance::new(Box::new(backend)).with_sync().unwrap();

        // Add a private key for authentication
        db.add_private_key("test_key").unwrap();

        // Create a user tree for testing tree ID
        let user_tree = db
            .new_database(crate::crdt::Doc::new(), "test_key")
            .unwrap();
        let tree_id = user_tree.root_id().clone();

        // Get the sync instance and its tree
        let sync = db.sync().unwrap();
        let sync_tree = &sync.sync_tree;
        let op = sync_tree.new_transaction().unwrap();

        let state_manager = SyncStateManager::new(&op);
        let peer_pubkey = "test_peer";

        // Test cursor management
        let mut cursor = state_manager
            .get_sync_cursor(peer_pubkey, &tree_id)
            .unwrap();
        assert!(!cursor.has_sync_history());

        let entry_id = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully")
            .id()
            .clone();
        cursor.update_sync(entry_id, 3);
        state_manager.update_sync_cursor(&cursor).unwrap();

        // Test metadata management
        let mut metadata = state_manager.get_sync_metadata(peer_pubkey).unwrap();
        metadata.record_successful_sync(3, 512, 50.0);
        state_manager.update_sync_metadata(&metadata).unwrap();

        // Test history
        let mut history_entry = SyncHistoryEntry::new(peer_pubkey.to_string(), tree_id.clone());
        history_entry.complete_success(3, 512);
        state_manager.add_sync_history(&history_entry).unwrap();

        // Commit the changes and test
        op.commit().unwrap();

        // Create a new operation on the sync tree and test that the history is persisted
        let op2 = sync_tree.new_transaction().unwrap();
        let state_manager2 = SyncStateManager::new(&op2);
        let history = state_manager2
            .get_sync_history(peer_pubkey, Some(10))
            .unwrap();

        // Verify that history is properly persisted and retrieved
        assert_eq!(history.len(), 1, "Should have one history entry");
        assert!(
            history[0].success,
            "History entry should be marked as success"
        );
        assert_eq!(history[0].entries_count, 3, "Should have synced 3 entries");
        assert_eq!(
            history[0].bytes_transferred, 512,
            "Should have transferred 512 bytes"
        );
    }
}
