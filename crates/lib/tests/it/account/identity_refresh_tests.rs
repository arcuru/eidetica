//! Causal floor advances are normal data writes, not Settings changes.

use eidetica::{
    AccountSession, Cluster, Database, Result, Snapshot,
    account::{DelegationAdvance, Identity},
    auth::{AuthKey, Permission, PermissionBounds, crypto::generate_keypair},
    backend::VerificationStatus,
    database::DatabaseKey,
    store::DocStore,
};

use crate::helpers::test_instance_with_user;

async fn target(
    account: &mut AccountSession,
    identity: &Identity,
    writer: &eidetica::auth::PublicKey,
    max: Permission,
    name: &str,
) -> Result<Database> {
    let db = account.new_database().name(name).build().await?.0;
    let tx = db.new_transaction().await?;
    let settings = tx.get_settings()?;
    settings
        .add_delegated_tree(
            identity
                .as_delegation(PermissionBounds { max, min: None })
                .await?,
        )
        .await?;
    settings
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await?;
    // If a refresh fell back to the writer's direct grant it could write even
    // when the selected delegation is Read-only.
    settings
        .set_auth_key(writer, AuthKey::active(None, Permission::Write(1)))
        .await?;
    tx.commit().await?;
    Ok(db)
}

#[tokio::test]
async fn test_identity_advance_sweep_on_two_replicas() -> Result<()> {
    let mut net = Cluster::builder().peers(2).build().await?;
    let owner = net.peer(0).key_id().clone();
    let session = net.peer_mut(0).account_session_mut();
    let admin = session.create_identity("team", &owner).await?;
    let writer = session.add_private_key(Some("writer")).await?;
    let (old_signer, removed_member) = generate_keypair();
    for member in [&writer, &removed_member] {
        admin
            .add_key(member, AuthKey::active(None, Permission::Write(5)))
            .await?;
    }
    let selected = session.select_identity_key("team", &writer).await?;
    let mut old_path = admin.delegation_key().await?;
    old_path.hint_mut().pubkey = Some(removed_member.clone());
    let stale = DatabaseKey::with_identity(old_signer, old_path);
    let a = target(session, &selected, &writer, Permission::Write(5), "a").await?;
    let b = target(session, &selected, &writer, Permission::Write(10), "b").await?;
    let read_only = target(session, &selected, &writer, Permission::Read, "read-only").await?;
    let unrelated = session.new_database().name("unrelated").build().await?.0;
    let old_unrelated = unrelated.snapshot().await?;
    let old_read_only = read_only.snapshot().await?;
    let targets = [&a, &b];
    let roots = [admin.root_id(), a.root_id(), b.root_id()];
    for root in roots {
        net.peer_mut(0).serve(root).await?;
        net.bootstrap(0, 1, root, Permission::Read).await?;
    }
    let replica = net.peer(1).instance().clone();
    let mut before = Vec::new();
    for target in targets {
        let received = Database::open(&replica, target.root_id()).await?;
        received.verify().await?;
        let parents = received.snapshot().await?;
        let auth = received.get_settings().await?.auth_snapshot().await?;
        assert_eq!(parents, target.snapshot().await?);
        assert_eq!(
            selected
                .open_database(target.root_id())
                .await?
                .current_permission()
                .await?,
            auth.get_delegated_tree(admin.root_id())?
                .permission_bounds
                .max
        );
        before.push((received, parents, auth));
    }

    admin.revoke_key(&removed_member).await?;
    let cut = admin.database().snapshot().await?;
    let claim = selected.delegation_key().await?;
    let results = net
        .peer(0)
        .account_session()
        .advance_delegations("team")
        .await?;
    let lookup = |id| &results.iter().find(|(root, _)| root == id).unwrap().1;
    assert!(matches!(
        lookup(unrelated.root_id()),
        DelegationAdvance::Skipped
    ));
    assert!(
        matches!(lookup(read_only.root_id()), DelegationAdvance::Failed(error) if error.is_permission_denied())
    );
    assert_eq!(unrelated.snapshot().await?, old_unrelated);
    assert_eq!(read_only.snapshot().await?, old_read_only);

    for target in targets {
        let DelegationAdvance::Updated(entry_id) = lookup(target.root_id()) else {
            panic!(
                "target {} was not updated: {:?}",
                target.root_id(),
                lookup(target.root_id())
            );
        };
        let entry = target.get_entry(entry_id).await?;
        assert!(entry.subtrees().is_empty());
        assert_eq!(entry.auth().key, claim);
        net.exchange(1, 0, target.root_id()).await?;
        let engine = replica.backend().local_engine().unwrap();
        assert_eq!(
            engine.get_verification_status(entry_id).await?,
            VerificationStatus::Unverified,
            "delivery before the Identity proof is not Verified acceptance"
        );
    }
    net.exchange(1, 0, admin.root_id()).await?;
    let identity_replica = Database::open(&replica, admin.root_id()).await?;
    identity_replica.verify().await?;
    assert_eq!(identity_replica.snapshot().await?, cut);
    for (target, (received, old, old_auth)) in targets.into_iter().zip(before) {
        received.verify().await?;
        let DelegationAdvance::Updated(entry_id) = lookup(target.root_id()) else {
            unreachable!()
        };
        assert_eq!(
            received.snapshot().await?,
            Snapshot::from([entry_id.clone()])
        );
        let current_auth = received.get_settings().await?.auth_snapshot().await?;
        assert_eq!(current_auth.as_doc(), old_auth.as_doc());
        assert_eq!(
            target.get_settings().await?.auth_snapshot().await?.as_doc(),
            old_auth.as_doc()
        );

        // Same signer and claim, different causal parents. The prior branch is
        // still writable under its old authority, but a descendant is rejected.
        let old_handle = received.with_key(stale.clone());
        assert!(old_handle.new_transaction().await?.commit().await.is_err());
        let sibling = old_handle.new_transaction_at(&old).await?.commit().await?;
        assert_eq!(
            old_handle.snapshot().await?,
            Snapshot::from([entry_id.clone(), sibling])
        );
    }

    let repeat = net
        .peer(0)
        .account_session()
        .advance_delegations("team")
        .await?;
    for target in targets {
        let id = target.root_id();
        assert!(repeat.iter().any(
            |(root, result)| root == id && matches!(result, DelegationAdvance::AlreadyCovered)
        ));
        let DelegationAdvance::Updated(entry_id) = lookup(id) else {
            unreachable!()
        };
        assert_eq!(target.snapshot().await?, Snapshot::from([entry_id.clone()]));
    }
    Ok(())
}

#[tokio::test]
async fn test_identity_advance_one_joins_concurrent_frontiers() -> Result<()> {
    // Also exercises an actual daemon connection with TEST_BACKEND=service.
    let (_instance, mut account) = test_instance_with_user("refresh").await;
    let owner = account.get_default_key()?;
    let admin = account.create_identity("team", &owner).await?;
    let writer = account.add_private_key(Some("writer")).await?;
    admin
        .add_key(&writer, AuthKey::active(None, Permission::Write(5)))
        .await?;
    let selected = account.select_identity_key("team", &writer).await?;
    let db = target(
        &mut account,
        &selected,
        &writer,
        Permission::Write(5),
        "target",
    )
    .await?;
    assert_eq!(
        selected.advance_delegation(db.root_id()).await?,
        None,
        "the configured floor counts"
    );
    let read_only = target(
        &mut account,
        &selected,
        &writer,
        Permission::Read,
        "read-only",
    )
    .await?;
    let unrelated = account.new_database().name("unrelated").build().await?.0;
    let old_target = db.snapshot().await?;
    let old_identity = admin.database().snapshot().await?;

    let mut target_tips = Vec::new();
    let mut identity_tips = Vec::new();
    for name in ["left", "right"] {
        let tx = db.new_transaction_at(&old_target).await?;
        tx.get_store::<DocStore>("data")
            .await?
            .set(name, "value")
            .await?;
        target_tips.push(tx.commit().await?);
        let tx = admin.database().new_transaction_at(&old_identity).await?;
        tx.get_settings()?.set_name(name).await?;
        identity_tips.push(tx.commit().await?);
    }
    let target_frontier = Snapshot::from(target_tips);
    assert_eq!(db.snapshot().await?, target_frontier);
    assert_eq!(
        admin.database().snapshot().await?,
        Snapshot::from(identity_tips)
    );
    let entry = selected
        .advance_delegation(db.root_id())
        .await?
        .expect("new cut");
    let authored = db.get_entry(&entry).await?;
    assert!(authored.subtrees().is_empty());
    assert_eq!(Snapshot::from(authored.parents()?), target_frontier);
    assert_eq!(authored.auth().key, selected.delegation_key().await?);

    // A subsequent direct-key write carries the inherited floor without its
    // own delegation claim. An idempotence check must not only read tip auth.
    let later = db.new_transaction().await?.commit().await?;
    let before = read_only.snapshot().await?;
    let outcomes = account.advance_delegations("team").await?;
    let lookup = |id| &outcomes.iter().find(|(root, _)| root == id).unwrap().1;
    assert!(matches!(
        lookup(db.root_id()),
        DelegationAdvance::AlreadyCovered
    ));
    assert!(matches!(
        lookup(unrelated.root_id()),
        DelegationAdvance::Skipped
    ));
    assert!(
        matches!(lookup(read_only.root_id()), DelegationAdvance::Failed(error) if error.is_permission_denied())
    );
    assert_eq!(db.snapshot().await?, Snapshot::from([later]));
    assert_eq!(read_only.snapshot().await?, before);
    Ok(())
}
