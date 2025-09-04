//! Tests for sync queue and flush worker integration
//!
//! This module tests the integration between:
//! - AtomicOp hooks detecting entry changes
//! - In-memory sync queue collecting entries for peers
//! - Flush worker processing queued entries

use crate::sync::helpers::setup;
use eidetica::Result;
use eidetica::sync::peer_types::{Address, PeerStatus};
use std::time::Duration;
use tokio::time::sleep;

/// Test direct queue operations without full BaseDB integration
#[tokio::test]
async fn test_sync_queue_operations() -> Result<()> {
    let (_basedb, sync) = setup();
    let peer_pubkey = "ed25519:test_peer_key";

    // Test getting sync queue
    let queue = sync.sync_queue();
    assert_eq!(queue.total_entries(), 0, "Queue should start empty");

    // Test adding entries directly to queue
    let entry_id1 = eidetica::Entry::builder("entry1".to_string())
        .build()
        .id()
        .clone();
    let entry_id2 = eidetica::Entry::builder("entry2".to_string())
        .build()
        .id()
        .clone();
    let tree_id = eidetica::Entry::builder("test_tree".to_string())
        .build()
        .id()
        .clone();

    // Add entries
    assert!(
        queue.queue_entry(peer_pubkey, &entry_id1, &tree_id)?,
        "First entry should be added"
    );
    assert!(
        queue.queue_entry(peer_pubkey, &entry_id2, &tree_id)?,
        "Second entry should be added"
    );
    assert!(
        !queue.queue_entry(peer_pubkey, &entry_id1, &tree_id)?,
        "Duplicate entry should not be added"
    );

    // Test getting pending entries
    let pending = queue.get_pending_entries(peer_pubkey)?;
    assert_eq!(pending.len(), 2, "Should have 2 pending entries");

    Ok(())
}

/// Test that flush worker processes queued entries
#[tokio::test]
async fn test_flush_worker_processes_queue() -> Result<()> {
    let (_basedb, mut sync) = setup();
    let peer_pubkey = "ed25519:test_peer_key";

    // Register a peer and add address
    sync.register_peer(peer_pubkey, Some("Test Peer"))?;
    sync.update_peer_status(peer_pubkey, PeerStatus::Active)?;
    sync.add_peer_address(peer_pubkey, Address::http("127.0.0.1:8080"))?;

    // Start the flush worker
    sync.start_flush_worker_async().await?;

    // Verify worker is running
    assert!(
        sync.is_flush_worker_running_async().await,
        "Flush worker should be running"
    );

    // Add entries directly to queue to test flushing
    let queue = sync.sync_queue();
    for i in 0..3 {
        let entry_id = eidetica::Entry::builder(format!("entry_{i}"))
            .build()
            .id()
            .clone();
        let tree_id_obj = eidetica::Entry::builder("test_tree".to_string())
            .build()
            .id()
            .clone();
        queue.queue_entry(peer_pubkey, &entry_id, &tree_id_obj)?;
    }

    // Verify entries are queued
    let pending_before = queue.get_pending_entries(peer_pubkey)?;
    assert_eq!(pending_before.len(), 3, "Should have 3 pending entries");

    // Wait for flush worker to process (it should process due to queue size)
    // Note: This is a basic test - the flush worker will try to "send" but fail
    // since there's no actual server running. That's expected for this test.
    sleep(Duration::from_secs(2)).await;

    // Stop the flush worker
    sync.stop_flush_worker_async().await?;
    assert!(
        !sync.is_flush_worker_running_async().await,
        "Flush worker should be stopped"
    );

    Ok(())
}

/// Test flush worker lifecycle management
#[tokio::test]
async fn test_flush_worker_lifecycle() -> Result<()> {
    let (_basedb, mut sync) = setup();

    // Initially worker should not be running
    assert!(
        !sync.is_flush_worker_running_async().await,
        "Worker should not be running initially"
    );

    // Start worker
    sync.start_flush_worker_async().await?;
    assert!(
        sync.is_flush_worker_running_async().await,
        "Worker should be running after start"
    );

    // Starting again should fail
    let result = sync.start_flush_worker_async().await;
    assert!(result.is_err(), "Starting worker twice should fail");

    // Stop worker
    sync.stop_flush_worker_async().await?;
    assert!(
        !sync.is_flush_worker_running_async().await,
        "Worker should be stopped"
    );

    // Stopping again should succeed (no-op)
    sync.stop_flush_worker_async().await?;
    assert!(
        !sync.is_flush_worker_running_async().await,
        "Worker should still be stopped"
    );

    Ok(())
}

/// Test sync queue management with peer states
#[tokio::test]
async fn test_queue_with_peer_management() -> Result<()> {
    let (_basedb, mut sync) = setup();
    let peer_pubkey = "ed25519:test_peer_key";

    // Register peer
    sync.register_peer(peer_pubkey, Some("Test Peer"))?;

    // Test queue operations
    let queue = sync.sync_queue();
    let entry_id = eidetica::Entry::builder("entry1".to_string())
        .build()
        .id()
        .clone();
    let tree_id = eidetica::Entry::builder("test_tree".to_string())
        .build()
        .id()
        .clone();

    // Test basic queuing
    queue.queue_entry(peer_pubkey, &entry_id, &tree_id)?;
    let pending = queue.get_pending_entries(peer_pubkey)?;
    assert_eq!(pending.len(), 1, "Should have 1 pending entry");

    // Test cleanup - entries with 0 attempts should NOT be cleaned with max_retries=0
    // because has_exceeded_retries(0) means attempts >= 0, which is true for 0 attempts
    queue.cleanup_failed_entries(0)?; // Clean entries with 0 retries max (will clean all)
    let pending_after = queue.get_pending_entries(peer_pubkey)?;
    assert_eq!(
        pending_after.len(),
        0,
        "Entry with 0 attempts exceeds max_retries=0"
    );

    // Add another entry and test with higher limit
    queue.queue_entry(peer_pubkey, &entry_id, &tree_id)?;
    queue.cleanup_failed_entries(3)?; // Don't clean entries with <3 attempts
    let pending_final = queue.get_pending_entries(peer_pubkey)?;
    assert_eq!(
        pending_final.len(),
        1,
        "Entry with 0 attempts should remain with max_retries=3"
    );

    Ok(())
}

/// Test queue size limits trigger flushing
#[tokio::test]
async fn test_queue_size_triggers_flush() -> Result<()> {
    let (_basedb, mut sync) = setup();
    let peer_pubkey = "ed25519:test_peer_key";

    // Register peer
    sync.register_peer(peer_pubkey, Some("Test Peer"))?;
    sync.update_peer_status(peer_pubkey, PeerStatus::Active)?;

    // Get the queue and add entries up to the size limit
    let queue = sync.sync_queue();
    let config = queue.config();
    let max_size = config.max_queue_size;

    // Add entries to reach the limit
    for i in 0..max_size {
        let entry_id = eidetica::Entry::builder(format!("entry_{i}"))
            .build()
            .id()
            .clone();
        let tree_id = eidetica::Entry::builder("test_tree".to_string())
            .build()
            .id()
            .clone();
        queue.queue_entry(peer_pubkey, &entry_id, &tree_id)?;
    }

    // Check that this peer is marked as needing flush
    let peers_needing_flush = queue.get_peers_needing_flush()?;
    assert!(
        peers_needing_flush.contains(&peer_pubkey.to_string()),
        "Peer should need flushing when queue size limit reached"
    );

    Ok(())
}
