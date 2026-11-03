//! Sync selects local Verified state, independently of raw/opt-in application views.

use std::collections::HashSet;

use eidetica::{
    Entry, Result, Snapshot,
    backend::VerificationStatus,
    entry::ID,
    store::DocStore,
    sync::{
        handler::SyncHandler,
        protocol::{RequestContext, SyncRequest, SyncResponse, SyncTreeRequest},
        utils::collect_ancestors_to_send,
    },
};

use super::helpers;
use crate::helpers::TestVerify;

fn child(tree: &ID, parent: &ID, tag: &str) -> Entry {
    Entry::builder(tree.clone())
        .add_parent(parent.clone())
        .set_height(1)
        .set_subtree_data("test", tag.as_bytes())
        .build()
        .unwrap()
}

#[tokio::test]
async fn verified_sync_enumeration_excludes_unverified_and_failed_seeds() -> Result<()> {
    let engine = crate::helpers::test_backend().await;
    let root = Entry::root_builder().build()?;
    let tree = root.id();
    engine.put_verified(root).await?;
    let pending = child(&tree, &tree, "pending");
    let failed = child(&tree, &tree, "failed");
    let good = child(&tree, &tree, "good");
    engine.put(pending.clone()).await?;
    engine.put(failed.clone()).await?;
    engine
        .update_verification_status(&failed.id(), VerificationStatus::Failed)
        .await?;
    engine.put_verified(good.clone()).await?;

    let raw = engine.snapshot(&tree).await?;
    assert_eq!(
        raw.len(),
        3,
        "positive control: raw tips include all statuses"
    );
    let sent = collect_ancestors_to_send(engine.as_ref(), raw.tips(), &[]).await?;
    assert_eq!(
        sent.iter().map(Entry::id).collect::<HashSet<_>>(),
        HashSet::from([tree.clone(), good.id()])
    );
    let delta =
        collect_ancestors_to_send(engine.as_ref(), raw.tips(), std::slice::from_ref(&tree)).await?;
    assert_eq!(
        delta.iter().map(Entry::id).collect::<Vec<_>>(),
        vec![good.id()]
    );

    engine
        .update_verification_status(&pending.id(), VerificationStatus::Verified)
        .await?;
    let sent =
        collect_ancestors_to_send(engine.as_ref(), &[pending.id(), failed.id()], &[]).await?;
    assert_eq!(
        sent.iter().map(Entry::id).collect::<HashSet<_>>(),
        HashSet::from([tree, pending.id()])
    );
    Ok(())
}

fn request(tree: &ID, tips: Snapshot) -> SyncRequest {
    SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree.clone(),
        our_tips: tips,
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: None,
    })
}

#[tokio::test]
async fn verified_sync_bootstrap_and_incremental_serve_only_verified_frontier() -> Result<()> {
    let (instance, mut owner) = eidetica::Instance::create_backend(
        crate::helpers::test_backend().await,
        eidetica::NewAccount::passwordless("owner"),
    )
    .await?;
    let key = owner.get_default_key()?;
    let db = owner
        .create_database(eidetica::crdt::Doc::new(), &key)
        .await?;
    let tx = db.new_transaction().await?;
    tx.get_settings()?
        .set_global_auth_key(eidetica::auth::AuthKey::active(
            None,
            eidetica::auth::Permission::Read,
        ))
        .await?;
    tx.commit().await?;
    owner
        .track_database(
            db.root_id().clone(),
            &key,
            eidetica::account::types::SyncSettings::enabled(),
        )
        .await?;
    let sync = eidetica::sync::Sync::new(instance.clone()).await?;
    sync.sync_account(owner.login_id(), owner.account().id())
        .await?;
    let tree = db.root_id().clone();
    let tx = db.new_transaction().await?;
    tx.get_store::<DocStore>("test")
        .await?
        .set_string("hello", "world")
        .await?;
    let good = tx.commit().await?;
    let engine = instance.backend().local_engine().unwrap();
    let expected = engine.verified_snapshot(&tree).await?;
    let pending = child(&tree, &tree, "pending");
    let failed = child(&tree, &tree, "failed");
    engine.put(pending.clone()).await?;
    engine.put(failed.clone()).await?;
    engine
        .update_verification_status(&failed.id(), VerificationStatus::Failed)
        .await?;
    // A raw-only orphan stays Unverified across authorization's verify-on-read.
    let orphan = child(&tree, &ID::from_bytes("withheld-parent"), "orphan");
    engine.put(orphan.clone()).await?;
    let handler = helpers::create_test_sync_handler(&sync);
    for tips in [Snapshot::EMPTY, Snapshot::new(vec![tree.clone()])] {
        let response = handler
            .handle_request(&request(&tree, tips), &RequestContext::default())
            .await;
        let entries = match response {
            SyncResponse::Bootstrap(response) => {
                let mut entries = response.all_entries;
                entries.push(response.root_entry);
                entries
            }
            SyncResponse::Incremental(response) => {
                assert_eq!(
                    Snapshot::new(response.their_tips),
                    expected,
                    "advertise Verified tips, not raw tips"
                );
                response.missing_entries
            }
            other => panic!("expected successful verified transfer: {other:?}"),
        };
        let ids: HashSet<_> = entries.iter().map(Entry::id).collect();
        assert!(
            ids.contains(&good),
            "positive control: new Verified content is served"
        );
        for excluded in [&pending, &failed, &orphan] {
            assert!(!ids.contains(&excluded.id()));
        }
        for entry in entries {
            assert_eq!(
                engine.get_verification_status(&entry.id()).await?,
                VerificationStatus::Verified
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn verified_sync_direct_send_refuses_nonverified_payload_before_network() -> Result<()> {
    use eidetica::{
        Database,
        auth::crypto::PrivateKey,
        crdt::Doc,
        sync::{Address, transports::http::HttpTransport},
    };
    let (source, sender) = helpers::setup().await;
    let (recipient, receiver) = helpers::setup().await;
    sender
        .register_transport("http", HttpTransport::builder())
        .await?;
    receiver
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    receiver.accept_connections().await?;
    let address = Address::http(receiver.get_server_address().await?);
    let db = Database::create(&source, PrivateKey::generate(), Doc::new()).await?;
    let engine = source.backend().local_engine().unwrap();
    let root = engine.get(db.root_id()).await?;
    sender
        .send_entries(std::slice::from_ref(&root), &address)
        .await?;
    assert!(
        recipient.has_entry(&root.id()).await,
        "positive control: Verified data reaches the peer"
    );
    let pending = child(db.root_id(), db.root_id(), "pending");
    engine.put(pending.clone()).await?;
    assert!(
        sender
            .send_entries(std::slice::from_ref(&pending), &address)
            .await
            .is_err()
    );
    assert!(!recipient.has_entry(&pending.id()).await);
    engine
        .update_verification_status(&pending.id(), VerificationStatus::Failed)
        .await?;
    assert!(
        sender
            .send_entries(std::slice::from_ref(&pending), &address)
            .await
            .is_err()
    );
    assert!(!recipient.has_entry(&pending.id()).await);
    let unstored = child(db.root_id(), db.root_id(), "unstored");
    assert!(
        sender
            .send_entries(std::slice::from_ref(&unstored), &address)
            .await
            .is_err()
    );
    assert!(!recipient.has_entry(&unstored.id()).await);
    receiver.stop_server().await?;
    Ok(())
}
