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
        .with_key(eidetica::database::DatabaseKey::global(signing_key.clone()));
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
    // The local writer initiates the next exchange, so its >1 MiB branch
    // must travel via the reverse-history producer, not the peer's pull path.
    let mut reverse_ids = Vec::new();
    for n in 0..18 {
        reverse_ids.push(write(&replica, &format!("reverse-{n}"), &payload).await);
    }
    assert!(!server.has_entry(reverse_ids.last().unwrap()).await);
    destination
        .sync_tree_with_peer_as(&server.id(), &tree, Some(&signing_key))
        .await
        .unwrap();
    for n in 0..18 {
        assert_eq!(value(&database, &format!("reverse-{n}")).await, payload);
    }
    for id in &reverse_ids {
        assert_eq!(
            server
                .backend()
                .local_engine()
                .unwrap()
                .get_verification_status(id)
                .await
                .unwrap(),
            VerificationStatus::Verified
        );
    }
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

async fn policy_change_between_pages(revoke_read: bool) {
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
    let (reader, reader_pub) = eidetica::auth::crypto::generate_keypair();
    let txn = database.new_transaction().await.unwrap();
    if revoke_read {
        txn.get_settings()
            .unwrap()
            .set_auth_key(
                &reader_pub,
                AuthKey::active(Some("reader"), Permission::Read),
            )
            .await
            .unwrap();
    } else {
        txn.get_settings()
            .unwrap()
            .set_global_auth_key(AuthKey::active(None, Permission::Read))
            .await
            .unwrap();
    }
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
    let known = if revoke_read {
        eidetica::Snapshot::from([tree.clone()])
    } else {
        eidetica::Snapshot::EMPTY
    };
    let auth = revoke_read.then(|| {
        eidetica::sync::protocol::SyncRequestAuth::sign(
            &reader,
            &instance.id(),
            &tree,
            &known,
            eidetica::Clock::now_millis(&eidetica::FixedClock::default()),
        )
    });
    let request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree.clone(),
        our_tips: known,
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth,
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut collector = Collector::new(request.clone(), deadline).unwrap();
    let mut frames = handler
        .clone()
        .response_frames(request.clone(), RequestContext::default(), deadline)
        .await
        .unwrap();
    collector
        .push(frames.next().await.unwrap().unwrap())
        .unwrap();
    let page = frames.next().await.unwrap().unwrap();
    assert!(matches!(&page, Frame::Page { page, .. } if page.next.is_some()));
    collector.push(page).unwrap();
    if revoke_read {
        assert!(
            handler
                .response_frames(request, RequestContext::default(), deadline)
                .await
                .is_err(),
            "initial nonce cannot authorize a second exchange"
        );
        let txn = database.new_transaction().await.unwrap();
        txn.get_settings()
            .unwrap()
            .revoke_auth_key(&reader_pub)
            .await
            .unwrap();
        txn.commit().await.unwrap();
    } else {
        owner
            .track_database(tree, &key, SyncSettings::disabled())
            .await
            .unwrap();
    }
    assert!(frames.next().await.unwrap().is_err());
    assert!(frames.next().await.is_none());
    assert!(collector.finish(Decoder::default()).is_err());
}

#[tokio::test]
async fn peer_paging_disabling_sync_between_pages_aborts_without_end() {
    policy_change_between_pages(false).await;
}

#[tokio::test]
async fn peer_paging_revoked_read_and_replayed_nonce_cannot_complete() {
    policy_change_between_pages(true).await;
}

struct StallAfterPage {
    inner: std::sync::Arc<eidetica::sync::handler::SyncHandlerImpl>,
    ready: std::sync::Arc<tokio::sync::Notify>,
    dropped: std::sync::Arc<tokio::sync::Notify>,
    stall: std::sync::atomic::AtomicBool,
}

struct ProducerDrop(std::sync::Arc<tokio::sync::Notify>);
impl Drop for ProducerDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[async_trait::async_trait]
impl eidetica::sync::handler::SyncHandler for StallAfterPage {
    async fn handle_request(
        &self,
        request: &eidetica::sync::protocol::SyncRequest,
        context: &eidetica::sync::protocol::RequestContext,
    ) -> eidetica::sync::protocol::SyncResponse {
        self.inner.handle_request(request, context).await
    }
    async fn response_frames(
        self: std::sync::Arc<Self>,
        request: eidetica::sync::protocol::SyncRequest,
        context: eidetica::sync::protocol::RequestContext,
        deadline: tokio::time::Instant,
    ) -> eidetica::Result<eidetica::sync::transports::framing::FrameStream> {
        use futures_util::StreamExt;
        let stall = matches!(request, eidetica::sync::protocol::SyncRequest::SyncTree(_))
            && self.stall.load(std::sync::atomic::Ordering::SeqCst);
        let frames = self
            .inner
            .clone()
            .response_frames(request, context, deadline)
            .await?;
        if !stall {
            return Ok(frames);
        }
        let ready = self.ready.clone();
        let guard = ProducerDrop(self.dropped.clone());
        Ok(Box::pin(futures_util::stream::unfold(
            (frames, 0, guard),
            move |(mut frames, n, guard)| {
                let ready = ready.clone();
                async move {
                    if n == 2 {
                        ready.notify_one();
                        std::future::pending::<()>().await;
                    }
                    frames
                        .next()
                        .await
                        .map(|frame| (frame, (frames, n + 1, guard)))
                }
            },
        )))
    }
}

async fn cancelled_native_pull(iroh: bool) {
    use eidetica::sync::{handler::SyncHandlerImpl, transports::SyncTransport};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    let (source, mut owner, key) =
        crate::helpers::test_local_instance_with_user_and_key("cancel-source", Some("owner")).await;
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
        write(&database, &format!("cancel-{n}"), &"x".repeat(80_000)).await;
    }
    owner
        .track_database(tree.clone(), &key, SyncSettings::enabled())
        .await
        .unwrap();
    source.enable_sync().await.unwrap();
    let ready = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(tokio::sync::Notify::new());
    let handler = Arc::new(StallAfterPage {
        inner: Arc::new(SyncHandlerImpl::new(
            source.clone(),
            source.sync().unwrap().sync_tree_root_id().clone(),
        )),
        ready: ready.clone(),
        dropped: dropped.clone(),
        stall: AtomicBool::new(true),
    });
    let server: Box<dyn SyncTransport> = if iroh {
        Box::new(
            IrohTransport::builder()
                .relay_mode(RelayMode::Disabled)
                .mdns(false)
                .n0_dns(false)
                .portmapper_config(PortmapperConfig::Disabled)
                .build()
                .unwrap(),
        )
    } else {
        Box::new(
            HttpTransport::builder()
                .bind("127.0.0.1:0")
                .build_sync()
                .unwrap(),
        )
    };
    server.start_server(handler.clone()).await.unwrap();
    let address = Address::new(
        if iroh { "iroh" } else { "http" },
        server.get_server_address().unwrap(),
    );
    let (client, _, _) =
        crate::helpers::test_local_instance_with_user_and_key("cancel-destination", Some("reader"))
            .await;
    let (destination, _) = transport(&client, iroh).await;
    let mut pull = Box::pin(destination.sync_with_peer(&address, Some(&tree)));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! { _ = ready.notified() => {}, result = &mut pull => panic!("stalled pull returned: {result:?}") }
    }).await.expect("real source must produce a page");
    assert!(
        !client.has_entry(&tree).await,
        "an incomplete page sequence was ingested"
    );
    drop(pull); // Closes the command's response receiver; cancels native transport.
    tokio::time::timeout(Duration::from_secs(5), dropped.notified())
        .await
        .expect("cancelled native body must release its producer and permit promptly");
    assert!(!client.has_entry(&tree).await);
    assert!(
        destination
            .get_sync_status(&tree, &source.id())
            .await
            .unwrap()
            .last_sync
            .is_none()
    );
    handler.stall.store(false, Ordering::SeqCst);
    destination
        .sync_with_peer(&address, Some(&tree))
        .await
        .unwrap();
    assert_eq!(
        database.snapshot().await.unwrap(),
        client.backend().snapshot(&tree).await.unwrap()
    );
    assert!(client.has_entry(&tree).await);
    server.stop_server().await.unwrap();
    destination.stop_server().await.unwrap();
}

#[tokio::test]
async fn peer_paging_http_cancelled_pull_discards_prefix_and_releases_producer() {
    cancelled_native_pull(false).await;
}
#[tokio::test]
async fn peer_paging_iroh_cancelled_pull_discards_prefix_and_releases_producer() {
    cancelled_native_pull(true).await;
}

async fn missing_delegated_history(iroh: bool) {
    use eidetica::{
        auth::{
            DelegatedTreeRef, DelegationStep, KeyHint, PermissionBounds, SigKey, TreeReference,
        },
        database::DatabaseKey,
    };
    let (source, mut owner, key) =
        crate::helpers::test_local_instance_with_user_and_key("delegation-source", Some("owner"))
            .await;
    let delegate_key = owner.add_private_key(Some("delegate")).await.unwrap();
    let delegate = owner.get_signing_key(&delegate_key).unwrap();
    let identity = owner
        .create_database(Doc::new(), &delegate_key)
        .await
        .unwrap();
    let target = owner.create_database(Doc::new(), &key).await.unwrap();
    let tx = identity.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let identity_tips = identity.snapshot().await.unwrap().into_tips();
    let tx = target.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    tx.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: None,
            },
            tree: TreeReference {
                root: identity.root_id().clone(),
                tips: identity_tips.clone(),
            },
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let delegated = target.clone().with_key(DatabaseKey::with_identity(
        delegate.clone(),
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: identity.root_id().clone(),
                tips: identity_tips,
            }],
            hint: KeyHint::from_pubkey(&delegate.public_key()),
        },
    ));
    let payload = "delegated synthetic".repeat(5_000);
    let mut ids = Vec::new();
    for n in 0..18 {
        ids.push(write(&delegated, &format!("d{n}"), &payload).await);
    }
    owner
        .track_database(target.root_id().clone(), &key, SyncSettings::enabled())
        .await
        .unwrap();
    owner
        .track_database(
            identity.root_id().clone(),
            &delegate_key,
            SyncSettings::enabled(),
        )
        .await
        .unwrap();
    let (server, address) = transport(&source, iroh).await;
    let (client, _, _) = crate::helpers::test_local_instance_with_user_and_key(
        "delegation-destination",
        Some("reader"),
    )
    .await;
    let (receiver, _) = transport(&client, iroh).await;
    for _ in 0..2 {
        assert!(
            receiver
                .sync_with_peer(&address, Some(target.root_id()))
                .await
                .is_err(),
            "a framed End/raw known tip cannot complete missing delegated verification"
        );
        assert!(
            !client.has_entry(identity.root_id()).await,
            "a target read must not export a delegated Database implicitly"
        );
        assert_eq!(
            client
                .backend()
                .local_engine()
                .unwrap()
                .get_verification_status(ids.last().unwrap())
                .await
                .unwrap(),
            VerificationStatus::Unverified
        );
    }
    let partial = Database::open(&client, target.root_id()).await.unwrap();
    assert!(
        partial
            .new_transaction()
            .await
            .unwrap()
            .get_store::<DocStore>("data")
            .await
            .unwrap()
            .get("d17")
            .await
            .is_err(),
        "unverified delegated payload must not enter the default view"
    );
    // A distinct explicitly authorized identity request can supply the missing
    // proof; retry of the same target then completes without a new trust rule.
    receiver
        .sync_with_peer(&address, Some(identity.root_id()))
        .await
        .unwrap();
    receiver
        .sync_with_peer(&address, Some(target.root_id()))
        .await
        .unwrap();
    for n in 0..18 {
        assert_eq!(value(&partial, &format!("d{n}")).await, payload);
    }
    for id in ids {
        assert_eq!(
            client
                .backend()
                .local_engine()
                .unwrap()
                .get_verification_status(&id)
                .await
                .unwrap(),
            VerificationStatus::Verified
        );
    }
    server.stop_server().await.unwrap();
    receiver.stop_server().await.unwrap();
}

#[tokio::test]
async fn peer_paging_http_missing_delegation_stays_unverified_until_separate_transfer() {
    missing_delegated_history(false).await;
}
#[tokio::test]
async fn peer_paging_iroh_missing_delegation_stays_unverified_until_separate_transfer() {
    missing_delegated_history(true).await;
}
