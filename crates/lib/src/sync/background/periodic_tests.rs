//! Exercise the private periodic round against local engines and real HTTP
//! transports. No event loops or write callbacks run, so only an explicit
//! periodic round can move data or update liveness.

use super::*;
use crate::{
    NewUser,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    crdt::Doc,
    sync::{Sync, transports::http::HttpTransport},
    user::types::SyncSettings,
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
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner"))
                .await?;
        let sync = Sync::new(instance.clone()).await?;
        let (_, command_rx) = mpsc::channel(1);
        let mut engine = BackgroundSync {
            transport_manager: TransportManager::new(),
            instance: instance.downgrade(),
            sync_tree_id: sync.sync_tree_root_id().clone(),
            queue: Arc::clone(&sync.queue),
            peer_state: Arc::clone(&sync.peer_state),
            retry_queue: Vec::new(),
            command_rx,
        };
        engine.transport_manager.add(
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
        let mut owner = self.instance.login_user("owner", None).await?;
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
            .sync_user(owner.user_uuid(), owner.user_database().root_id())
            .await?;
        Ok(database)
    }

    async fn follow(&self, source: &Self, trees: &[&Database]) -> Result<()> {
        let peer = source.instance.id();
        self.sync.register_peer(&peer, None).await?;
        self.sync
            .add_peer_address(
                &peer,
                Address::http(source.engine.transport_manager.get_server_address("http")?),
            )
            .await?;
        for tree in trees {
            self.sync.add_tree_sync(&peer, tree.root_id()).await?;
        }
        Ok(())
    }
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
async fn periodic_round_walks_registered_trees_and_stamps_liveness() -> Result<()> {
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

    recipient.engine.periodic_sync_all_peers().await;
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
    recipient.engine.periodic_sync_all_peers().await;
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
async fn periodic_round_does_not_stamp_a_dead_peer_or_skip_a_healthy_peer() -> Result<()> {
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
    dead.engine.stop_server(None).await?;

    assert!(!recipient.instance.has_entry(&dead_entry).await);
    assert!(!recipient.instance.has_entry(&healthy_entry).await);
    recipient.engine.periodic_sync_all_peers().await;

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
