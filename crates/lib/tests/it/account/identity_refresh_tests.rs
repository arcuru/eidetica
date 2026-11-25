//! Moving each delegating database's accepted pin after an identity change.

use eidetica::{
    Cluster, Database, Instance, Result, Snapshot,
    account::Identity,
    auth::{
        crypto::{PrivateKey, PublicKey, generate_keypair},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey,
        },
    },
    backend::VerificationStatus,
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    store::DocStore,
};

use super::helpers::{login_account, setup_instance_with_user};
use crate::helpers::{next_snapshot, test_local_instance};

const BOUNDS: PermissionBounds = PermissionBounds {
    max: Permission::Write(10),
    min: None,
};

/// Create a database administered by `admin` that delegates to `identity`.
async fn delegating(
    instance: &Instance,
    admin: &PrivateKey,
    delegation: DelegatedTreeRef,
) -> Result<Database> {
    let mut auth = eidetica::auth::settings::AuthSettings::new();
    auth.add_key(
        &admin.public_key(),
        AuthKey::active(None, Permission::Admin(0)),
    )?;
    auth.add_delegated_tree(delegation)?;
    let mut settings = Doc::new();
    settings.set("name", "original");
    settings.set("auth", Doc::from(auth));
    Database::create(instance, admin.clone(), settings).await
}

/// The caller's operation: one database's outcome, never an aggregate.
async fn refresh(target: &Database, identity: &Database, cut: &Snapshot) -> Result<Option<ID>> {
    let tx = target.new_transaction().await?;
    if tx.get_settings()?.advance_delegation(identity, cut).await? {
        Ok(Some(tx.commit().await?))
    } else {
        Ok(None)
    }
}

/// Read back the accepted pin: dropping the transaction writes nothing.
async fn covers(target: &Database, identity: &Database, cut: &Snapshot) -> Result<bool> {
    let tx = target.new_transaction().await?;
    Ok(!tx.get_settings()?.advance_delegation(identity, cut).await?)
}

async fn declared(target: &Database, identity: &ID) -> Result<DelegatedTreeRef> {
    target
        .get_settings()
        .await?
        .auth_snapshot()
        .await?
        .get_delegated_tree(identity)
}

fn pin(reference: &DelegatedTreeRef) -> Snapshot {
    Snapshot::from(&reference.tree.tips)
}

fn path(steps: &[(&ID, &Snapshot)], member: &PublicKey) -> SigKey {
    SigKey::Delegation {
        path: steps
            .iter()
            .map(|(tree, at)| DelegationStep {
                tree: (*tree).clone(),
                tips: at.tips().to_vec(),
            })
            .collect(),
        hint: KeyHint::from_pubkey(member),
    }
}

async fn write(database: &Database) -> Result<ID> {
    let tx = database.new_transaction().await?;
    tx.get_store::<DocStore>("data")
        .await?
        .set("key", "value")
        .await?;
    tx.commit().await
}

async fn named(database: &Database, at: &Snapshot, name: &str) -> Result<Snapshot> {
    let tx = database.new_transaction_at(at).await?;
    tx.get_settings()?.set_name(name).await?;
    Ok(Snapshot::from(vec![tx.commit().await?]))
}

async fn identity_with_member(
    username: &str,
) -> Result<(Instance, Identity, PrivateKey, PublicKey, Snapshot)> {
    let (instance, username) = setup_instance_with_user(username, "").await;
    let mut session = login_account(&instance, &username, "").await;
    let identity = session
        .create_identity("personal", &session.get_default_key()?)
        .await?;
    let (secret, member) = generate_keypair();
    identity
        .add_key(&member, AuthKey::active(None, Permission::Write(10)))
        .await?;
    let before = identity.database().snapshot().await?;
    Ok((instance, identity, secret, member, before))
}

#[tokio::test]
async fn removal_cuts_off_only_databases_whose_pin_advanced() -> Result<()> {
    let (instance, identity, phone, phone_id, before) =
        identity_with_member("refresh_independent").await?;
    let root = identity.root_id();
    let admin = PrivateKey::generate();
    let a = delegating(&instance, &admin, identity.as_delegation(BOUNDS).await?).await?;
    let stronger = PermissionBounds {
        max: Permission::Admin(1),
        min: None,
    };
    let b = delegating(&instance, &admin, identity.as_delegation(stronger).await?).await?;

    // Independent grants on A: the operator's other settings must survive.
    let tx = a.new_transaction().await?;
    let settings = tx.get_settings()?;
    settings
        .set_auth_key(&phone_id, AuthKey::active(None, Permission::Write(50)))
        .await?;
    settings.set_name("renamed").await?;
    tx.commit().await?;
    let a_before = a.snapshot().await?;
    let a_declared = declared(&a, root).await?;

    let via = |target: &Database, at: &Snapshot| {
        target.clone().with_key(DatabaseKey::with_identity(
            phone.clone(),
            path(&[(root, at)], &phone_id),
        ))
    };
    write(&via(&a, &before)).await?;
    write(&via(&b, &before)).await?;
    let sibling_of = a.snapshot().await?;
    let owner = identity.open_database(a.root_id()).await?;
    write(&owner).await?;

    identity.revoke_key(&phone_id).await?;
    let cut = identity.database().snapshot().await?;
    let at_cut = identity.database().new_transaction_at(&cut).await?;
    assert_eq!(
        at_cut
            .get_settings()?
            .get_auth_key(&phone_id)
            .await?
            .status(),
        &KeyStatus::Revoked,
        "a covering frontier is not by itself a removal"
    );

    // Revoking the key moved no pin, so an old claim still signs on both.
    assert!(!covers(&a, identity.database(), &cut).await?);
    assert!(!covers(&b, identity.database(), &cut).await?);
    write(&via(&a, &before)).await?;

    // B's operator is an Admin, but ranks below this delegation's bounds.
    let (weaker, weaker_id) = generate_keypair();
    let tx = b.new_transaction().await?;
    tx.get_settings()?
        .set_auth_key(&weaker_id, AuthKey::active(None, Permission::Admin(5)))
        .await?;
    tx.commit().await?;
    let b_weaker = b.clone().with_key(DatabaseKey::new(weaker));

    let a_entry = refresh(&a, identity.database(), &cut).await?;
    let b_result = refresh(&b_weaker, identity.database(), &cut).await;
    assert!(a_entry.is_some());
    assert!(b_result.is_err(), "unauthorized refresh must not report B");
    assert!(covers(&a, identity.database(), &cut).await?);
    assert!(!covers(&b, identity.database(), &cut).await?);

    // Only the pin changed on A.
    let after = declared(&a, root).await?;
    assert_eq!(pin(&after), cut);
    assert_eq!(after.permission_bounds, a_declared.permission_bounds);
    assert_eq!(a.get_name().await?, "renamed");

    // The removed member can neither regress below A's pin nor use the cut.
    for at in [&before, &cut] {
        assert!(write(&via(&a, at)).await.is_err());
    }
    write(&via(&b, &before))
        .await
        .expect("B did not advance, so its old claim is not cut off");

    // Independent direct grant, earlier settings and an older sibling are kept.
    write(&a.clone().with_key(DatabaseKey::new(phone.clone()))).await?;
    let old = a.new_transaction_at(&a_before).await?;
    assert_eq!(
        old.get_settings()?
            .auth_snapshot()
            .await?
            .get_delegated_tree(root)?,
        a_declared
    );
    let stale = via(&a, &before).new_transaction_at(&sibling_of).await?;
    stale
        .get_store::<DocStore>("data")
        .await?
        .set("concurrent", "yes")
        .await?;
    stale
        .commit()
        .await
        .expect("a branch that never observed the refresh is not cut off");

    // A handle opened before the refresh claims an older cut. Reopen it.
    assert!(write(&owner).await.is_err());
    write(&identity.open_database(a.root_id()).await?).await?;

    // Repeating is idempotent; B can advance later under its own authority.
    assert!(refresh(&a, identity.database(), &cut).await?.is_none());
    assert!(refresh(&b, identity.database(), &cut).await?.is_some());
    assert!(write(&via(&b, &before)).await.is_err());
    Ok(())
}

#[tokio::test]
async fn advance_keeps_incomparable_pins_and_never_rewinds() -> Result<()> {
    let (instance, identity, _, _, before) = identity_with_member("refresh_frontiers").await?;
    let root = identity.root_id();
    let database = identity.database();
    let x = named(database, &before, "x").await?;
    let y = named(database, &before, "y").await?;
    let both = database.snapshot().await?;
    assert_eq!(both.len(), 2);
    let merged = named(database, &both, "merged").await?;

    let mut delegation = identity.as_delegation(BOUNDS).await?;
    delegation.tree.tips = x.tips().to_vec();
    let target = delegating(&instance, &PrivateKey::generate(), delegation).await?;

    // A stale replica's frontier is already covered, so nothing is written.
    let unchanged = target.snapshot().await?;
    assert!(refresh(&target, database, &before).await?.is_none());
    assert_eq!(target.snapshot().await?, unchanged);

    // Incomparable, including a request that repeats part of the pin.
    for request in [&both, &y] {
        let tx = target.new_transaction().await?;
        assert!(
            tx.get_settings()?
                .advance_delegation(database, request)
                .await?
        );
        assert_eq!(
            pin(&tx
                .get_settings()?
                .auth_snapshot()
                .await?
                .get_delegated_tree(root)?),
            both
        );
        // The staged pin is visible to this transaction.
        assert!(
            !tx.get_settings()?
                .advance_delegation(database, request)
                .await?
        );
    }
    assert!(refresh(&target, database, &y).await?.is_some());
    assert!(refresh(&target, database, &x).await?.is_none());
    assert!(refresh(&target, database, &merged).await?.is_some());
    assert_eq!(pin(&declared(&target, root).await?), merged);

    // Nothing vacuous: an empty cut, undeclared root or foreign cut is an error.
    assert!(covers(&target, database, &Snapshot::EMPTY).await.is_err());
    let other = Database::create(&instance, PrivateKey::generate(), Doc::new()).await?;
    let other_cut = other.snapshot().await?;
    assert!(covers(&target, &other, &other_cut).await.is_err());
    assert!(refresh(&target, database, &other_cut).await.is_err());
    assert_eq!(pin(&declared(&target, root).await?), merged);
    Ok(())
}

/// A pin joined with an incomparable branch covers the removal's history, but
/// the key's state there is whatever the Identity's own merge decides.
#[tokio::test]
async fn covered_removal_can_lose_at_a_joined_pin() -> Result<()> {
    let (instance, identity, phone, phone_id, before) =
        identity_with_member("refresh_joined").await?;
    let database = identity.database();
    let taller = named(database, &before, "taller branch").await?;
    let tx = database.new_transaction_at(&taller).await?;
    tx.get_settings()?
        .set_auth_key(&phone_id, AuthKey::active(None, Permission::Write(10)))
        .await?;
    let kept = Snapshot::from(vec![tx.commit().await?]);
    let delegation = identity.as_delegation(BOUNDS).await?;
    assert_eq!(pin(&delegation), kept);
    let target = delegating(&instance, &PrivateKey::generate(), delegation).await?;

    let tx = database.new_transaction_at(&before).await?;
    tx.get_settings()?.revoke_auth_key(&phone_id).await?;
    let cut = Snapshot::from(vec![tx.commit().await?]);
    let status = async |at: &Snapshot| -> Result<KeyStatus> {
        let tx = database.new_transaction_at(at).await?;
        let key = tx.get_settings()?.get_auth_key(&phone_id).await?;
        Ok(key.status().clone())
    };
    assert_eq!(status(&cut).await?, KeyStatus::Revoked);

    refresh(&target, database, &cut).await?;
    assert!(covers(&target, database, &cut).await?);
    let joined = pin(&declared(&target, identity.root_id()).await?);
    assert_eq!(status(&joined).await?, KeyStatus::Active);
    let via = target.clone().with_key(DatabaseKey::with_identity(
        phone,
        path(&[(identity.root_id(), &joined)], &phone_id),
    ));
    write(&via)
        .await
        .expect("coverage alone does not cut the key off");

    // Removing it again on the joined Identity history does.
    identity.revoke_key(&phone_id).await?;
    let cut = database.snapshot().await?;
    assert_eq!(status(&cut).await?, KeyStatus::Revoked);
    assert!(refresh(&target, database, &cut).await?.is_some());
    assert_eq!(pin(&declared(&target, identity.root_id()).await?), cut);
    assert!(write(&via).await.is_err());
    Ok(())
}

/// Delegations merge whole: a concurrent edit and a refresh cannot both win.
#[tokio::test]
async fn concurrent_edit_requires_checking_the_merged_delegation() -> Result<()> {
    let (instance, identity, _, member, before) =
        identity_with_member("refresh_concurrent").await?;
    identity.revoke_key(&member).await?;
    let cut = identity.database().snapshot().await?;
    let root = identity.root_id();

    for (removes, edit_wins) in [(false, true), (false, false), (true, true), (true, false)] {
        let mut delegation = identity.as_delegation(BOUNDS).await?;
        delegation.tree.tips = before.tips().to_vec();
        let target = delegating(&instance, &PrivateKey::generate(), delegation.clone()).await?;
        let base = target.snapshot().await?;
        // The higher settings write wins the merge.
        let higher = named(&target, &base, "taller branch").await?;
        let (edit_at, refresh_at) = if edit_wins {
            (&higher, &base)
        } else {
            (&base, &higher)
        };

        let mut narrowed = delegation;
        narrowed.permission_bounds.max = Permission::Read;
        let tx = target.new_transaction_at(edit_at).await?;
        let settings = tx.get_settings()?;
        if removes {
            settings
                .as_doc_store()
                .delete(format!("auth.delegations.{root}"))
                .await?;
        } else {
            settings.add_delegated_tree(narrowed.clone()).await?;
        }
        tx.commit().await?;

        let tx = target.new_transaction_at(refresh_at).await?;
        assert!(
            tx.get_settings()?
                .advance_delegation(identity.database(), &cut)
                .await?
        );
        tx.commit().await?;

        // A committed refresh is not proof of the merged delegation.
        let covered = covers(&target, identity.database(), &cut).await;
        match (removes, edit_wins) {
            (true, true) => {
                assert!(covered.is_err());
                assert!(declared(&target, root).await.is_err());
            }
            (false, true) => {
                assert!(!covered?);
                assert_eq!(declared(&target, root).await?, narrowed);
                assert!(refresh(&target, identity.database(), &cut).await?.is_some());
                let repaired = declared(&target, root).await?;
                assert_eq!(pin(&repaired), cut);
                assert_eq!(repaired.permission_bounds, narrowed.permission_bounds);
            }
            // Covered does not mean a concurrent change survived.
            (_, false) => {
                assert!(covered?);
                assert_eq!(declared(&target, root).await?.permission_bounds, BOUNDS);
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn nested_delegation_advances_one_parent_at_a_time() -> Result<()> {
    let (instance, identity, secret, member, before) =
        identity_with_member("refresh_nested").await?;
    let team = delegating(
        &instance,
        &PrivateKey::generate(),
        identity.as_delegation(BOUNDS).await?,
    )
    .await?;
    let team_before = team.snapshot().await?;
    let team_delegation = DelegatedTreeRef {
        permission_bounds: BOUNDS,
        tree: eidetica::auth::types::TreeReference {
            root: team.root_id().clone(),
            tips: team_before.tips().to_vec(),
        },
    };
    let project = delegating(&instance, &PrivateKey::generate(), team_delegation).await?;
    let via = |team_at: &Snapshot| {
        project.clone().with_key(DatabaseKey::with_identity(
            secret.clone(),
            path(
                &[(team.root_id(), team_at), (identity.root_id(), &before)],
                &member,
            ),
        ))
    };
    write(&via(&team_before)).await?;

    identity.revoke_key(&member).await?;
    let cut = identity.database().snapshot().await?;
    // The outer database has no pin on the identity to refresh directly.
    assert!(
        refresh(&project, identity.database(), &cut).await.is_err(),
        "project delegates to the team, not to the identity"
    );
    assert!(refresh(&team, identity.database(), &cut).await?.is_some());
    let team_cut = team.snapshot().await?;
    assert!(!covers(&project, &team, &team_cut).await?);
    write(&via(&team_before))
        .await
        .expect("the outer pin has not moved yet");

    assert!(refresh(&project, &team, &team_cut).await?.is_some());
    for team_at in [&team_before, &team_cut] {
        assert!(write(&via(team_at)).await.is_err());
    }
    Ok(())
}

#[tokio::test]
async fn unverified_cut_is_not_accepted_as_a_pin() -> Result<()> {
    let instance = test_local_instance().await;
    let mut session = login_account(&instance, "admin", "").await;
    let identity = session
        .create_identity("personal", &session.get_default_key()?)
        .await?;
    let target = delegating(
        &instance,
        &PrivateKey::generate(),
        identity.as_delegation(BOUNDS).await?,
    )
    .await?;
    let old = declared(&target, identity.root_id()).await?;

    let cut = named(
        identity.database(),
        &identity.database().snapshot().await?,
        "held but unverified",
    )
    .await?;
    let engine = instance.backend().local_engine().unwrap();
    engine
        .update_verification_status(&cut.tips()[0], VerificationStatus::Unverified)
        .await?;

    assert!(refresh(&target, identity.database(), &cut).await.is_err());
    assert_eq!(declared(&target, identity.root_id()).await?, old);

    identity.database().verify().await?;
    assert!(refresh(&target, identity.database(), &cut).await?.is_some());
    Ok(())
}

/// Each delegating database lives with its own operator. One is cut off from
/// the identity, and its absence is visible rather than counted as advanced.
#[tokio::test]
async fn unavailable_recipient_is_not_reported_as_advanced() -> Result<()> {
    let mut net = Cluster::builder().peers(3).build().await?;
    let key = net.peer(0).key_id().clone();
    let session = net.peer_mut(0).account_session_mut();
    let identity = session.create_identity("personal", &key).await?;
    let (_, member) = generate_keypair();
    identity
        .add_key(&member, AuthKey::active(None, Permission::Write(10)))
        .await?;
    let root = identity.root_id().clone();
    net.peer_mut(0).serve(&root).await?;

    let mut recipients = Vec::new();
    for peer in 1..3 {
        net.bootstrap(0, peer, &root, Permission::Read).await?;
        let instance = net.peer(peer).instance();
        let replica = Database::open(instance, &root).await?;
        replica.verify().await?;
        assert_eq!(
            replica.snapshot().await?,
            identity.database().snapshot().await?
        );
        let target = delegating(
            instance,
            &PrivateKey::generate(),
            identity.as_delegation(BOUNDS).await?,
        )
        .await?;
        recipients.push((replica, target));
    }
    let (tx, mut notified) = tokio::sync::mpsc::unbounded_channel();
    let _watch = recipients[0]
        .0
        .on_write(move |latest, _| {
            let _ = tx.send(latest.clone());
            async { Ok(()) }
        })
        .await?;
    next_snapshot(&mut notified).await;

    identity.revoke_key(&member).await?;
    let cut = identity.database().snapshot().await?;

    // Only peer 1 hears about the change. Its watch says what it accepted.
    net.exchange(1, 0, &root).await?;
    assert_eq!(next_snapshot(&mut notified).await, cut);
    let mut advanced = Vec::new();
    for (replica, target) in &recipients {
        advanced.push(refresh(target, replica, &cut).await);
    }
    assert!(matches!(advanced[0], Ok(Some(_))));
    assert!(
        matches!(&advanced[1], Err(error) if error.is_not_found()),
        "missing identity history is not an already-covered pin"
    );
    assert!(covers(&recipients[0].1, &recipients[0].0, &cut).await?);
    assert!(
        covers(&recipients[1].1, &recipients[1].0, &cut)
            .await
            .is_err()
    );
    let stale = declared(&recipients[1].1, &root).await?;
    assert_ne!(pin(&stale), cut);

    // The unavailable recipient catches up on its own schedule.
    net.exchange(2, 0, &root).await?;
    recipients[1].0.verify().await?;
    let (replica, target) = &recipients[1];
    assert!(refresh(target, replica, &cut).await?.is_some());
    assert!(covers(target, replica, &cut).await?);
    Ok(())
}

/// A refresh committed at one replica is only a request to every other one.
#[tokio::test]
async fn committed_refresh_is_accepted_per_replica() -> Result<()> {
    let mut net = Cluster::builder().peers(2).build().await?;
    let key = net.peer(0).key_id().clone();
    let session = net.peer_mut(0).account_session_mut();
    let identity = session.create_identity("personal", &key).await?;
    let (_, member) = generate_keypair();
    identity
        .add_key(&member, AuthKey::active(None, Permission::Write(10)))
        .await?;
    let target = session.create_database(Doc::new(), &key).await?;
    let tx = target.new_transaction().await?;
    let settings = tx.get_settings()?;
    settings
        .add_delegated_tree(identity.as_delegation(BOUNDS).await?)
        .await?;
    settings
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await?;
    tx.commit().await?;

    let roots = [identity.root_id().clone(), target.root_id().clone()];
    for root in &roots {
        net.peer_mut(0).serve(root).await?;
        net.bootstrap(0, 1, root, Permission::Read).await?;
    }
    let instance = net.peer(1).instance();
    let replica = Database::open(instance, &roots[0]).await?;
    let recipient = Database::open(instance, &roots[1]).await?;
    replica.verify().await?;
    recipient.verify().await?;
    let old = declared(&recipient, &roots[0]).await?;
    assert_eq!(old, declared(&target, &roots[0]).await?);

    identity.revoke_key(&member).await?;
    let cut = identity.database().snapshot().await?;
    let entry = refresh(&target, identity.database(), &cut).await?.unwrap();
    assert!(covers(&target, identity.database(), &cut).await?);

    // The recipient holds the settings entry, but not the proof behind it.
    net.exchange(1, 0, &roots[1]).await?;
    assert_eq!(recipient.verify().await?.still_unverified, 1);
    let engine = instance.backend().local_engine().unwrap();
    assert_eq!(
        engine.get_verification_status(&entry).await?,
        VerificationStatus::Unverified
    );
    assert_eq!(declared(&recipient, &roots[0]).await?, old);
    assert!(
        matches!(covers(&recipient, &replica, &cut).await, Err(error) if error.is_not_found()),
        "holding the settings entry does not establish the cut"
    );

    // Proof first, then the dependent database.
    net.exchange(1, 0, &roots[0]).await?;
    replica.verify().await?;
    recipient.verify().await?;
    assert!(covers(&recipient, &replica, &cut).await?);
    Ok(())
}
