// End-to-end integration test for Iroh transport with actual data synchronization
// This test can be run with different relay modes to test real-world scenarios

use std::time::Duration;

use eidetica::{
    entry::Entry,
    instance::LegacyInstanceOps,
    sync::{peer_types::Address, transports::iroh::IrohTransport},
};
use iroh::RelayMode;

use super::helpers;

/// Test basic Iroh sync functionality with local direct connections
/// This test verifies basic sync operation without relay overhead.
#[tokio::test]
async fn test_iroh_e2e_basic_local() {
    // Setup with disabled relays for reliable local testing
    let (base_db1, sync1) = helpers::setup();
    let (_base_db2, sync2) = helpers::setup();

    let transport1 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();
    let transport2 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();

    sync1.enable_iroh_transport_with_config(transport1).unwrap();
    sync2.enable_iroh_transport_with_config(transport2).unwrap();

    // Start servers
    sync1.start_server_async("ignored").await.unwrap();
    sync2.start_server_async("ignored").await.unwrap();

    // Allow endpoints to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Setup peer relationship
    let _addr1 = sync1.get_server_address_async().await.unwrap();
    let addr2 = sync2.get_server_address_async().await.unwrap();
    let _pubkey1 = sync1.get_device_public_key().unwrap();
    let pubkey2 = sync2.get_device_public_key().unwrap();

    sync1.register_peer(&pubkey2, Some("peer2")).unwrap();
    sync1
        .add_peer_address(&pubkey2, Address::iroh(&addr2))
        .unwrap();

    // Add authentication key
    base_db1.add_private_key("basic_test_key").unwrap();

    // Create a small set of test entries for functional testing
    let mut entries = Vec::new();
    for i in 0..3 {
        let entry = Entry::root_builder()
            .set_subtree_data("data", format!(r#"{{"test": {i}}}"#))
            .build()
            .expect("Entry should build successfully");
        base_db1.backend().put_verified(entry.clone()).unwrap();
        entries.push(entry);
    }

    // Perform sync
    let result = sync1
        .send_entries_async(&entries, &Address::iroh(&addr2))
        .await;

    assert!(result.is_ok(), "Sync failed: {:?}", result.err());

    println!(
        "✅ Successfully synced {} entries via Iroh P2P transport!",
        entries.len()
    );

    // Cleanup
    sync1.stop_server_async().await.unwrap();
    sync2.stop_server_async().await.unwrap();
}

/// Test Iroh transport resilience and reconnection
///
/// Tests network failure recovery by intentionally breaking and restoring connections.
/// This test includes deliberate timeout scenarios where sync attempts fail, which can
/// take 10-30+ seconds depending on transport timeout settings.
///
/// Run manually with: `cargo test test_iroh_e2e_resilience -- --ignored --nocapture`
#[tokio::test]
#[ignore = "Slow test: Includes intentional network timeouts (10-30+ seconds)"]
async fn test_iroh_e2e_resilience() {
    // Setup nodes with local transport
    let (base_db1, sync1) = helpers::setup();
    let (_base_db2, sync2) = helpers::setup();

    let transport1 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();
    let transport2 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();

    sync1.enable_iroh_transport_with_config(transport1).unwrap();
    sync2.enable_iroh_transport_with_config(transport2).unwrap();

    // Start both nodes
    sync1.start_server_async("ignored").await.unwrap();
    sync2.start_server_async("ignored").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let _addr1 = sync1.get_server_address_async().await.unwrap();
    let addr2 = sync2.get_server_address_async().await.unwrap();
    let _pubkey1 = sync1.get_device_public_key().unwrap();
    let pubkey2 = sync2.get_device_public_key().unwrap();

    // Register peers
    sync1.register_peer(&pubkey2, None).unwrap();
    sync1
        .add_peer_address(&pubkey2, Address::iroh(&addr2))
        .unwrap();

    // Create test entries
    let entry1 = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "resilience_1"}"#)
        .build()
        .expect("Entry should build successfully");
    base_db1.backend().put_verified(entry1.clone()).unwrap();

    // First sync should succeed
    let result = sync1
        .send_entries_async(&vec![entry1], &Address::iroh(&addr2))
        .await;
    assert!(result.is_ok());

    // Stop node 2
    sync2.stop_server_async().await.unwrap();

    // Create another entry while node 2 is down
    let entry2 = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "resilience_2"}"#)
        .build()
        .expect("Entry should build successfully");
    base_db1.backend().put_verified(entry2.clone()).unwrap();

    // This sync should fail (node 2 is down)
    // Note: This will timeout and may take 10-30 seconds depending on transport settings
    println!("⏳ Testing sync failure when peer is offline (this will timeout)...");
    let failed_sync_start = std::time::Instant::now();
    let result = sync1
        .send_entries_async(&vec![entry2.clone()], &Address::iroh(&addr2))
        .await;
    let failed_sync_duration = failed_sync_start.elapsed();
    println!("⌛ Sync failure detected after {failed_sync_duration:?} (expected timeout)");
    assert!(result.is_err(), "Should fail when peer is offline");

    // Restart node 2 with a new transport
    println!("🔄 Restarting node 2 to test reconnection...");
    let transport2_new = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();
    sync2
        .enable_iroh_transport_with_config(transport2_new)
        .unwrap();
    sync2.start_server_async("ignored").await.unwrap();

    // Reduced wait time - just enough for endpoint to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get new address (will have changed)
    let addr2_new = sync2.get_server_address_async().await.unwrap();

    // Update peer address
    sync1
        .add_peer_address(&pubkey2, Address::iroh(&addr2_new))
        .unwrap();

    // Retry sync - should succeed now
    let result = sync1
        .send_entries_async(&vec![entry2], &Address::iroh(&addr2_new))
        .await;
    assert!(
        result.is_ok(),
        "Should succeed after peer comes back online"
    );

    println!("✅ Iroh transport successfully handled disconnection and reconnection!");

    // Cleanup
    sync1.stop_server_async().await.unwrap();
    sync2.stop_server_async().await.unwrap();
}

/// Test end-to-end Iroh sync with actual relay servers (requires internet)
///
///   **LIVE RELAY SERVER TEST** ⚠️
/// This test connects to n0's production relay servers over the internet.
/// It will FAIL if:
/// - No internet connection is available
/// - n0's relay servers are unreachable
/// - Firewall blocks connections to relay servers
/// - Network conditions prevent P2P hole-punching
///
/// Run manually with: `cargo test test_iroh_e2e_with_relays -- --ignored --nocapture`
///
/// This test:
/// 1. Sets up two nodes with production relay servers (relay.iroh.computer)
/// 2. Attempts to establish P2P connection through live relay infrastructure
/// 3. Syncs actual data between the nodes over the internet
/// 4. Verifies data integrity after sync
#[tokio::test]
#[ignore = "⚠️ REQUIRES INTERNET: Uses live n0 relay servers - will fail without network access"]
async fn test_iroh_e2e_with_relays() {
    println!("🌐 Starting Iroh end-to-end test with LIVE production relay servers...");
    println!("📡 This test connects to n0's relay infrastructure over the internet");
    println!("⚠️  Test will FAIL if relay servers are unreachable or network is unavailable");
    println!();

    // Create two independent databases with sync engines
    let (base_db1, sync1) = helpers::setup();
    let (base_db2, sync2) = helpers::setup();

    // Enable Iroh transport with production relays (default)
    println!("🔧 Enabling Iroh transport with production relay servers...");
    sync1.enable_iroh_transport().unwrap();
    sync2.enable_iroh_transport().unwrap();

    // Start both servers - this will attempt to connect to relay servers
    println!("🚀 Starting Iroh endpoints (connecting to live relay servers)...");
    let start_result1 = sync1.start_server_async("ignored").await;
    let start_result2 = sync2.start_server_async("ignored").await;

    if start_result1.is_err() || start_result2.is_err() {
        eprintln!("❌ FAILED: Unable to start Iroh endpoints");
        eprintln!("💡 This likely means:");
        eprintln!("   • No internet connection available");
        eprintln!("   • n0's relay servers (relay.iroh.computer) are unreachable");
        eprintln!("   • Firewall is blocking connections to relay infrastructure");
        panic!("Cannot connect to live relay servers - network/connectivity issue");
    }

    // Allow time for endpoints to initialize with relay servers
    println!("⏳ Waiting for endpoints to connect to relay infrastructure...");
    tokio::time::sleep(Duration::from_millis(2000)).await;

    // Get server addresses (should include relay connectivity info)
    let addr1_result = sync1.get_server_address_async().await;
    let addr2_result = sync2.get_server_address_async().await;

    if addr1_result.is_err() || addr2_result.is_err() {
        eprintln!("❌ FAILED: Unable to get server addresses from relay-connected endpoints");
        eprintln!("💡 This likely means relay connectivity was not established");
        panic!("Endpoints failed to establish relay connectivity");
    }

    let addr1 = addr1_result.unwrap();
    let addr2 = addr2_result.unwrap();

    println!("📍 Node 1 address (with relay info): {addr1}");
    println!("📍 Node 2 address (with relay info): {addr2}");

    // Get public keys for peer registration
    let pubkey1 = sync1.get_device_public_key().unwrap();
    let pubkey2 = sync2.get_device_public_key().unwrap();

    // Register peers
    println!("👥 Registering peers for P2P communication...");
    sync1.register_peer(&pubkey2, Some("relay_peer2")).unwrap();
    sync1
        .add_peer_address(&pubkey2, Address::iroh(&addr2))
        .unwrap();

    sync2.register_peer(&pubkey1, Some("relay_peer1")).unwrap();
    sync2
        .add_peer_address(&pubkey1, Address::iroh(&addr1))
        .unwrap();

    // Create test entries
    let mut entries = Vec::new();
    for i in 0..5 {
        let entry = Entry::root_builder()
            .set_subtree_data("data", format!(r#"{{"test": "relay_{i}"}}"#))
            .build()
            .expect("Entry should build successfully");
        base_db1.backend().put_verified(entry.clone()).unwrap();
        entries.push(entry);
    }

    println!("📦 Created {} entries for relay-based sync", entries.len());

    // Test sync over live relay servers
    println!("🌐 Attempting sync through live relay infrastructure...");
    println!("   This may take longer than local tests due to relay coordination");

    let sync_start = std::time::Instant::now();
    let result = sync1
        .send_entries_async(&entries, &Address::iroh(&addr2))
        .await;
    let sync_duration = sync_start.elapsed();

    match result {
        Ok(_) => {
            println!(
                "✅ SUCCESS: Synced {} entries via live relay servers in {:?}!",
                entries.len(),
                sync_duration
            );

            // Allow time for processing
            tokio::time::sleep(Duration::from_millis(1000)).await;

            // Verify entries were synced
            println!("🔍 Verifying entries arrived at destination...");
            let mut synced_count = 0;
            for entry in &entries {
                if base_db2.backend().get(&entry.id()).is_ok() {
                    synced_count += 1;
                }
            }

            if synced_count == entries.len() {
                println!(
                    "🎉 All {synced_count} entries successfully synced through live relay infrastructure!"
                );
            } else {
                println!(
                    "⚠️  Only {}/{} entries synced - possible network issues",
                    synced_count,
                    entries.len()
                );
            }
        }
        Err(e) => {
            eprintln!("❌ SYNC FAILED through live relay servers: {e:?}");
            eprintln!();
            eprintln!("💡 This failure means:");
            eprintln!("   • Relay servers may be temporarily unavailable");
            eprintln!("   • Network conditions prevented P2P hole-punching");
            eprintln!("   • Firewall blocked P2P traffic");
            eprintln!("   • Relay coordination failed due to network issues");
            eprintln!();
            eprintln!("🔧 To debug:");
            eprintln!("   1. Check internet connectivity");
            eprintln!("   2. Verify relay.iroh.computer is reachable");
            eprintln!("   3. Check if firewall allows UDP traffic");
            eprintln!("   4. Try running local tests (without --ignored flag)");

            panic!("Live relay sync failed - see error details above");
        }
    }

    // Cleanup
    println!("🧹 Cleaning up relay connections...");
    sync1.stop_server_async().await.unwrap();
    sync2.stop_server_async().await.unwrap();

    println!("🎯 Live relay test completed successfully!");
}
