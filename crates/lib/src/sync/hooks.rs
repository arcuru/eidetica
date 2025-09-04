//! Sync hooks for detecting entry changes during commit operations.
//!
//! This module provides the infrastructure for hooking into Transaction commit
//! operations to detect when new entries are created that need to be synchronized.

use crate::Result;
use crate::entry::{Entry, ID};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::background::SyncCommand;

/// Context information passed to sync hooks during commit operations.
#[derive(Debug, Clone)]
pub struct SyncHookContext {
    /// The tree ID where the entry was committed
    pub tree_id: ID,
    /// The newly committed entry
    pub entry: Entry,
    /// Whether this is the first entry in the tree (root entry)
    pub is_root_entry: bool,
}

/// Trait for implementing sync hooks that are called during entry commit operations.
///
/// Sync hooks allow the sync system to be notified when new entries are created
/// in trees that have sync relationships configured. This enables automatic
/// change detection and queuing of entries for synchronization.
pub trait SyncHook: Send + Sync {
    /// Called after an entry has been successfully committed to a tree.
    ///
    /// This method is called by Transaction.commit() after the entry has been
    /// persisted to the backend but before the commit operation returns.
    ///
    /// # Arguments
    /// * `context` - Information about the committed entry and its context
    ///
    /// # Returns
    /// A Result indicating whether the hook processed successfully.
    /// Hook failures do not rollback the commit, but may be logged.
    fn on_entry_committed(&self, context: &SyncHookContext) -> Result<()>;
}

/// A collection of sync hooks that can be executed together.
///
/// This allows multiple hooks to be registered and executed in sequence
/// during commit operations.
#[derive(Default)]
pub struct SyncHookCollection {
    hooks: Vec<Arc<dyn SyncHook>>,
}

impl SyncHookCollection {
    /// Create a new empty hook collection.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Add a sync hook to the collection.
    ///
    /// # Arguments
    /// * `hook` - The sync hook to add
    pub fn add_hook(&mut self, hook: Arc<dyn SyncHook>) {
        self.hooks.push(hook);
    }

    /// Execute all hooks in the collection with the given context.
    ///
    /// Hooks are executed in the order they were added. If a hook fails,
    /// execution continues with remaining hooks and errors are collected.
    ///
    /// # Arguments
    /// * `context` - The sync hook context to pass to each hook
    ///
    /// # Returns
    /// A Result that is Ok if all hooks succeeded, or contains the first error encountered.
    pub fn execute_hooks(&self, context: &SyncHookContext) -> Result<()> {
        let mut first_error = None;

        for hook in &self.hooks {
            if let Err(e) = hook.on_entry_committed(context) {
                tracing::error!("Sync hook failed: {e}");
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Check if there are any hooks registered.
    pub fn has_hooks(&self) -> bool {
        !self.hooks.is_empty()
    }

    /// Get the number of registered hooks.
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Check if the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

/// Command-based sync hook implementation that uses the background sync engine.
///
/// This hook sends commands to the background sync thread instead of directly
/// interacting with sync state, avoiding circular dependency issues.
pub struct SyncHookImpl {
    /// Command channel to the background sync engine
    pub command_tx: mpsc::Sender<SyncCommand>,
    /// Public key of the peer to sync with
    pub peer_pubkey: String,
}

impl SyncHookImpl {
    /// Create a new command-based sync hook
    pub fn new(command_tx: mpsc::Sender<SyncCommand>, peer_pubkey: String) -> Self {
        Self {
            command_tx,
            peer_pubkey,
        }
    }
}

impl SyncHook for SyncHookImpl {
    fn on_entry_committed(&self, context: &SyncHookContext) -> Result<()> {
        // Send QueueEntry command to background engine
        // Use try_send for non-blocking operation since this is called during commit
        let command = SyncCommand::QueueEntry {
            peer: self.peer_pubkey.clone(),
            entry_id: context.entry.id().clone(),
            tree_id: context.tree_id.clone(),
        };

        if let Err(e) = self.command_tx.try_send(command) {
            // Log error but don't fail the commit
            tracing::error!(
                "Failed to queue entry for sync with peer {}: {}",
                self.peer_pubkey,
                e
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Entry;

    struct TestHook {
        name: String,
        should_fail: bool,
    }

    impl TestHook {
        fn new(name: &str, should_fail: bool) -> Self {
            Self {
                name: name.to_string(),
                should_fail,
            }
        }
    }

    impl SyncHook for TestHook {
        fn on_entry_committed(&self, _context: &SyncHookContext) -> Result<()> {
            tracing::debug!("Hook {} executed", self.name);
            if self.should_fail {
                Err(crate::Error::Sync(crate::sync::SyncError::Network(
                    format!("Hook {} intentionally failed", self.name),
                )))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn test_sync_hook_collection_empty() {
        let collection = SyncHookCollection::new();
        assert!(collection.is_empty());
        assert_eq!(collection.len(), 0);
        assert!(!collection.has_hooks());
    }

    #[test]
    fn test_sync_hook_collection_execution() {
        let mut collection = SyncHookCollection::new();

        // Add some test hooks
        collection.add_hook(Arc::new(TestHook::new("hook1", false)));
        collection.add_hook(Arc::new(TestHook::new("hook2", false)));

        assert!(!collection.is_empty());
        assert_eq!(collection.len(), 2);
        assert!(collection.has_hooks());

        // Create test context
        let entry = Entry::builder("test_tree").build();
        let context = SyncHookContext {
            tree_id: entry.id().clone(),
            entry: entry.clone(),
            is_root_entry: true,
        };

        // Execute hooks - should succeed
        assert!(collection.execute_hooks(&context).is_ok());
    }

    #[test]
    fn test_sync_hook_collection_with_failure() {
        let mut collection = SyncHookCollection::new();

        // Add hooks, one that will fail
        collection.add_hook(Arc::new(TestHook::new("good_hook", false)));
        collection.add_hook(Arc::new(TestHook::new("bad_hook", true)));
        collection.add_hook(Arc::new(TestHook::new("another_good_hook", false)));

        // Create test context
        let entry = Entry::builder("test_tree").build();
        let context = SyncHookContext {
            tree_id: entry.id().clone(),
            entry: entry.clone(),
            is_root_entry: false,
        };

        // Execute hooks - should fail due to bad_hook, but all hooks should execute
        assert!(collection.execute_hooks(&context).is_err());
    }
}
