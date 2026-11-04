//! Synthetic native transport regressions. Daemon restart coverage is separate.

use eidetica::{
    Database, Instance,
    account::types::SyncSettings,
    auth::{AuthKey, Permission},
    backend::VerificationStatus,
    crdt::Doc,
    entry::ID,
    store::DocStore,
    sync::{
        Address, Sync,
        transports::{http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::{RelayMode, endpoint::PortmapperConfig};

async fn transport(instance: &Instance, iroh: bool) -> (Sync, Address) {
    instance.enable_sync().await.unwrap();
    let sync = instance.sync().unwrap();
    if iroh {
        sync.register_transport(
            "peer",
            IrohTransport::builder()
                .relay_mode(RelayMode::Disabled)
                .mdns(false)
                .n0_dns(false)
                .portmapper_config(PortmapperConfig::Disabled),
        )
        .await
        .unwrap();
    } else {
        sync.register_transport("peer", HttpTransport::builder().bind("127.0.0.1:0"))
            .await
            .unwrap();
    }
    sync.accept_connections().await.unwrap();
    let address = sync.get_server_address().await.unwrap();
    (
        (*sync).clone(),
        if iroh {
            Address::iroh(address)
        } else {
            Address::http(address)
        },
    )
}

async fn write(database: &Database, key: &str, value: &str) -> ID {
    let txn = database.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set(key, value)
        .await
        .unwrap();
    txn.commit().await.unwrap()
}

async fn value(database: &Database, key: &str) -> String {
    database
        .new_transaction()
        .await
        .unwrap()
        .get_store::<DocStore>("data")
        .await
        .unwrap()
        .get(key)
        .await
        .unwrap()
        .as_text()
        .unwrap()
        .to_owned()
}

async fn populated(iroh: bool) {
    let (server, mut owner, owner_key) =
        crate::helpers::test_local_instance_with_user_and_key("paging-source", Some("owner")).await;
    let (client, mut reader, reader_key) =
        crate::helpers::test_local_instance_with_user_and_key("paging-destination", Some("reader"))
            .await;
    assert_ne!(server.id(), client.id());
    assert_ne!(owner_key, reader_key);
    let database = owner.create_database(Doc::new(), &owner_key).await.unwrap();
    let tree = database.root_id().clone();
    let txn = database.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let payload = "synthetic\"\\\n\t".repeat(6000);
    let mut ids = Vec::new();
    for n in 0..18 {
        ids.push(write(&database, &format!("k{n}"), &payload).await);
    }
    let mut bodies = Vec::new();
    for id in &ids {
        bodies.push(server.backend().get(id).await.unwrap());
    }
    assert!(serde_json::to_vec(&bodies).unwrap().len() > 1024 * 1024);
    drop(bodies);
    owner
        .track_database(tree.clone(), &owner_key, SyncSettings::enabled())
        .await
        .unwrap();
    let (source, source_address) = transport(&server, iroh).await;
    let (destination, destination_address) = transport(&client, iroh).await;
    let signing_key = reader.get_signing_key(&reader_key).unwrap();
    destination
        .sync_with_peer_for_bootstrap_with_key(
            &source_address,
            &tree,
            &signing_key,
            "reader",
            Permission::Write(10),
        )
        .await
        .unwrap();
    reader
        .track_database(tree.clone(), &reader_key, SyncSettings::enabled())
        .await
        .unwrap();
    let replica = Database::open(&client, &tree)
        .await
        .unwrap()
        .with_key(eidetica::database::DatabaseKey::global(signing_key));
    for n in 0..18 {
        assert_eq!(value(&replica, &format!("k{n}")).await, payload);
    }
    let engine = client.backend().local_engine().unwrap();
    for id in &ids {
        assert_eq!(
            engine.get_verification_status(id).await.unwrap(),
            VerificationStatus::Verified
        );
    }
    assert_eq!(
        database.snapshot().await.unwrap(),
        replica.snapshot().await.unwrap()
    );
    // Both directions are required, not an A-or-B conformance check.
    write(&database, "from-source", "new source value").await;
    destination
        .sync_with_peer(&source_address, Some(&tree))
        .await
        .unwrap();
    assert_eq!(value(&replica, "from-source").await, "new source value");
    write(&replica, "from-destination", "new destination value").await;
    source
        .sync_with_peer(&destination_address, Some(&tree))
        .await
        .unwrap();
    assert_eq!(
        value(&database, "from-destination").await,
        "new destination value"
    );
    assert_eq!(
        database.snapshot().await.unwrap(),
        replica.snapshot().await.unwrap()
    );
    source.stop_server().await.unwrap();
    destination.stop_server().await.unwrap();
}

#[tokio::test]
async fn peer_paging_http_populated_bootstrap_and_bidirectional_convergence() {
    populated(false).await;
}

#[tokio::test]
async fn peer_paging_iroh_populated_bootstrap_and_bidirectional_convergence() {
    populated(true).await;
}

#[tokio::test]
async fn peer_paging_disabling_sync_between_pages_aborts_without_end() {
    use eidetica::sync::{
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{RequestContext, SyncRequest, SyncTreeRequest},
        transports::framing::{Collector, Decoder, Frame},
    };
    use futures_util::StreamExt;
    use std::{sync::Arc, time::Duration};
    let (instance, mut owner, key) =
        crate::helpers::test_local_instance_with_user_and_key("page-policy", Some("owner")).await;
    let database = owner.create_database(Doc::new(), &key).await.unwrap();
    let tree = database.root_id().clone();
    let txn = database.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    for n in 0..18 {
        write(&database, &format!("k{n}"), &"x".repeat(80_000)).await;
    }
    owner
        .track_database(tree.clone(), &key, SyncSettings::enabled())
        .await
        .unwrap();
    instance.enable_sync().await.unwrap();
    let sync = instance.sync().unwrap();
    let handler = Arc::new(SyncHandlerImpl::new(
        instance.clone(),
        sync.sync_tree_root_id().clone(),
    ));
    let request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree.clone(),
        our_tips: eidetica::snapshot::Snapshot::EMPTY,
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: None,
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut collector = Collector::new(request.clone(), deadline).unwrap();
    let mut frames = handler
        .response_frames(request, RequestContext::default(), deadline)
        .await
        .unwrap();
    collector
        .push(frames.next().await.unwrap().unwrap())
        .unwrap();
    let page = frames.next().await.unwrap().unwrap();
    assert!(matches!(&page, Frame::Page { page, .. } if page.next.is_some()));
    collector.push(page).unwrap();
    owner
        .track_database(tree, &key, SyncSettings::disabled())
        .await
        .unwrap();
    assert!(frames.next().await.unwrap().is_err());
    assert!(frames.next().await.is_none());
    assert!(collector.finish(Decoder::default()).is_err());
}
