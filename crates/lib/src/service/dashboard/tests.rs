use super::*;
use crate::{
    Instance, NewAccount,
    auth::{AuthKey, SigKey, crypto::generate_keypair},
    backend::{VerificationStatus, database::InMemory},
    crdt::Doc,
    service::ServiceServer,
    store::DocStore,
    sync::{RequestStatus, transports::http::HttpTransport},
};
use std::os::unix::fs::PermissionsExt;
use tokio::sync::watch;

async fn local(name: &str) -> (Instance, crate::account::AccountSession) {
    let (instance, account) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless(name))
            .await
            .unwrap();
    instance.enable_sync().await.unwrap();
    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync.accept_connections().await.unwrap();
    (instance, account)
}

#[tokio::test]
async fn dashboard_join_and_approval_remain_proof_and_target_bound() {
    let (host, mut admin) = local("owner").await;
    let (peer, mut guest) = local("guest").await;
    let host_sync = host.sync().unwrap();
    let key = admin.add_private_key(Some("target Admin")).await.unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&host.id(), AuthKey::active(None, Permission::Admin(0)))
        .await
        .unwrap();
    tx.get_store::<DocStore>("payload")
        .await
        .unwrap()
        .set_string("hello", "authorized body")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    db.share().await.unwrap();
    let ticket = db.ticket().await.unwrap();
    let outsider = NewAccount::passwordless("outsider");
    admin
        .admin()
        .await
        .unwrap()
        .create_account(outsider)
        .await
        .unwrap();

    let sockets = tempfile::tempdir().unwrap();
    std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (stop, rx) = watch::channel(());
    let host_socket = sockets.path().join("host.sock");
    let peer_socket = sockets.path().join("peer.sock");
    let native_socket = sockets.path().join("native.sock");
    let host_server = ServiceServer::bind_dashboard(host.clone(), &host_socket)
        .await
        .unwrap();
    let peer_server = ServiceServer::bind_dashboard(peer.clone(), &peer_socket)
        .await
        .unwrap();
    let native_server = ServiceServer::bind(host.clone(), &native_socket)
        .await
        .unwrap();
    let tasks = [
        tokio::spawn(host_server.run(rx.clone())),
        tokio::spawn(peer_server.run(rx.clone())),
        tokio::spawn(native_server.run(rx)),
    ];

    let pre_auth = Connection::connect(&host_socket).await.unwrap();
    pre_auth.ping().await.unwrap();
    assert!(pre_auth.request(Request::Databases).await.is_err());
    for request in [
        ServiceRequest::GetInstanceMetadata,
        ServiceRequest::ListLocalAccounts,
        ServiceRequest::SignerChallenge {
            pubkey: key.clone(),
        },
    ] {
        assert!(
            matches!(pre_auth.0.request_ok(request).await, Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::Unsupported)
        );
    }
    let native = RemoteConnection::connect(&native_socket).await.unwrap();
    assert!(
        matches!(native.request_ok(ServiceRequest::Dashboard(Box::new(Request::Ping))).await, Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::Unsupported)
    );

    let owner = pre_auth.login("owner", "").await.unwrap();
    let outsider = Connection::connect(&host_socket)
        .await
        .unwrap()
        .login("outsider", "")
        .await
        .unwrap();
    let guest_login = Connection::connect(&peer_socket)
        .await
        .unwrap()
        .login("guest", "")
        .await
        .unwrap();
    let guest_key = guest.get_default_key().unwrap();
    let root = db.root_id();
    assert!(matches!(
        guest_login
            .join(ticket.clone(), Permission::Read)
            .await
            .unwrap(),
        JoinOutcome::Pending(_)
    ));
    assert!(guest.tracked_database(root).await.unwrap().is_none());
    let requests = owner.access_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let id = &requests[0].id;
    assert!(outsider.access_requests().await.unwrap().is_empty());
    assert!(
        outsider
            .decide(root.clone(), id.clone(), true)
            .await
            .is_err()
    );
    assert_eq!(owner.access_requests().await.unwrap().len(), 1);
    assert!(
        owner
            .decide(ID::from_bytes(b"foreign"), id.clone(), false)
            .await
            .is_err()
    );

    // Prepare a real grant; a foreign connection cannot use its token or act
    // as the prepared signer. A stale local preparation cannot restore it.
    let reply = owner
        .connection
        .request(Request::Decide {
            root: root.clone(),
            id: id.clone(),
            reject: false,
        })
        .await
        .unwrap();
    let Reply::Grant {
        token,
        key: signing_id,
        draft,
    } = reply
    else {
        panic!("expected grant draft");
    };
    assert_eq!(signing_id, key);
    let signed = draft
        .sign(&owner.unlocked.signer(&signing_id).unwrap())
        .unwrap();
    assert!(
        outsider
            .connection
            .ok(Request::SubmitGrant {
                token: token.clone(),
                signed: Box::new(signed.clone())
            })
            .await
            .is_err()
    );
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("payload")
        .await
        .unwrap()
        .set_string("hello", "advanced frontier")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        owner
            .connection
            .ok(Request::SubmitGrant {
                token,
                signed: Box::new(signed)
            })
            .await
            .is_err()
    );
    assert!(
        !crate::Database::can_access(&host, root, &guest_key, &Permission::Read)
            .await
            .unwrap()
    );
    owner.decide(root.clone(), id.clone(), false).await.unwrap();
    assert!(
        crate::Database::can_access(&host, root, &guest_key, &Permission::Read)
            .await
            .unwrap()
    );
    assert!(
        !crate::Database::can_access(&host, root, &guest_key, &Permission::Write(10))
            .await
            .unwrap()
    );
    assert!(owner.access_requests().await.unwrap().is_empty());
    assert!(matches!(
        guest_login
            .join(ticket.clone(), Permission::Read)
            .await
            .unwrap(),
        JoinOutcome::Complete
    ));
    // A previously unlocked native AccountSession caches key metadata. A
    // newly opened session must see the dashboard's persisted mapping.
    guest = peer.login_account("guest", "").await.unwrap();
    let tracked = guest.database(root).await.unwrap();
    assert_eq!(tracked.key_id, guest_key);
    assert_eq!(
        guest
            .open_database(root)
            .await
            .unwrap()
            .current_permission()
            .await
            .unwrap(),
        Permission::Read
    );

    // A second target is unheld on the requester. Rejection must be
    // terminal without creating either a grant or a local tracking row.
    let rejected_db = admin.create_database(Doc::new(), &key).await.unwrap();
    rejected_db.share().await.unwrap();
    let rejected_root = rejected_db.root_id();
    let rejected_ticket = rejected_db.ticket().await.unwrap();
    assert!(matches!(
        guest_login
            .join(rejected_ticket.clone(), Permission::Read)
            .await
            .unwrap(),
        JoinOutcome::Pending(_)
    ));
    let rows = owner.access_requests().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        owner
            .decide(root.clone(), rows[0].id.clone(), true)
            .await
            .is_err()
    );
    owner
        .decide(rejected_root.clone(), rows[0].id.clone(), true)
        .await
        .unwrap();
    assert!(
        !crate::Database::can_access(&host, rejected_root, &guest_key, &Permission::Read)
            .await
            .unwrap()
    );
    assert!(
        host_sync
            .pending_bootstrap_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        host_sync
            .get_bootstrap_request(&rows[0].id)
            .await
            .unwrap()
            .unwrap()
            .1
            .status,
        RequestStatus::Rejected { .. }
    ));
    assert!(matches!(
        guest_login
            .join(rejected_ticket, Permission::Read)
            .await
            .unwrap(),
        JoinOutcome::Rejected(_)
    ));
    assert!(
        guest
            .tracked_database(rejected_root)
            .await
            .unwrap()
            .is_none()
    );

    // No reauthentication in place, even after using a signed write.
    assert!(
        owner
            .connection
            .0
            .trusted_login("outsider", "")
            .await
            .is_err()
    );
    assert!(owner.database(root.clone()).await.unwrap().is_some());
    owner.retire();
    owner.disconnected().await;
    assert!(owner.databases().await.is_err());
    assert!(guest_login.database(root.clone()).await.unwrap().is_some());
    drop(stop);
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    host_sync.stop_server().await.unwrap();
    peer.sync().unwrap().stop_server().await.unwrap();
}

#[tokio::test]
async fn verified_projection_counts_without_returning_unverified_or_extra_ids() {
    let (instance, mut user) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::passwordless("sample"),
    )
    .await
    .unwrap();
    let key = user.get_default_key().unwrap();
    let db = user.create_database(Doc::new(), &key).await.unwrap();
    let other = user.create_database(Doc::new(), &key).await.unwrap();
    let root = db.root_id();
    for height in 1..=102 {
        let (private, public) = generate_keypair();
        let entry = crate::Entry::builder(root.clone())
            .add_parent(root.clone())
            .set_height(height)
            .set_subtree_data("fixture", vec![0])
            .set_auth(
                crate::auth::AuthInfo::builder()
                    .key(SigKey::from_pubkey(&public))
                    .build(),
            )
            .build()
            .unwrap()
            .sign(&private)
            .unwrap();
        let id = entry.id();
        let backend = instance.require_local_engine().unwrap();
        // Backend projection fixture uses storage labels directly; none of
        // these synthetic Entries are published or expected to pass Auth.
        backend.put(entry).await.unwrap();
        if height > 1 {
            backend
                .update_verification_status(&id, VerificationStatus::Verified)
                .await
                .unwrap();
        }
    }
    let backend = instance.require_local_engine().unwrap();
    let (count, sample) = backend.verified_entry_summary(root, 100).await.unwrap();
    assert_eq!(count, 102);
    assert_eq!(sample.len(), 100);
    assert_eq!(sample.first(), Some(root));
    for id in sample {
        assert!(backend.get(&id).await.unwrap().in_tree(root));
        assert_eq!(
            backend.get_verification_status(&id).await.unwrap(),
            VerificationStatus::Verified
        );
    }
    assert_eq!(
        backend
            .verified_entry_summary(other.root_id(), 0)
            .await
            .unwrap(),
        (1, vec![])
    );
}
