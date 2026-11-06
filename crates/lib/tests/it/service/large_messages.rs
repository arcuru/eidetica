//! Logical messages above the former frame limit, through ordinary service APIs.

use super::*;
use eidetica::{
    Database,
    crdt::Doc,
    entry::{Entry, ID},
    message_io::{TransferBudget, TransferPolicy},
    store::DocStore,
};

const MIB: usize = 1024 * 1024;

/// One more Mebibyte than the 64 MiB service frames could carry whole.
const OVER_FORMER_LIMIT: usize = 70 * MIB;

async fn start_server_with(
    budget: TransferBudget,
) -> (PathBuf, watch::Sender<()>, Instance, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = dir.path().join("test.sock");
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        eidetica::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let (tx, rx) = watch::channel(());
    let server = ServiceServer::bind_with_budget(instance.clone(), socket_path.clone(), budget)
        .await
        .unwrap();
    tokio::spawn(server.run(rx));
    (socket_path, tx, instance, dir)
}

fn limited(max_message_bytes: u64) -> TransferBudget {
    let mut policy = TransferPolicy::default();
    policy.max_message_bytes = max_message_bytes;
    TransferBudget::new(policy).unwrap()
}

/// A connected client with a database it owns, created over the wire.
async fn client_database(path: &std::path::Path, server: &Instance) -> (Instance, Database) {
    crate::helpers::create_account(server, "alice", None)
        .await
        .unwrap();
    let client = login_client(path, "alice").await;
    let mut user = client.login_account("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let database = user.create_database(Doc::new(), &key).await.unwrap();
    (client, database)
}

async fn write(database: &Database, key: &str, value: String) -> eidetica::Result<ID> {
    let tx = database.new_transaction().await?;
    let store = tx.get_store::<DocStore>("data").await?;
    store.set(key, value).await?;
    tx.commit().await
}

fn encoded_size(entry: &Entry) -> usize {
    entry.data("data").unwrap().len()
}

#[tokio::test]
async fn one_entry_over_the_former_limit_is_an_ordinary_write_and_read() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let (client, database) = client_database(&path, &server).await;

    let value = "x".repeat(OVER_FORMER_LIMIT);
    let id = write(&database, "large", value.clone()).await.unwrap();

    // The daemon accepted the same signed entry, and it verified there.
    let stored = server.backend().get(&id).await.unwrap();
    assert_eq!(stored.id(), id);
    assert!(encoded_size(&stored) > OVER_FORMER_LIMIT);

    // The download direction, into a client that holds nothing locally.
    let reader = login_client(&path, "alice").await;
    let read = reader.backend().get(&id).await.unwrap();
    assert_eq!(read, stored);

    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    assert_eq!(store.get_string("large").await.unwrap(), value);
    drop(client);
}

#[tokio::test]
async fn an_aggregate_response_over_the_former_limit_needs_no_paging() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let (client, database) = client_database(&path, &server).await;

    let first = write(&database, "a", "a".repeat(40 * MIB)).await.unwrap();
    let second = write(&database, "b", "b".repeat(40 * MIB)).await.unwrap();

    let conn = remote_conn(&client);
    let identity = conn.session_identity().unwrap();
    let tips = conn
        .get_verified_tips(database.root_id().clone(), identity.clone())
        .await
        .unwrap();
    let entries = conn
        .get_store_entries(
            database.root_id().clone(),
            identity,
            "data".to_string(),
            tips.into_tips(),
            ReadScope::Verified,
        )
        .await
        .unwrap();

    let ids = entries.iter().map(Entry::id).collect::<Vec<_>>();
    assert!(ids.contains(&first) && ids.contains(&second));
    let total: usize = entries
        .iter()
        .map(|entry| entry.data("data").map_or(0, |data| data.len()))
        .sum();
    assert!(total > 64 * MIB);
}

#[tokio::test]
async fn a_request_over_the_server_policy_is_refused_and_accepts_nothing() {
    let (path, _shutdown, server, _dir) = start_server_with(limited(8 * MIB as u64)).await;
    let (_client, database) = client_database(&path, &server).await;
    let before = server.backend().snapshot(database.root_id()).await.unwrap();

    // Larger than socket buffers, so the send is still running when refused.
    let refused = write(&database, "large", "x".repeat(32 * MIB)).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(eidetica::Error::is_resource_limit),
        "{refused:?}"
    );
    assert_eq!(
        server.backend().snapshot(database.root_id()).await.unwrap(),
        before
    );

    // Retired, not desynchronized: a fresh connection works as before.
    let fresh = login_client(&path, "alice").await;
    assert_eq!(fresh.id(), server.id());
}

#[tokio::test]
async fn a_response_over_the_server_policy_refuses_only_that_call() {
    let (path, _shutdown, server, _dir) = start_server_with(limited(2 * MIB as u64)).await;
    let (client, database) = client_database(&path, &server).await;

    // Stored through the daemon's own instance, so no wire limit applies yet.
    let user = server.login_account("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let local = Database::open(&server, database.root_id())
        .await
        .unwrap()
        .with_key(user.get_signing_key(&key).unwrap());
    let id = write(&local, "large", "x".repeat(3 * MIB)).await.unwrap();

    let refused = client.backend().get(&id).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(eidetica::Error::is_resource_limit),
        "{refused:?}"
    );
    // The response slot was answered, so this connection is still in step.
    assert_eq!(
        client.backend().get(database.root_id()).await.unwrap().id(),
        *database.root_id()
    );
}

#[tokio::test]
async fn many_concurrent_small_requests_are_never_refused_for_capacity() {
    let mut policy = TransferPolicy::default();
    policy.max_inbound_messages = 1;
    policy.max_outbound_messages = 1;
    let (path, _shutdown, server, _dir) =
        start_server_with(TransferBudget::new(policy).unwrap()).await;

    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let path = path.clone();
        calls.spawn(async move {
            let client = Instance::connect(format!("unix://{}", path.display())).await?;
            Ok::<_, eidetica::Error>(client.id())
        });
    }
    while let Some(call) = calls.join_next().await {
        assert_eq!(call.unwrap().unwrap(), server.id());
    }
}

/// A client that sends without reading stops being read itself: its answers wait
/// in the socket, not in a growing queue, and none is lost or reordered.
#[tokio::test]
async fn a_client_that_stops_reading_is_backpressured_and_stays_in_step() {
    const REQUESTS: usize = 40_000;
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let (mut reader, mut writer) = raw_handshake(&path).await;

    let sender = tokio::spawn(async move {
        for _ in 0..REQUESTS {
            write_frame(&mut writer, &ServiceRequest::GetInstanceMetadata)
                .await
                .unwrap();
        }
        writer
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !sender.is_finished(),
        "positive control: the socket and queue should have filled by now"
    );

    for _ in 0..REQUESTS {
        match read_response(&mut reader).await {
            ServiceResponse::InstanceMetadata(Some(metadata)) => {
                assert_eq!(metadata.id, server.id());
            }
            other => panic!("answer out of step: {other:?}"),
        }
    }
    sender.await.unwrap();
}
