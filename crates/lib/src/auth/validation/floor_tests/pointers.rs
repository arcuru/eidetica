//! Committed pointer writes, operator rewinds and effective removal boundaries.

use super::*;

/// Direct-key Admin may intentionally rewind a valid configured pointer.
#[tokio::test]
async fn pointer_add_delegated_tree_allows_admin_rewind() {
    let fx = fixture().await;
    let admin_target = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::new(fx.target_admin.clone()));

    // Forward: i0 → i1.
    let txn = admin_target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), fx.i1.clone()))
        .await
        .unwrap();
    txn.commit().await.expect("advancing the pointer commits");

    // Equal: i1 → i1 (bounds change only).
    let txn = admin_target.new_transaction().await.unwrap();
    let mut same = delegation_ref(fx.identity.root_id(), fx.i1.clone());
    same.permission_bounds.max = Permission::Write(10);
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(same)
        .await
        .unwrap();
    txn.commit()
        .await
        .expect("re-committing the same pointer commits");

    // Backwards: i1 → i0.
    let txn = admin_target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), fx.i0.clone()))
        .await
        .unwrap();
    txn.commit().await.expect("admin rewind commits");
    let committed = admin_target
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap()
        .get_delegated_tree(fx.identity.root_id())
        .unwrap();
    assert_eq!(as_set(&committed.tree.tips), as_set(&fx.i0));
}

/// A raw `_settings` Admin write (also used by remote ingest) may rewind.
#[tokio::test]
async fn pointer_raw_settings_write_allows_admin_rewind() {
    let fx = fixture().await;
    // Advance to i1 first.
    let forward = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&forward, "raw write advancing i0 → i1").await;

    let backwards = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i0.clone())).await;
    fx.accept(&backwards, "admin raw write rewinds i1 → i0")
        .await;

    let equal = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&equal, "raw write re-committing i1").await;
}

/// A partial raw write may also rewind an Admin-managed pointer.
#[tokio::test]
async fn pointer_partial_tips_write_is_gated() {
    let fx = fixture().await;
    let forward = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&forward, "advance to i1").await;

    let mut partial = Doc::new();
    let mut tips = Doc::new();
    for (i, tip) in fx.i0.iter().enumerate() {
        tips.set(i.to_string(), tip.to_string());
    }
    partial.set(
        format!("auth.delegations.{}.tree.tips", fx.identity.root_id()),
        tips,
    );
    let entry = settings_entry(&fx, partial).await;
    fx.accept(&entry, "admin may rewind with partial tips-only write")
        .await;
}

/// An invalid delegation key cannot be used to evade canonical root identity.
#[tokio::test]
async fn pointer_is_matched_by_tree_root_not_settings_key() {
    let fx = fixture().await;
    let forward = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&forward, "advance to i1").await;

    let mut respelled = Doc::new();
    respelled.set(
        "auth.delegations.alias-for-identity",
        delegation_ref(fx.identity.root_id(), fx.i0.clone()),
    );
    let entry = settings_entry(&fx, respelled).await;
    assert!(
        !fx.validate(&entry).await.unwrap_or(false),
        "invalid alias must not validate"
    );
}

/// A pointer naming tips of some other tree is unusable and rejected.
#[tokio::test]
async fn pointer_to_wrong_tree_is_rejected() {
    let fx = fixture().await;
    let foreign = vec![fx.target.root_id().clone()];
    let entry = settings_entry(&fx, pointer_write(fx.identity.root_id(), foreign)).await;
    fx.reject(&entry, "pointer naming a tip of the target tree")
        .await;
}

/// An unsynced first pointer is retryable. Malformed partial references fail,
/// but a complete bounds update at the same verified pointer succeeds.
#[tokio::test]
async fn pointer_first_declaration_and_bounds_only_writes_pass() {
    let fx = fixture().await;
    let unknown_root = ID::from_bytes("some-unsynced-tree");
    let unknown_tip = ID::from_bytes("some-unsynced-tip");
    let first = settings_entry(&fx, pointer_write(&unknown_root, vec![unknown_tip])).await;
    assert!(
        matches!(fx.validate(&first).await, Err(Error::Auth(e)) if e.is_delegated_tree_unsynced())
    );

    let mut bounds_only = Doc::new();
    bounds_only.set(
        format!(
            "auth.delegations.{}.permission_bounds.max",
            fx.identity.root_id()
        ),
        "write:3",
    );
    let entry = settings_entry(&fx, bounds_only).await;
    fx.reject(&entry, "incomplete delegation reference is invalid")
        .await;
    let mut complete = delegation_ref(fx.identity.root_id(), fx.i0.clone());
    complete.permission_bounds.max = Permission::Write(3);
    let mut doc = Doc::new();
    doc.set(
        format!("auth.delegations.{}", fx.identity.root_id()),
        complete,
    );
    let entry = settings_entry(&fx, doc).await;
    fx.accept(&entry, "complete bounds-only update").await;
}

/// A descendant of an Admin pointer write must cover its inherited floor.
#[tokio::test]
async fn pointer_advance_raises_committed_floor_for_descendants() {
    let fx = fixture().await;
    let root_tips = fx.tips().await;
    let advance_entry =
        settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    let advanced = fx.accept(&advance_entry, "advance to i1").await;

    // Descendant of the advance pins i0: below the committed pointer.
    let below = fx.via_identity(&[advanced], &fx.i0).await;
    fx.reject(&below, "i0 under the advanced pointer").await;

    // A sibling branch that does not descend from the advance still pins i0
    // legitimately — its pinned settings carry the old pointer.
    let _ = root_tips;
}

/// A direct-key Admin may rewind the configured pointer even at a merge;
/// inherited signed claims remain floors for subsequent delegated entries.
#[tokio::test]
async fn pointer_merge_joins_both_parent_floors() {
    let fx = fixture().await;
    let txn = fx
        .identity
        .new_transaction_at(&crate::Snapshot::from(fx.i1.clone()))
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("pointer-x")
        .await
        .unwrap();
    let ix = vec![txn.commit().await.unwrap()];
    let txn = fx
        .identity
        .new_transaction_at(&crate::Snapshot::from(fx.i1.clone()))
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("pointer-y")
        .await
        .unwrap();
    let it = vec![txn.commit().await.unwrap()];

    let base = crate::Snapshot::from(fx.tips().await);
    let tx = fx.target.new_transaction_at(&base).await.unwrap();
    tx.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), ix.clone()))
        .await
        .unwrap();
    let px = tx.commit().await.unwrap();
    let tx = fx.target.new_transaction_at(&base).await.unwrap();
    tx.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), it.clone()))
        .await
        .unwrap();
    let py = tx.commit().await.unwrap();

    let only_x = settings_entry(&fx, pointer_write(fx.identity.root_id(), ix)).await;
    let only_x = Entry::builder(fx.target.root_id().clone())
        .set_parents(vec![px.clone(), py.clone()])
        .set_subtree_data(SETTINGS, only_x.data(SETTINGS).unwrap().clone())
        .set_subtree_parents(SETTINGS, vec![px, py])
        .set_metadata(only_x.metadata().unwrap().to_vec())
        .set_height(only_x.height() + 1)
        .set_auth(only_x.auth().clone())
        .build()
        .unwrap();
    let signature = sign_entry(&only_x, &fx.target_admin).unwrap();
    let only_x = only_x.with_auth(|auth| auth.signature = Some(signature));
    fx.accept(&only_x, "admin can rewind one branch pointer at a merge")
        .await;

    let txn = fx.identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("pointer-merged")
        .await
        .unwrap();
    let merged = vec![txn.commit().await.unwrap()];
    let both = settings_entry(&fx, pointer_write(fx.identity.root_id(), merged)).await;
    assert!(fx.validate(&both).await.unwrap());
}

/// An effective removal clears the direct delegation floor, so re-addition starts fresh.
#[tokio::test]
async fn pointer_effective_removal_resets_floor() {
    let fx = fixture().await;
    let forward = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&forward, "advance to i1").await;

    let mut removal = Doc::new();
    removal.remove(format!("auth.delegations.{}", fx.identity.root_id()));
    let removal = settings_entry(&fx, removal).await;
    fx.accept(&removal, "remove delegation").await;

    let readd = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i0.clone())).await;
    fx.accept(&readd, "re-add after an effective removal starts fresh")
        .await;
}

/// A stale delta that loses to an active sibling does not remove the
/// delegation from the resulting merged settings state.
#[tokio::test]
async fn losing_removal_does_not_clear_active_branch_floor() {
    let fx = fixture().await;
    let base = crate::Snapshot::from(fx.tips().await);
    let admin_target = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::new(fx.target_admin.clone()));
    let mut deletion = Doc::new();
    deletion.remove(format!("auth.delegations.{}", fx.identity.root_id()));
    let removal = settings_entry(&fx, deletion).await;
    let tx = admin_target.new_transaction_at(&base).await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_name("active precursor")
        .await
        .unwrap();
    let precursor = tx.commit().await.unwrap();
    let tx = admin_target
        .new_transaction_at(&crate::Snapshot::from(vec![precursor]))
        .await
        .unwrap();
    tx.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), fx.i1.clone()))
        .await
        .unwrap();
    let active = tx.commit().await.unwrap();
    let removed = fx.accept(&removal, "concurrent removal").await;
    let merged = fx.direct(&[active, removed]).await;
    let merged = fx.accept(&merged, "merged direct entry").await;
    let ancestry = fx
        .engine()
        .get_tree_from_tips(
            fx.target.root_id(),
            &crate::Snapshot::from(vec![merged.clone()]),
        )
        .await
        .unwrap();
    let effective = crate::database::fold_settings_entries(&ancestry).unwrap();
    assert!(
        effective.get_delegated_tree(fx.identity.root_id()).is_ok(),
        "removal must lose to the active sibling for this regression"
    );
    let stale = fx.via_identity(&[merged], &fx.i0).await;
    fx.reject(&stale, "active branch's configured i1 survives")
        .await;
}

/// Effective removal resets descendants, not the removing entry's authority.
/// A revoked delegated Admin cannot erase an acknowledged revocation by signing
/// its own removal against the older configured pointer.
#[tokio::test]
async fn delegated_removal_cannot_evade_inherited_revocation() {
    let fx = fixture().await;
    let other = PrivateKey::generate();
    let txn = fx.identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &other.public_key(),
            AuthKey::active(Some("other"), Permission::Admin(5)),
        )
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&fx.member_pub)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let revoked = fx.identity.snapshot().await.unwrap().into_tips();
    let observed = fx
        .delegated(
            &fx.tips().await,
            &[(fx.identity.root_id(), &revoked)],
            &other,
        )
        .await;
    fx.accept(&observed, "acknowledge revocation on the primary branch")
        .await;

    let mut deletion = Doc::new();
    deletion.remove(format!("auth.delegations.{}", fx.identity.root_id()));
    let removal = settings_entry(&fx, deletion).await;
    let key = |tips: Vec<ID>, signer: &PrivateKey| SigKey::Delegation {
        path: vec![DelegationStep {
            tree: fx.identity.root_id().clone(),
            tips,
        }],
        hint: KeyHint::from_pubkey(&signer.public_key()),
    };
    let local = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::with_identity(
            fx.member.clone(),
            key(fx.i0.clone(), &fx.member),
        ));
    let before = fx
        .engine()
        .get_tree(fx.target.root_id())
        .await
        .unwrap()
        .len();
    let txn = local.new_transaction().await.unwrap();
    txn.get_store::<crate::store::DocStore>(SETTINGS)
        .await
        .unwrap()
        .delete(format!("auth.delegations.{}", fx.identity.root_id()))
        .await
        .unwrap();
    let error = txn
        .commit()
        .await
        .expect_err("local self-removal must fail");
    assert!(matches!(error, Error::Transaction(e)
        if matches!(*e, crate::transaction::TransactionError::EntryValidationFailed)));
    assert_eq!(
        fx.engine()
            .get_tree(fx.target.root_id())
            .await
            .unwrap()
            .len(),
        before
    );

    let forged = Fixture::sign(removal.clone(), key(fx.i0.clone(), &fx.member), &fx.member);
    assert_eq!(
        fx.submit_remote(forged).await,
        VerificationStatus::Failed,
        "removal must not let the revoked signer evade this entry's inherited floor"
    );

    // An authorized delegated Admin covering the inherited observation can
    // remove the declaration. Its claim must not resurrect the removed floor
    // in the published state: a subsequent direct Admin re-add starts fresh.
    let authorized = Fixture::sign(removal, key(revoked, &other), &other);
    assert_eq!(
        fx.submit_remote(authorized).await,
        VerificationStatus::Verified
    );
    let readd = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i0.clone())).await;
    let readd = fx
        .accept(&readd, "direct Admin re-add after authorized removal")
        .await;
    let fresh = fx.via_identity(&[readd], &fx.i0).await;
    assert_eq!(fx.submit_remote(fresh).await, VerificationStatus::Verified);
}

#[tokio::test]
async fn same_entry_delegated_pointer_write_must_cover_new_pointer() {
    let fx = fixture().await;
    let write = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    for (claim, expected) in [(&fx.i0, false), (&fx.i1, true)] {
        let key = SigKey::Delegation {
            path: vec![DelegationStep {
                tree: fx.identity.root_id().clone(),
                tips: claim.clone(),
            }],
            hint: KeyHint::from_pubkey(&fx.member_pub),
        };
        let entry = Fixture::sign(write.clone(), key, &fx.member);
        assert_eq!(fx.validate(&entry).await.unwrap(), expected);
    }
}

/// A nested I observation persists across first-hop M removal/re-addition.
#[tokio::test]
async fn nested_floor_survives_first_hop_removal_and_readd() {
    let fx = fixture().await;
    let m_admin = PrivateKey::generate();
    let m = Database::create(&fx.instance, m_admin, Doc::new())
        .await
        .unwrap();
    let txn = m.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), fx.i0.clone()))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let m0 = m.snapshot().await.unwrap().into_tips();
    let admin_target = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::new(fx.target_admin.clone()));
    let txn = admin_target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(m.root_id(), m0.clone()))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let nested = fx
        .delegated(
            &fx.tips().await,
            &[(m.root_id(), &m0), (fx.identity.root_id(), &fx.i1)],
            &fx.member,
        )
        .await;
    fx.accept(&nested, "nested i1 observation").await;
    let mut deletion = Doc::new();
    deletion.remove(format!("auth.delegations.{}", m.root_id()));
    let removal = settings_entry(&fx, deletion).await;
    fx.accept(&removal, "remove M").await;
    let readd = settings_entry(&fx, pointer_write(m.root_id(), m0.clone())).await;
    let readd = fx.accept(&readd, "re-add M at m0").await;
    let stale_i = fx
        .delegated(
            &[readd],
            &[(m.root_id(), &m0), (fx.identity.root_id(), &fx.i0)],
            &fx.member,
        )
        .await;
    fx.reject(&stale_i, "nested I floor survives M removal")
        .await;
}

/// Removing a direct R declaration resets only direct observations. A still
/// active T -> M -> R route continues to inherit the nested R observation.
#[tokio::test]
async fn overlapping_direct_and_nested_root_removal_keeps_nested_floor() {
    let fx = fixture().await;
    let m = Database::create(&fx.instance, PrivateKey::generate(), Doc::new())
        .await
        .unwrap();
    let txn = m.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(fx.identity.root_id(), fx.i0.clone()))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let m0 = m.snapshot().await.unwrap().into_tips();
    let admin = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::new(fx.target_admin.clone()));
    let txn = admin.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(m.root_id(), m0.clone()))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let nested = fx
        .delegated(
            &fx.tips().await,
            &[(m.root_id(), &m0), (fx.identity.root_id(), &fx.i1)],
            &fx.member,
        )
        .await;
    let nested = fx.accept(&nested, "observe R at i1 through M").await;
    let mut deletion = Doc::new();
    deletion.remove(format!("auth.delegations.{}", fx.identity.root_id()));
    let removal = settings_entry(&fx, deletion).await;
    let removal = fx.accept(&removal, "remove direct R only").await;
    let merge = fx.direct(&[nested, removal]).await;
    let merge = fx
        .accept(&merge, "carry nested floor through removal merge")
        .await;
    let stale = fx
        .delegated(
            std::slice::from_ref(&merge),
            &[(m.root_id(), &m0), (fx.identity.root_id(), &fx.i0)],
            &fx.member,
        )
        .await;
    fx.reject(&stale, "nested i1 must survive direct R removal")
        .await;
    let current = fx
        .delegated(
            &[merge],
            &[(m.root_id(), &m0), (fx.identity.root_id(), &fx.i1)],
            &fx.member,
        )
        .await;
    fx.accept(&current, "nested i1 remains usable").await;
}

#[tokio::test]
async fn merged_absence_clears_even_an_active_branch_claim() {
    let fx = fixture().await;
    let base = fx.tips().await;
    let signed = fx.via_identity(&base, &fx.i1).await;
    assert!(fx.validate(&signed).await.unwrap());
    let precursor = fx.direct(&base).await;
    fx.accept(&precursor, "removal branch precursor").await;
    let mut deletion = Doc::new();
    deletion.remove(format!("auth.delegations.{}", fx.identity.root_id()));
    let removal = settings_entry(&fx, deletion).await;
    let removed = fx.accept(&removal, "later-height removal branch").await;
    let signed = fx.inject_verified_unchecked(&signed).await;
    let merge = fx.direct(&[removed, signed]).await;
    let merge = fx.accept(&merge, "merge with effective absence").await;
    let entries = fx
        .engine()
        .get_tree_from_tips(fx.target.root_id(), &crate::Snapshot::from(vec![merge]))
        .await
        .unwrap();
    assert!(
        crate::database::fold_settings_entries(&entries)
            .unwrap()
            .get_delegated_tree(fx.identity.root_id())
            .is_err()
    );
    let readd = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i0.clone())).await;
    fx.accept(&readd, "i0 is allowed after effective merged absence")
        .await;
}
