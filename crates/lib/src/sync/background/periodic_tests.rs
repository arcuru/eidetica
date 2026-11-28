//! Exercise the background round against local engines and real transports,
//! and the automatic event loop against a controllable in-process transport.

use super::*;
use crate::{
    NewAccount,
    account::types::SyncSettings,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    crdt::Doc,
    store::DocStore,
    sync::{Sync, transports::http::HttpTransport},
};

struct LocalPeer {
    instance: Instance,
    sync: Sync,
    engine: BackgroundSync,
}

impl LocalPeer {
    async fn new() -> Result<Self> {
        // Always local, including when the wider suite selects the service backend.
        let (instance, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("owner"))
                .await?;
        let sync = Sync::new(instance.clone()).await?;
        let (_, command_rx) = mpsc::channel(1);
        let mut engine = BackgroundSync {
            round: RoundContext {
                transport_manager: TransportManager::new(),
                instance: instance.downgrade(),
                sync_tree_id: sync.sync_tree_root_id().clone(),
                peer_state: Arc::clone(&sync.peer_state),
            },
            queue: Arc::clone(&sync.queue),
            retry_queue: Vec::new(),
            command_rx,
        };
        engine.round.transport_manager.add(
            "http",
            Arc::new(HttpTransport::builder().bind("127.0.0.1:0").build_sync()?),
        );
        Ok(Self {
            instance,
            sync,
            engine,
        })
    }

    async fn shared_tree(&self) -> Result<Database> {
        let mut owner = self.instance.login_account("owner", "").await?;
        let key = owner.get_default_key()?;
        let database = owner.create_database(Doc::new(), &key).await?;
        let tx = database.new_transaction().await?;
        tx.get_settings()?
            .set_global_auth_key(AuthKey::active(None, Permission::Read))
            .await?;
        tx.commit().await?;
        owner
            .track_database(database.root_id().clone(), &key, SyncSettings::enabled())
            .await?;
        self.sync
            .sync_account(owner.login_id(), owner.account().id())
            .await?;
        Ok(database)
    }

    async fn set_local_settings(&self, tree: &ID, settings: &SyncSettings) -> Result<()> {
        use crate::sync::account_sync_manager::AccountSyncManager;

        let txn = self
            .engine
            .round
            .get_sync_tree()
            .await?
            .new_transaction()
            .await?;
        AccountSyncManager::new(&txn)
            .set_combined_settings(tree, settings)
            .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn follow(&self, source: &Self, trees: &[&Database]) -> Result<()> {
        let address = Address::http(
            source
                .engine
                .round
                .transport_manager
                .get_server_address("http")?,
        );
        self.follow_at(&source.instance.id(), address, trees).await
    }

    async fn follow_at(
        &self,
        peer: &PublicKey,
        address: Address,
        trees: &[&Database],
    ) -> Result<()> {
        self.sync.register_peer(peer, None).await?;
        self.sync.add_peer_address(peer, address).await?;
        for tree in trees {
            self.sync.add_tree_sync(peer, tree.root_id()).await?;
        }
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn automatic_rounds_use_current_scope_without_waking_unrelated_peers() -> Result<()> {
    use crate::{
        sync::operation_tests::{Trace, operation_id},
        testing::{SimLoopback, SimNetwork, TestTransport},
    };

    let trace = Trace::default();
    let _capture = trace.capture();
    let transport = SimLoopback::new(SimNetwork::new());
    let (a, b, recipient) = (
        LocalPeer::new().await?,
        LocalPeer::new().await?,
        LocalPeer::new().await?,
    );
    let (a_id, b_id) = (a.instance.id(), b.instance.id());
    let fast = a.shared_tree().await?;
    let shared = a.shared_tree().await?;
    let disabled = a.shared_tree().await?;
    let absent = a.shared_tree().await?;
    let unrelated = b.shared_tree().await?;
    recipient
        .follow_at(
            &a_id,
            transport.serve(&a.sync).await?,
            &[&fast, &shared, &disabled, &absent],
        )
        .await?;
    recipient
        .follow_at(&b_id, transport.serve(&b.sync).await?, &[&unrelated])
        .await?;
    recipient
        .set_local_settings(fast.root_id(), &SyncSettings::enabled().with_interval(5))
        .await?;
    for db in [&shared, &disabled, &unrelated] {
        recipient
            .set_local_settings(db.root_id(), &SyncSettings::enabled().with_interval(3600))
            .await?;
    }
    transport.serve(&recipient.sync).await?;
    // Only the recipient initiates tree exchange; no remote pulls can restore
    // the relationship removed below. Stopping the server leaves outbound on.
    recipient.sync.stop_server().await?;

    tokio::time::sleep(Duration::from_secs(70)).await;
    for db in [&fast, &shared, &disabled, &unrelated] {
        assert!(
            recipient.instance.has_entry(db.root_id()).await,
            "startup is the positive control for every enabled route"
        );
    }
    assert!(!recipient.instance.has_entry(absent.root_id()).await);
    recipient
        .set_local_settings(
            disabled.root_id(),
            &SyncSettings::disabled().with_interval(10),
        )
        .await?;

    trace.clear();
    tokio::time::sleep(Duration::from_secs(60)).await;
    let events = trace.output();
    let peer_rounds = |id: &PublicKey| {
        events
            .lines()
            .filter(|line| {
                line.contains("Starting peer synchronization") && line.contains(&id.to_string())
            })
            .count()
    };
    let requests = |db: &Database| pull_count(&events, db);
    let fast_rounds = peer_rounds(&a_id);
    assert!(
        (9..=16).contains(&fast_rounds),
        "observed {fast_rounds} rounds to the short-interval peer"
    );
    assert_eq!(
        peer_rounds(&b_id),
        0,
        "the fast database must not dial the unrelated peer"
    );
    assert_eq!(
        requests(&disabled),
        0,
        "a disabled database must not ride along with an enabled one"
    );
    // Ignore a round straddling either edge of the observation window.
    let mut coalesced = 0;
    for round in events.lines().filter(|line| {
        line.contains("Starting peer synchronization") && line.contains(&a_id.to_string())
    }) {
        let id = operation_id(round);
        let round: Vec<_> = events
            .lines()
            .filter(|line| line.contains("operation_id=") && operation_id(line) == id)
            .collect();
        if round.iter().any(|line| line.contains("Sync route outcome")) {
            let round = round.join("\n");
            assert_eq!(pull_count(&round, &fast), 1);
            assert_eq!(
                pull_count(&round, &shared),
                1,
                "the slow database belongs to the same round"
            );
            coalesced += 1;
        }
    }
    assert!(coalesced >= 8);
    assert_eq!(
        requests(&absent),
        0,
        "no combined settings is not an opt-in"
    );

    recipient
        .sync
        .remove_tree_sync(&a_id, shared.root_id())
        .await?;
    trace.clear();
    tokio::time::sleep(Duration::from_secs(7)).await;
    let events = trace.output();
    assert!(pull_count(&events, &fast) > 0);
    assert_eq!(
        pull_count(&events, &shared),
        0,
        "registration revoked before the next round"
    );

    // Lengthening the remaining enabled interval may leave one pending fast
    // deadline. After it completes, the disabled 10s database must not wake A.
    recipient
        .set_local_settings(fast.root_id(), &SyncSettings::enabled().with_interval(3600))
        .await?;
    tokio::time::sleep(Duration::from_secs(7)).await;
    trace.clear();
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(
        pull_count(&trace.output(), &fast),
        0,
        "the disabled database cannot drive the peer's cadence"
    );
    Ok(())
}

fn pull_count(events: &str, db: &Database) -> usize {
    let id = db.root_id().to_string();
    events
        .lines()
        .filter(|line| line.contains("Sending sync tree request") && line.contains(&id))
        .count()
}

async fn write(database: &Database, message: &str) -> Result<ID> {
    let tx = database.new_transaction().await?;
    tx.get_store::<DocStore>("messages")
        .await?
        .set_string("message", message)
        .await?;
    tx.commit().await
}

#[tokio::test]
async fn explicit_engine_round_walks_registered_trees_and_stamps_liveness() -> Result<()> {
    let mut source = LocalPeer::new().await?;
    let recipient = LocalPeer::new().await?;
    let trees = [source.shared_tree().await?, source.shared_tree().await?];
    source.engine.start_server(None).await?;
    recipient.follow(&source, &[&trees[0], &trees[1]]).await?;
    let peer = source.instance.id();

    for tree in &trees {
        assert!(!recipient.instance.has_entry(tree.root_id()).await);
        assert_eq!(
            recipient
                .sync
                .get_sync_status(tree.root_id(), &peer)
                .await?
                .last_sync,
            None
        );
    }

    recipient
        .engine
        .round
        .sync_with_peer(&PeerId::from(&peer))
        .await?;
    for tree in &trees {
        assert!(
            recipient.instance.has_entry(tree.root_id()).await,
            "every registered tree must bootstrap"
        );
        assert!(
            recipient
                .sync
                .get_sync_status(tree.root_id(), &peer)
                .await?
                .last_sync
                .is_some()
        );
    }

    // With existing tips this round must discover and pull remote changes, not
    // merely repeat the initial bootstrap or deliver queued local writes.
    let entries = [
        write(&trees[0], "first tree").await?,
        write(&trees[1], "second tree").await?,
    ];
    for entry in &entries {
        assert!(!recipient.instance.has_entry(entry).await);
    }
    recipient
        .engine
        .round
        .sync_with_peer(&PeerId::from(&peer))
        .await?;
    for entry in &entries {
        assert!(
            recipient.instance.has_entry(entry).await,
            "the next round must pull each tree's new entry"
        );
    }
    source.engine.stop_server(None).await?;
    Ok(())
}

#[tokio::test]
async fn automatic_round_distinguishes_route_failure_from_success() -> Result<()> {
    let mut dead = LocalPeer::new().await?;
    let mut healthy = LocalPeer::new().await?;
    let recipient = LocalPeer::new().await?;
    let dead_tree = dead.shared_tree().await?;
    let healthy_tree = healthy.shared_tree().await?;
    let dead_entry = write(&dead_tree, "unreachable").await?;
    let healthy_entry = write(&healthy_tree, "reachable").await?;
    dead.engine.start_server(None).await?;
    healthy.engine.start_server(None).await?;
    recipient.follow(&dead, &[&dead_tree]).await?;
    recipient.follow(&healthy, &[&healthy_tree]).await?;
    for tree in [&dead_tree, &healthy_tree] {
        recipient
            .set_local_settings(tree.root_id(), &SyncSettings::enabled())
            .await?;
    }
    dead.engine.stop_server(None).await?;

    assert!(!recipient.instance.has_entry(&dead_entry).await);
    assert!(!recipient.instance.has_entry(&healthy_entry).await);
    let engine = &recipient.engine.round;
    let run = |peer: PeerId| async move {
        let mut report = RoundReport::default();
        let plan = engine.periodic_plan(&peer).await?;
        assert_eq!(plan.1.len(), 1);
        let (addresses, periods) = plan;
        let result = engine
            .reconcile(
                &peer,
                Some((addresses, periods.into_keys().collect())),
                &mut report,
            )
            .await;
        Ok::<_, Error>((result, report))
    };
    let (dead_round, healthy_round) = tokio::join!(
        run(PeerId::from(dead.instance.id())),
        run(PeerId::from(healthy.instance.id()))
    );
    let (result, dead_report) = dead_round?;
    assert!(result.is_err());
    assert!(dead_report.network_failure);
    let (result, healthy_report) = healthy_round?;
    result?;
    assert!(healthy_report.reached_peer);
    assert!(!healthy_report.network_failure);

    assert!(!recipient.instance.has_entry(&dead_entry).await);
    assert_eq!(
        recipient
            .sync
            .get_sync_status(dead_tree.root_id(), &dead.instance.id())
            .await?
            .last_sync,
        None
    );
    assert!(
        recipient.instance.has_entry(&healthy_entry).await,
        "a failed peer must not prevent another peer syncing"
    );
    assert!(
        recipient
            .sync
            .get_sync_status(healthy_tree.root_id(), &healthy.instance.id())
            .await?
            .last_sync
            .is_some()
    );
    healthy.engine.stop_server(None).await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn background_round_correlates_routes_and_reports_partial_tree_outcome() -> Result<()> {
    use crate::sync::operation_tests::{Trace, assert_fields, operation_id, summary};

    let trace = Trace::default();
    let _guard = trace.capture();
    let mut source = LocalPeer::new().await?;
    let recipient = LocalPeer::new().await?;
    let tree = source.shared_tree().await?;
    source.engine.start_server(None).await?;
    recipient.follow(&source, &[&tree]).await?;
    let peer = source.instance.id();
    recipient
        .sync
        .add_peer_address(&peer, Address::new("unsupported", "route"))
        .await?;
    recipient
        .sync
        .add_tree_sync(&peer, &ID::from_bytes("missing tree"))
        .await?;
    trace.clear();

    recipient
        .engine
        .round
        .sync_with_peer(&PeerId::from(peer.clone()))
        .await?;
    let output = trace.output();
    let aggregate = summary(&output);
    assert_fields(
        aggregate,
        &[
            "outcome=\"partial\"",
            "routes=2",
            "attempted=1",
            "unsupported=1",
            "unobserved=0",
            "trees_ok=1",
            "trees_failed=1",
            "last_tree_error=Some(",
        ],
    );
    let id = operation_id(aggregate);
    for message in [
        "Selected sync route",
        "Sending sync tree request",
        "Failed to sync tree",
        "Bootstrap completed successfully",
    ] {
        let event = output
            .lines()
            .find(|line| line.contains(message))
            .expect(message);
        assert_eq!(operation_id(event), id, "{event}");
    }
    // A round still returns Ok when none of its trees sync. Its diagnostic
    // outcome must describe those failures rather than the scheduling policy.
    recipient
        .sync
        .remove_tree_sync(&peer, tree.root_id())
        .await?;
    trace.clear();
    recipient
        .engine
        .round
        .sync_with_peer(&PeerId::from(peer))
        .await?;
    let output = trace.output();
    assert_fields(
        summary(&output),
        &["outcome=\"failed\"", "trees_ok=0", "trees_failed=1"],
    );
    source.engine.stop_server(None).await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn background_send_emits_one_route_outcome() -> Result<()> {
    use crate::sync::operation_tests::{Trace, assert_fields, summary};

    let trace = Trace::default();
    let _guard = trace.capture();
    let mut source = LocalPeer::new().await?;
    let recipient = LocalPeer::new().await?;
    source.engine.start_server(None).await?;
    recipient.follow(&source, &[]).await?;
    trace.clear();
    recipient
        .engine
        .send_to_peer(&PeerId::from(source.instance.id()), vec![])
        .await?;
    let output = trace.output();
    assert_fields(
        summary(&output),
        &[
            "kind=\"send_entries\"",
            "outcome=\"success\"",
            "routes=1",
            "attempted=1",
            "unobserved=0",
        ],
    );
    source.engine.stop_server(None).await?;
    Ok(())
}

struct RecordVerifiedPull {
    inner: SyncHandlerImpl,
    tips: Arc<std::sync::Mutex<Vec<crate::Snapshot>>>,
}

#[async_trait::async_trait]
impl crate::sync::handler::SyncHandler for RecordVerifiedPull {
    async fn handle_request(
        &self,
        request: &SyncRequest,
        context: &crate::sync::protocol::RequestContext,
    ) -> SyncResponse {
        if let SyncRequest::SyncTree(request) = request {
            self.tips.lock().unwrap().push(request.our_tips.clone());
        }
        self.inner.handle_request(request, context).await
    }
}

fn withheld(tree: &ID, tag: &[u8]) -> Entry {
    Entry::builder(tree.clone())
        .add_parent(ID::from_bytes("withheld"))
        .set_height(50)
        .set_subtree_data("test", tag)
        .build()
        .unwrap()
}

#[tokio::test]
async fn verified_sync_round_advertises_and_transfers_only_verified_state() -> Result<()> {
    use crate::backend::VerificationStatus;
    let mut source = LocalPeer::new().await?;
    let recipient = LocalPeer::new().await?;
    let tree = source.shared_tree().await?;
    let source_engine = source.instance.require_local_engine()?;
    let unverified = withheld(tree.root_id(), b"source pending");
    let failed = withheld(tree.root_id(), b"source rejected");
    source_engine.put(unverified.clone()).await?;
    source_engine.put(failed.clone()).await?;
    source_engine
        .update_verification_status(&failed.id(), VerificationStatus::Failed)
        .await?;
    let tips = Arc::new(std::sync::Mutex::new(Vec::new()));
    source
        .engine
        .round
        .transport_manager
        .start_server(
            "http",
            Arc::new(RecordVerifiedPull {
                inner: SyncHandlerImpl::new(
                    source.instance.clone(),
                    source.sync.sync_tree_root_id().clone(),
                ),
                tips: Arc::clone(&tips),
            }),
        )
        .await?;
    recipient.follow(&source, &[&tree]).await?;
    // Orphan raw tips must not turn a bootstrap request into an incremental one.
    let recipient_engine = recipient.instance.require_local_engine()?;
    let local_pending = withheld(tree.root_id(), b"recipient pending");
    recipient_engine.put(local_pending.clone()).await?;
    recipient
        .engine
        .round
        .sync_with_peer(&PeerId::from(source.instance.id()))
        .await?;
    assert_eq!(*tips.lock().unwrap(), vec![crate::Snapshot::EMPTY]);
    assert!(recipient.instance.has_entry(tree.root_id()).await);
    assert!(!recipient.instance.has_entry(&unverified.id()).await);
    assert!(!recipient.instance.has_entry(&failed.id()).await);
    let before = recipient_engine.verified_snapshot(tree.root_id()).await?;
    let next = write(&tree, "next Verified update").await?;
    recipient
        .engine
        .round
        .sync_with_peer(&PeerId::from(source.instance.id()))
        .await?;
    assert_eq!(*tips.lock().unwrap(), vec![crate::Snapshot::EMPTY, before]);
    assert!(recipient.instance.has_entry(&next).await);
    assert!(!source.instance.has_entry(&local_pending.id()).await);
    source.engine.stop_server(None).await?;
    Ok(())
}

#[tokio::test]
async fn verified_sync_queue_and_retry_reject_nonverified_payloads_at_actual_send() -> Result<()> {
    use crate::backend::VerificationStatus;
    let mut sender = LocalPeer::new().await?;
    let mut recipient = LocalPeer::new().await?;
    let tree = sender.shared_tree().await?;
    recipient.engine.start_server(None).await?;
    sender.follow(&recipient, &[&tree]).await?;
    let peer = PeerId::new(recipient.instance.id());
    let engine = sender.instance.require_local_engine()?;
    let root = engine.get(tree.root_id()).await?;
    sender.engine.send_to_peer(&peer, vec![root]).await?;
    assert!(
        recipient.instance.has_entry(tree.root_id()).await,
        "positive control: the route sends Verified entries"
    );
    let pending = withheld(tree.root_id(), b"never serve");
    engine.put(pending.clone()).await?;
    sender
        .sync
        .queue
        .enqueue(&peer, pending.id(), tree.root_id().clone());
    assert_eq!(sender.engine.process_queue().await, 1);
    assert_eq!(sender.engine.retry_queue.len(), 1);
    assert!(!recipient.instance.has_entry(&pending.id()).await);
    engine
        .update_verification_status(&pending.id(), VerificationStatus::Failed)
        .await?;
    assert_eq!(sender.engine.flush_retry_queue().await, 1);
    assert!(!recipient.instance.has_entry(&pending.id()).await);
    recipient.engine.stop_server(None).await?;
    Ok(())
}
