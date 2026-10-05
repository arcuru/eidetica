//! Delegated-database dependency acquisition and lifecycle tests.

use eidetica::{
    Database, Instance,
    auth::{
        AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds, SigKey,
        TreeReference, crypto::PrivateKey,
    },
    crdt::Doc,
    database::DatabaseKey,
    store::DocStore,
    sync::{Address, Sync, handler::SyncHandler},
    user::types::SyncSettings,
};

use super::helpers::{HttpTransportFactory, TransportFactory};
use crate::helpers::test_local_instance_with_user_and_key;

struct Fixture {
    server: Instance,
    server_user: eidetica::user::User,
    delegated_key: PrivateKey,
    parent: Database,
    parent_id: eidetica::ID,
    dependency_id: eidetica::ID,
}

async fn fixture() -> Fixture {
    let (server, mut server_user, server_key) =
        test_local_instance_with_user_and_key("server", Some("device")).await;

    let delegated_key = PrivateKey::generate();
    let delegated_pubkey = delegated_key.public_key();
    let dependency = Database::create(&server, delegated_key.clone(), Doc::new())
        .await
        .unwrap();
    let txn = dependency.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegate"), Permission::Admin(0)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let dependency_tips = dependency.snapshot().await.unwrap().into_tips();

    let parent = server_user
        .create_database(Doc::new(), &server_key)
        .await
        .unwrap();
    let txn = parent.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(0),
                min: None,
            },
            tree: TreeReference {
                root: dependency.root_id().clone(),
                tips: dependency_tips.clone(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let txn = parent.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let delegated_parent = Database::open(&server, parent.root_id())
        .await
        .unwrap()
        .with_key(DatabaseKey::with_identity(
            delegated_key.clone(),
            SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: dependency.root_id().clone(),
                    tips: dependency_tips,
                }],
                hint: KeyHint::from_pubkey(&delegated_pubkey),
            },
        ));
    let txn = delegated_parent.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("delegated", "verified")
        .await
        .unwrap();
    txn.commit().await.unwrap();

    server_user
        .track_database(
            parent.root_id().clone(),
            &server_key,
            SyncSettings::enabled(),
        )
        .await
        .unwrap();

    Fixture {
        server,
        server_user,
        delegated_key,
        parent_id: parent.root_id().clone(),
        dependency_id: dependency.root_id().clone(),
        parent,
    }
}

async fn start_server(fixture: &Fixture) -> (Sync, Address) {
    let sync = HttpTransportFactory
        .create_sync(fixture.server.clone())
        .await
        .unwrap();
    sync.accept_connections().await.unwrap();
    let address = Address::http(sync.get_server_address().await.unwrap());
    (sync, address)
}

#[tokio::test]
async fn sync_acquires_and_records_delegated_dependency() {
    let fixture = fixture().await;
    let (_server_sync, address) = start_server(&fixture).await;
    let client = crate::helpers::test_local_instance().await;
    let client_sync = HttpTransportFactory
        .create_sync(client.clone())
        .await
        .unwrap();

    client_sync
        .sync_with_peer(&address, Some(&fixture.parent_id))
        .await
        .unwrap();

    assert!(client.has_database(&fixture.dependency_id).await);
    assert_eq!(
        client_sync
            .database_dependencies(&fixture.parent_id)
            .await
            .unwrap(),
        vec![fixture.dependency_id.clone()]
    );
    assert_eq!(
        client_sync
            .get_peer_trees(&_server_sync.get_device_pubkey().unwrap())
            .await
            .unwrap()
            .iter()
            .filter(|id| **id == fixture.dependency_id)
            .count(),
        1,
        "same-peer acquisition must deduplicate the inherited relationship"
    );

    let parent = Database::open(&client, &fixture.parent_id).await.unwrap();
    assert_eq!(
        parent
            .get_store_viewer::<DocStore>("data")
            .await
            .unwrap()
            .get_string("delegated")
            .await
            .unwrap(),
        "verified",
        "the blocked parent must be retried after its dependency arrives"
    );
}

#[tokio::test]
async fn dependency_inherits_serving_policy() {
    let mut fixture = fixture().await;
    let (server_sync, address) = start_server(&fixture).await;
    let client = crate::helpers::test_local_instance().await;
    let client_sync = HttpTransportFactory
        .create_sync(client.clone())
        .await
        .unwrap();
    client_sync
        .sync_with_peer(&address, Some(&fixture.parent_id))
        .await
        .unwrap();

    let handler = super::helpers::create_test_sync_handler(&client_sync);
    let response = handler
        .handle_request(
            &eidetica::sync::protocol::SyncRequest::SyncTree(
                eidetica::sync::protocol::SyncTreeRequest {
                    tree_id: fixture.dependency_id.clone(),
                    our_tips: Default::default(),
                    peer_pubkey: None,
                    requesting_key: None,
                    requesting_key_name: None,
                    requested_permission: None,
                    metadata: None,
                    auth: None,
                    dependency_path: vec![fixture.parent_id.clone()],
                },
            ),
            &Default::default(),
        )
        .await;
    assert!(
        !matches!(response, eidetica::sync::protocol::SyncResponse::Error(ref e) if e.contains("Tree not found")),
        "a dependency replica must be serveable under inherited sync policy"
    );

    assert_eq!(fixture.parent.root_id(), &fixture.parent_id);
    assert!(
        server_sync
            .database_dependencies(&fixture.parent_id)
            .await
            .unwrap()
            .is_empty()
    );

    let delegated_pubkey = fixture
        .server_user
        .add_private_key(Some("direct dependency"))
        .await
        .unwrap();
    let dependency = Database::open(&fixture.server, &fixture.dependency_id)
        .await
        .unwrap()
        .with_key(fixture.delegated_key.clone());
    let txn = dependency.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("direct dependency"), Permission::Read),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    fixture
        .server_user
        .track_database(
            fixture.dependency_id.clone(),
            &delegated_pubkey,
            SyncSettings::on_commit(),
        )
        .await
        .unwrap();
    assert!(
        fixture
            .server_user
            .database(&fixture.dependency_id)
            .await
            .unwrap()
            .sync_settings
            .sync_on_commit,
        "direct tracking must promote the same replica to explicit policy"
    );
}
