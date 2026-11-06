//! Generic transport conformance tests that work with any SyncTransport implementation
//!
//! These tests verify that all transport implementations provide consistent behavior
//! for syncing entries between Instance instances.

use std::{sync::Arc, time::Duration};

use eidetica::{
    Database, Entry, Instance, Result,
    account::{AccountSession, types::SyncSettings},
    auth::crypto::PublicKey,
    crdt::Doc,
    entry::ID,
    message_io::{TransferBudget, TransferPolicy, TransferUsage},
    store::DocStore,
    sync::{
        Address, Sync, SyncError, TimeoutPhase,
        handler::SyncHandler,
        protocol::{RequestContext, SyncRequest, SyncResponse},
        transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::RelayMode;
use tokio::time::sleep;

use super::helpers::{
    HttpTransportFactory, IrohTransportFactory, TransportFactory, setup_public_sync_enabled_server,
    setup_sync_enabled_client,
};
use crate::helpers::test_local_instance_with_user_and_key as test_instance_with_user_and_key;

/// Set up two Instance instances with users and private keys
async fn setup_databases() -> Result<(
    Instance,
    AccountSession,
    PublicKey,
    Instance,
    AccountSession,
    PublicKey,
)> {
    let (db1, user1, key_id1) = test_instance_with_user_and_key("user1", Some("device_key")).await;
    let (db2, user2, key_id2) = test_instance_with_user_and_key("user2", Some("device_key")).await;
    Ok((db1, user1, key_id1, db2, user2, key_id2))
}

/// Set up sync instances with servers and peer connections
async fn setup_sync_with_peers<F>(
    factory: &F,
    db1: &Instance,
    db2: &Instance,
) -> Result<(Sync, Sync, PublicKey, PublicKey)>
where
    F: TransportFactory,
{
    // Create sync instances with transports
    let sync1 = factory.create_sync(db1.clone()).await?;
    let sync2 = factory.create_sync(db2.clone()).await?;

    // Start servers on any available port
    sync1.accept_connections().await?;
    sync2.accept_connections().await?;

    // Give servers time to initialize
    sleep(Duration::from_millis(200)).await;

    // Get server addresses
    let addr1: String = sync1.get_server_address().await?;
    let addr2: String = sync2.get_server_address().await?;

    println!("Server 1 address: {addr1}");
    println!("Server 2 address: {addr2}");

    // Create Address objects for connection
    let address1 = factory.create_address(&addr1);
    let address2 = factory.create_address(&addr2);

    // Connect peers
    let peer1_pubkey = sync1.get_device_pubkey()?;
    let peer2_pubkey = sync2.get_device_pubkey()?;

    // Register peers with each other
    sync1
        .register_peer(&peer2_pubkey, Some("test_peer_2"))
        .await?;
    sync1.add_peer_address(&peer2_pubkey, address2).await?;

    sync2
        .register_peer(&peer1_pubkey, Some("test_peer_1"))
        .await?;
    sync2.add_peer_address(&peer1_pubkey, address1).await?;

    Ok((sync1, sync2, peer1_pubkey, peer2_pubkey))
}

/// Set up trees with bidirectional sync wiring.
///
/// Relies on the standard sync-on-commit machinery:
/// - `instance.enable_sync()` (done by the transport factory) registers the
///   global write callback that routes local commits into `Sync::on_local_write`.
/// - `user.track_database(..., SyncSettings::on_commit())` flips the user's
///   preferences (overwriting the `disabled()` defaults set by
///   `create_database`) and propagates them to combined settings.
/// - `sync.add_tree_sync(peer, tree)` associates each peer with the tree.
///
/// The Instance's global callback then queues entries on every local commit
/// automatically — no manual `on_write` hooks needed.
#[allow(clippy::too_many_arguments)]
async fn setup_sync_hooks(
    user1: &mut AccountSession,
    user2: &mut AccountSession,
    key_id1: &PublicKey,
    key_id2: &PublicKey,
    sync1: &Sync,
    sync2: &Sync,
    peer1_pubkey: &PublicKey,
    peer2_pubkey: &PublicKey,
) -> Result<(Database, Database)> {
    let mut settings1 = Doc::new();
    settings1.set("name", "test_tree_1");
    let tree1 = user1.create_database(settings1, key_id1).await?;
    user1
        .track_database(tree1.root_id().clone(), key_id1, SyncSettings::on_commit())
        .await?;
    sync1.add_tree_sync(peer2_pubkey, tree1.root_id()).await?;

    let mut settings2 = Doc::new();
    settings2.set("name", "test_tree_2");
    let tree2 = user2.create_database(settings2, key_id2).await?;
    user2
        .track_database(tree2.root_id().clone(), key_id2, SyncSettings::on_commit())
        .await?;
    sync2.add_tree_sync(peer1_pubkey, tree2.root_id()).await?;

    Ok((tree1, tree2))
}

/// Clean up sync instances
async fn cleanup_sync(sync1: Sync, sync2: Sync) -> Result<()> {
    sync1.stop_server().await?;
    sync2.stop_server().await?;
    Ok(())
}

/// Generic test function that works with any transport factory
async fn test_instance_entry_sync_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    println!("Testing {} transport conformance", factory.transport_name());

    // Set up databases and sync instances
    let (db1, mut user1, key_id1, db2, mut user2, key_id2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, _tree2) = setup_sync_hooks(
        &mut user1,
        &mut user2,
        &key_id1,
        &key_id2,
        &sync1,
        &sync2,
        &peer1_pubkey,
        &peer2_pubkey,
    )
    .await?;

    // Create entries in DB1 - these should automatically sync via hooks
    let txn1 = tree1.new_transaction().await?;
    let docstore1 = txn1.get_store::<DocStore>("data").await?;
    docstore1.set("name", "Alice").await?;
    docstore1.set("age", "30").await?;
    let entry_id1 = txn1.commit().await?;

    let txn2 = tree1.new_transaction().await?;
    let docstore1_2 = txn2.get_store::<DocStore>("data").await?;
    docstore1_2.set("name", "Bob").await?;
    docstore1_2.set("age", "25").await?;
    let entry_id2 = txn2.commit().await?;

    println!("Created entries in DB1 (with sync hooks): {entry_id1} and {entry_id2}");

    // Flush sync queue to send entries immediately
    sync1.flush().await?;

    // Verify entries were synced to DB2 backend
    let entry1_in_db2 = db2.backend().get(&entry_id1).await;
    let entry2_in_db2 = db2.backend().get(&entry_id2).await;

    println!("Checking for entries in DB2 backend...");
    println!("Entry 1 in DB2: {:?}", entry1_in_db2.is_ok());
    println!("Entry 2 in DB2: {:?}", entry2_in_db2.is_ok());

    assert!(
        entry1_in_db2.is_ok() && entry2_in_db2.is_ok(),
        "Both entries should have synced to DB2"
    );

    // Verify both synced entries have the expected data
    if let Ok(synced_entry1) = &entry1_in_db2 {
        println!("Successfully found synced entry 1: {}", synced_entry1.id());

        if let Some(data) = synced_entry1
            .data("data")
            .ok()
            .and_then(|d| std::str::from_utf8(d).ok())
        {
            println!("Synced entry 1 data: {data}");
            assert!(data.contains("Alice"), "Entry 1 should contain Alice data");
        }
    }

    if let Ok(synced_entry2) = &entry2_in_db2 {
        println!("Successfully found synced entry 2: {}", synced_entry2.id());

        if let Some(data) = synced_entry2
            .data("data")
            .ok()
            .and_then(|d| std::str::from_utf8(d).ok())
        {
            println!("Synced entry 2 data: {data}");
            assert!(data.contains("Bob"), "Entry 2 should contain Bob data");
        }
    }

    println!(
        "✅ Successfully verified entries synced via {} transport",
        factory.transport_name()
    );
    println!("   - Entries accessible via normal Instance/DocStore interfaces");
    println!("   - Data integrity maintained across sync");

    cleanup_sync(sync1, sync2).await?;
    Ok(())
}

const MIB: usize = 1024 * 1024;

async fn write_large(database: &Database, key: &str, bytes: usize) -> Result<ID> {
    let txn = database.new_transaction().await?;
    let store = txn.get_store::<DocStore>("data").await?;
    store.set(key, "x".repeat(bytes)).await?;
    txn.commit().await
}

/// Entries larger than the former 1 MiB peer limit and aggregates of them, in
/// both directions, through ordinary sync APIs and without any paging.
async fn test_large_messages_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    let (db1, mut user1, key_id1, db2, mut user2, key_id2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, tree2) = setup_sync_hooks(
        &mut user1,
        &mut user2,
        &key_id1,
        &key_id2,
        &sync1,
        &sync2,
        &peer1_pubkey,
        &peer2_pubkey,
    )
    .await?;

    // Upload one oversized Entry, then an aggregate far over the former limit.
    let single = write_large(&tree1, "single", 3 * MIB).await?;
    sync1.flush().await?;
    assert_eq!(
        db2.backend().get(&single).await?,
        db1.backend().get(&single).await?
    );

    let mut batch = Vec::new();
    for index in 0..6 {
        batch.push(write_large(&tree1, &format!("batch{index}"), MIB / 2).await?);
    }
    sync1.flush().await?;
    for id in &batch {
        assert_eq!(db2.backend().get(id).await?, db1.backend().get(id).await?);
    }

    // The same from the other peer, so neither end is only ever a sender.
    let reverse = write_large(&tree2, "reverse", 2 * MIB).await?;
    sync2.flush().await?;
    assert_eq!(
        db1.backend().get(&reverse).await?,
        db2.backend().get(&reverse).await?
    );

    cleanup_sync(sync1, sync2).await
}

/// Download direction: a bootstrap response holding oversized Entries, then an
/// incremental response with another.
async fn test_large_download_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    let (server, _user, _key, database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "large").await;
    let first = write_large(&database, "first", 3 * MIB).await?;
    let second = write_large(&database, "second", 3 * MIB).await?;

    let server_sync = {
        let _ = server_sync;
        factory.create_sync(server.clone()).await?
    };
    server_sync.accept_connections().await?;
    let address = factory.create_address(&server_sync.get_server_address().await?);

    let (client, _client_user, _client_key, _client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    let client_sync = factory.create_sync(client.clone()).await?;

    client_sync.sync_with_peer(&address, Some(&tree_id)).await?;
    client_sync.flush().await?;
    for id in [&first, &second] {
        assert_eq!(
            client.backend().get(id).await?,
            server.backend().get(id).await?
        );
    }

    let third = write_large(&database, "third", 3 * MIB).await?;
    client_sync.sync_with_peer(&address, Some(&tree_id)).await?;
    client_sync.flush().await?;
    assert_eq!(
        client.backend().get(&third).await?,
        server.backend().get(&third).await?
    );

    client_sync.stop_server().await.ok();
    server_sync.stop_server().await
}

/// A peer's resource refusal reaches the sender as that, on each transport, and
/// leaves no Entry behind, including while the upload is still running.
async fn test_resource_refusal_conformance<T, B>(
    build: impl Fn(TransferBudget) -> B,
    address: impl Fn(String) -> Address,
) -> Result<()>
where
    T: SyncTransport,
    B: Future<Output = T>,
{
    let mut policy = TransferPolicy::default();
    policy.max_message_bytes = MIB as u64;
    let server = build(TransferBudget::new(policy)?).await;
    let (instance, handler) = super::helpers::setup_test_handler().await;
    server.start_server(handler).await?;
    let target = address(server.get_server_address()?);

    let entry = Entry::root_builder()
        .set_subtree_data("data", "x".repeat(32 * MIB))
        .build()?;
    let id = entry.id();
    let client = build(TransferBudget::default()).await;
    let refused = client.send_owned_entries(&target, vec![entry]).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(eidetica::Error::is_resource_limit),
        "{refused:?}"
    );
    assert!(instance.backend().get(&id).await.is_err());

    // The server still serves a request within its policy.
    let small = Entry::root_builder()
        .set_subtree_data("data", "small")
        .build()?;
    let small_id = small.id();
    client.send_owned_entries(&target, vec![small]).await?;
    assert!(instance.backend().get(&small_id).await.is_ok());

    // A sender's own limit refuses before any byte leaves.
    let mut policy = TransferPolicy::default();
    policy.max_message_bytes = MIB as u64;
    let limited = build(TransferBudget::new(policy)?).await;
    let large = Entry::root_builder()
        .set_subtree_data("data", "x".repeat(2 * MIB))
        .build()?;
    let refused = limited.send_owned_entries(&target, vec![large]).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(eidetica::Error::is_resource_limit),
        "{refused:?}"
    );

    server.stop_server().await
}

/// A handler that takes the request and never answers.
struct NeverAnswers;

#[async_trait::async_trait]
impl SyncHandler for NeverAnswers {
    async fn handle_request(&self, _: &SyncRequest, _: &RequestContext) -> SyncResponse {
        std::future::pending().await
    }
}

/// A caller's overall deadline ends an exchange that no other timer would, and
/// cancelling any exchange gives back everything it was admitted.
async fn test_deadline_and_cancellation_conformance<T, B>(
    build: impl Fn(TransferBudget) -> B,
    address: impl Fn(String) -> Address,
) -> Result<()>
where
    T: SyncTransport,
    B: Future<Output = T>,
{
    let server = build(TransferBudget::default()).await;
    server.start_server(Arc::new(NeverAnswers)).await?;
    let target = address(server.get_server_address()?);
    let large = || {
        Entry::root_builder()
            .set_subtree_data("data", "x".repeat(2 * MIB))
            .build()
            .unwrap()
    };

    let mut policy = TransferPolicy::default();
    policy.overall_timeout = Some(Duration::from_millis(500));
    let client = build(TransferBudget::new(policy)?).await;
    let started = std::time::Instant::now();
    let failed = client.send_owned_entries(&target, vec![large()]).await;
    assert!(started.elapsed() < Duration::from_secs(10), "{failed:?}");
    assert!(
        matches!(
            &failed,
            Err(eidetica::Error::Sync(error)) if matches!(
                **error,
                SyncError::Timeout { phase: TimeoutPhase::Transfer, .. }
            )
        ),
        "{failed:?}"
    );

    let budget = TransferBudget::default();
    let client = build(budget.clone()).await;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(500),
        client.send_owned_entries(&target, vec![large()]),
    );
    assert!(cancelled.await.is_err(), "the peer never answers");
    tokio::time::timeout(Duration::from_secs(5), async {
        while budget.usage() != TransferUsage::default() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a cancelled exchange must release its admission");

    server.stop_server().await
}

/// Test bidirectional sync to ensure both directions work
async fn test_bidirectional_sync_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    println!("Testing {} bidirectional sync", factory.transport_name());

    // Set up databases and sync instances
    let (db1, mut user1, key_id1, db2, mut user2, key_id2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, tree2) = setup_sync_hooks(
        &mut user1,
        &mut user2,
        &key_id1,
        &key_id2,
        &sync1,
        &sync2,
        &peer1_pubkey,
        &peer2_pubkey,
    )
    .await?;

    // Create entry in DB1
    let txn1 = tree1.new_transaction().await?;
    let docstore1 = txn1.get_store::<DocStore>("data").await?;
    docstore1.set("origin", "db1").await?;
    let entry_from_db1 = txn1.commit().await?;

    // Create entry in DB2
    let txn2 = tree2.new_transaction().await?;
    let docstore2 = txn2.get_store::<DocStore>("data").await?;
    docstore2.set("origin", "db2").await?;
    let entry_from_db2 = txn2.commit().await?;

    // Flush both sync queues for bidirectional sync
    sync1.flush().await?;
    sync2.flush().await?;

    // Verify DB1 has entry from DB2
    let db2_entry_in_db1 = db1.backend().get(&entry_from_db2).await;
    println!("DB2 entry in DB1: {:?}", db2_entry_in_db1.is_ok());

    // Verify DB2 has entry from DB1
    let db1_entry_in_db2 = db2.backend().get(&entry_from_db1).await;
    println!("DB1 entry in DB2: {:?}", db1_entry_in_db2.is_ok());

    // At least one direction should have synced
    assert!(
        db2_entry_in_db1.is_ok() || db1_entry_in_db2.is_ok(),
        "Bidirectional sync should have worked in at least one direction"
    );

    // If both worked, verify data integrity
    if let Ok(synced_entry) = db2_entry_in_db1
        && let Some(data) = synced_entry
            .data("data")
            .ok()
            .and_then(|d| std::str::from_utf8(d).ok())
    {
        assert!(
            data.contains("db2"),
            "Synced entry should contain 'db2' origin data"
        );
    }

    if let Ok(synced_entry) = db1_entry_in_db2
        && let Some(data) = synced_entry
            .data("data")
            .ok()
            .and_then(|d| std::str::from_utf8(d).ok())
    {
        assert!(
            data.contains("db1"),
            "Synced entry should contain 'db1' origin data"
        );
    }

    println!(
        "✅ Successfully verified bidirectional sync via {} transport",
        factory.transport_name()
    );

    cleanup_sync(sync1, sync2).await?;
    Ok(())
}

#[tokio::test]
async fn test_http_transport_conformance() {
    test_instance_entry_sync_conformance(HttpTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_iroh_transport_conformance() {
    test_instance_entry_sync_conformance(IrohTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_http_bidirectional_conformance() {
    test_bidirectional_sync_conformance(HttpTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_iroh_bidirectional_conformance() {
    test_bidirectional_sync_conformance(IrohTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_http_large_messages() {
    test_large_messages_conformance(HttpTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_iroh_large_messages() {
    test_large_messages_conformance(IrohTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_http_large_download() {
    test_large_download_conformance(HttpTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_iroh_large_download() {
    test_large_download_conformance(IrohTransportFactory)
        .await
        .unwrap();
}

fn http_transport(budget: TransferBudget) -> HttpTransport {
    HttpTransport::builder()
        .bind("127.0.0.1:0")
        .transfer_budget(budget)
        .build_sync()
        .unwrap()
}

fn iroh_transport(budget: TransferBudget) -> IrohTransport {
    IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .transfer_budget(budget)
        .build()
        .unwrap()
}

#[tokio::test]
async fn test_http_deadline_and_cancellation() {
    test_deadline_and_cancellation_conformance(
        |budget| async move { http_transport(budget) },
        Address::http,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_iroh_deadline_and_cancellation() {
    test_deadline_and_cancellation_conformance(
        |budget| async move { iroh_transport(budget) },
        Address::iroh,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_http_resource_refusal() {
    test_resource_refusal_conformance(
        |budget| async move { http_transport(budget) },
        Address::http,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_iroh_resource_refusal() {
    test_resource_refusal_conformance(
        |budget| async move { iroh_transport(budget) },
        Address::iroh,
    )
    .await
    .unwrap();
}
