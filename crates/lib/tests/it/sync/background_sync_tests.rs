//! Tests for background sync functionality.
//!
//! This module tests the background sync engine including scheduler,
//! flush worker, and lifecycle management.

use super::helpers;
use eidetica::entry::Entry;
use eidetica::sync::scheduler::SyncSchedulerConfig;
use std::time::Duration;

#[tokio::test]
async fn test_start_stop_background_sync() {
    let (_base_db, sync) = helpers::setup();
    
    // Initially no background sync should be running
    let (scheduler_running, flush_worker_running) = sync.is_background_sync_running().await;
    assert!(!scheduler_running);
    assert!(!flush_worker_running);
    
    // Start background sync
    let config = SyncSchedulerConfig {
        sync_interval_secs: 60,
        enabled: true,
    };
    sync.start_legacy_background_sync(Some(config)).await.unwrap();
    
    // Check that flush worker is running (scheduler is disabled for now)
    let (scheduler_running, flush_worker_running) = sync.is_background_sync_running().await;
    assert!(!scheduler_running); // Currently disabled
    assert!(flush_worker_running);
    
    // Stop background sync
    sync.stop_background_sync().await.unwrap();
    
    // Check that both are stopped
    let (scheduler_running, flush_worker_running) = sync.is_background_sync_running().await;
    assert!(!scheduler_running);
    assert!(!flush_worker_running);
}

#[tokio::test]
async fn test_background_sync_double_start() {
    let (_base_db, sync) = helpers::setup();
    
    // Start background sync twice - should not error
    sync.start_legacy_background_sync(None).await.unwrap();
    sync.start_legacy_background_sync(None).await.unwrap();
    
    // Should still be running
    let (_, flush_worker_running) = sync.is_background_sync_running().await;
    assert!(flush_worker_running);
    
    // Cleanup
    sync.stop_background_sync().await.unwrap();
}

#[tokio::test]
async fn test_background_sync_double_stop() {
    let (_base_db, sync) = helpers::setup();
    
    // Start then stop twice - should not error
    sync.start_legacy_background_sync(None).await.unwrap();
    sync.stop_background_sync().await.unwrap();
    sync.stop_background_sync().await.unwrap();
    
    // Should be stopped
    let (scheduler_running, flush_worker_running) = sync.is_background_sync_running().await;
    assert!(!scheduler_running);
    assert!(!flush_worker_running);
}

#[tokio::test]
async fn test_flush_worker_processes_queue() {
    let (_base_db, sync) = helpers::setup();
    
    // Add some entries to the sync queue
    let peer_pubkey = "test_peer";
    let entry1 = Entry::builder("test_tree")
        .set_subtree_data("data", r#"{"test": "entry1"}"#)
        .build();
    let entry2 = Entry::builder("test_tree") 
        .set_subtree_data("data", r#"{"test": "entry2"}"#)
        .build();
    let tree_id = entry1.id().clone();
    
    sync.sync_queue()
        .queue_entry(peer_pubkey, &entry1.id(), &tree_id)
        .unwrap();
    sync.sync_queue()
        .queue_entry(peer_pubkey, &entry2.id(), &tree_id) 
        .unwrap();
    
    // Start background sync to process the queue
    sync.start_legacy_background_sync(None).await.unwrap();
    
    // Wait a moment for processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Stop background sync
    sync.stop_background_sync().await.unwrap();
    
    // Test passes if no panics occurred during queue processing
}

#[tokio::test] 
async fn test_bidirectional_sync_basic() {
    let (_base_db, sync) = helpers::setup();
    
    // Basic test that sync tree with peer method exists and can be called
    // (even though it will fail without transport setup)
    let entry_id: eidetica::entry::ID = "test_entry".into();
    let peer_pubkey = "test_peer";
    let result = sync.sync_tree_with_peer(peer_pubkey, &entry_id).await;
    
    // Should fail due to no peer configured, which is expected
    assert!(result.is_err());
}