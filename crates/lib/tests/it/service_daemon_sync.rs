//! End-to-end coverage for sync owned by a service daemon.

#![cfg(all(unix, feature = "service"))]

use std::{path::Path, time::Duration};

use eidetica::{
    Instance, NewAccount, Result,
    account::types::SyncSettings,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    crdt::Doc,
    service::ServiceServer,
    store::DocStore,
    sync::{peer_types::Address, transports::http::HttpTransport},
};
use tokio::{sync::watch, task::JoinHandle};

async fn start_service(
    instance: Instance,
    socket: &Path,
) -> (watch::Sender<()>, JoinHandle<Result<()>>) {
    let (tx, rx) = watch::channel(());
    let server = ServiceServer::bind(instance, socket).await.unwrap();
    let handle = tokio::spawn(async move { server.run(rx).await });
    (tx, handle)
}

async fn stop_service(
    tx: watch::Sender<()>,
    handle: JoinHandle<Result<()>>,
    socket: &Path,
) -> Result<()> {
    drop(tx);
    handle.await.expect("service task panicked")?;
    assert!(!socket.exists(), "service socket survived server shutdown");
    Ok(())
}

#[tokio::test]
async fn service_client_database_syncs_and_restart_preserves_callback_delivery() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let snapshot = dir.path().join("daemon.json");
    let socket = dir.path().join("daemon.sock");
    let daemon_url = format!("memory://{}", snapshot.display());

    let (daemon, _) =
        Instance::connect_or_create(&daemon_url, NewAccount::passwordless("alice")).await?;
    daemon.enable_sync().await?;
    let daemon_sync = daemon.sync().unwrap();
    daemon_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    daemon_sync.accept_connections().await?;
    let daemon_addr = Address::http(daemon_sync.get_server_address_for("http").await?);
    let (service_shutdown, service_handle) = start_service(daemon.clone(), &socket).await;

    let (peer, mut peer_user) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("bob"))
            .await?;
    peer.enable_sync().await?;
    let peer_sync = peer.sync().unwrap();
    peer_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    peer_sync.accept_connections().await?;
    let peer_addr = Address::http(peer_sync.get_server_address_for("http").await?);

    let service = Instance::connect(format!("unix://{}", socket.display())).await?;
    let mut client = service.login_account("alice", None).await?;
    let key = client.get_default_key()?;
    let mut settings = Doc::new();
    settings.set("name", "created-by-service-client");
    let db = client.create_database(settings, &key).await?;
    let tree = db.root_id().clone();
    let tx = db.new_transaction().await?;
    tx.get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Admin(0)))
        .await?;
    tx.commit().await?;

    client
        .track_database(tree.clone(), &key, SyncSettings::on_commit())
        .await?;
    peer_sync.sync_with_peer(&daemon_addr, Some(&tree)).await?;
    peer_user
        .track_database(
            tree.clone(),
            &peer_user.get_default_key()?,
            SyncSettings::on_commit(),
        )
        .await?;
    let peer_db = peer_user.open_database(&tree).await?;
    daemon_sync.add_tree_sync(&peer.id(), &tree).await?;
    peer_sync.add_tree_sync(&daemon.id(), &tree).await?;
    peer_sync
        .add_peer_address(&daemon.id(), daemon_addr.clone())
        .await?;
    daemon_sync
        .add_peer_address(&peer.id(), peer_addr.clone())
        .await?;

    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("data")
            .await?
            .set_string("before_restart", "one")
            .await
    })
    .await?;
    drop(service);
    drop(client);
    drop(db);
    daemon.flush_sync().await?;
    assert_eq!(
        peer_db
            .get_store_viewer::<DocStore>("data")
            .await?
            .get_string("before_restart")
            .await?,
        "one"
    );

    daemon.flush()?;
    stop_service(service_shutdown, service_handle, &socket).await?;
    drop(daemon_sync);
    drop(daemon);

    let restarted = Instance::connect(&daemon_url).await?;
    restarted.enable_sync().await?;
    let restarted_sync = restarted.sync().unwrap();
    restarted_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    restarted_sync.accept_connections().await?;
    let restarted_addr = Address::http(restarted_sync.get_server_address_for("http").await?);
    restarted_sync
        .add_peer_address(&peer.id(), peer_addr)
        .await?;
    peer_sync
        .add_peer_address(&restarted.id(), restarted_addr)
        .await?;
    let (service_shutdown, service_handle) = start_service(restarted.clone(), &socket).await;

    let service = Instance::connect(format!("unix://{}", socket.display())).await?;
    let client = service.login_account("alice", None).await?;
    let client_db = client.open_database(&tree).await?;
    let (fired_tx, mut fired_rx) = tokio::sync::mpsc::unbounded_channel();
    let _callback = client_db
        .on_write(move |event, _db| {
            let source = event.source();
            let tx = fired_tx.clone();
            async move {
                let _ = tx.send(source);
                Ok(())
            }
        })
        .await?;

    peer_db
        .with_transaction(|tx| async move {
            tx.get_store::<DocStore>("data")
                .await?
                .set_string("after_restart", "two")
                .await
        })
        .await?;
    peer.flush_sync().await?;

    let source = tokio::time::timeout(Duration::from_secs(2), fired_rx.recv())
        .await
        .expect("reconnected client did not receive the synced write callback")
        .expect("callback channel closed");
    assert_eq!(source, eidetica::instance::WriteSource::Remote);
    assert_eq!(
        client_db
            .get_store_viewer::<DocStore>("data")
            .await?
            .get_string("after_restart")
            .await?,
        "two"
    );

    stop_service(service_shutdown, service_handle, &socket).await?;
    restarted_sync.stop_server().await?;
    peer_sync.stop_server().await?;
    Ok(())
}
