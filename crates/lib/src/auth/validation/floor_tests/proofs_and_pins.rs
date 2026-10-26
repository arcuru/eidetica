//! Verified-history proofs, retry/cache recovery and authenticated settings pins.

use super::super::{delegation::DelegationResolver, floors::DerivedFloors};
use super::*;

/// A remote entry cannot drop an authenticated parent's pin by omitting
/// metadata, even if it does not modify `_settings` itself.
#[tokio::test]
async fn empty_pin_cannot_promote_unsigned_child() {
    let fx = fixture().await;
    let parents = fx.tips().await;
    let entry = Entry::builder(fx.target.root_id().clone())
        .set_height(fx.data_entry(&parents).await.height())
        .set_parents(parents)
        .set_subtree_data("data", b"unsigned")
        .build()
        .unwrap();
    assert_eq!(fx.submit_remote(entry).await, VerificationStatus::Failed);
}

/// An actually unconfigured tree still permits unsigned entries with no pin.
#[tokio::test]
async fn empty_pin_allows_unsigned_child_without_settings() {
    let instance = new_instance().await;
    let backend = instance.require_local_engine().unwrap();
    let root = Entry::root_builder().build().unwrap();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    backend
        .update_verification_status(&root_id, VerificationStatus::Verified)
        .await
        .unwrap();
    let child = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(1)
        .build()
        .unwrap();
    let child_id = child.id();
    instance
        .put_remote_entries(&root_id, vec![child])
        .await
        .unwrap();
    assert_eq!(
        backend.get_verification_status(&child_id).await.unwrap(),
        VerificationStatus::Verified
    );
}

/// A Verified foreign main parent cannot provide a shortcut around the
/// target tree's authenticated history.
#[tokio::test]
async fn foreign_main_parent_cannot_promote_unsigned_child() {
    let fx = fixture().await;
    let parents = [fx.tips().await, fx.i0.clone()].concat();
    let entry = Entry::builder(fx.target.root_id().clone())
        .set_height(fx.data_entry(&parents).await.height())
        .set_parents(parents)
        .set_subtree_data("data", b"unsigned foreign-parent")
        .build()
        .unwrap();
    assert_eq!(fx.submit_remote(entry).await, VerificationStatus::Failed);
}

/// An empty configured pointer on the second hop cannot authorize a claim,
/// even though the claim itself names a complete, verified identity snapshot.
#[tokio::test]
async fn nested_empty_configured_pointer_is_invalid() {
    let fx = fixture().await;
    let m = Database::create(&fx.instance, PrivateKey::generate(), Doc::new())
        .await
        .unwrap();
    let mut doc = Doc::new();
    doc.set(
        format!("auth.delegations.{}", fx.identity.root_id()),
        delegation_ref(fx.identity.root_id(), vec![]),
    );
    let parent = m.snapshot().await.unwrap().into_tips();
    let settings_parents = fx
        .engine()
        .store_snapshot_at(
            m.root_id(),
            SETTINGS,
            &crate::Snapshot::from(parent.clone()),
        )
        .await
        .unwrap()
        .into_tips();
    // Simulate an older, already-Verified middle tree whose signed settings
    // contain an empty pointer. This tests resolution rather than validating
    // the middle tree's own new pointer write.
    let entry = Entry::builder(m.root_id().clone())
        .set_parents(parent)
        .set_subtree_parents(SETTINGS, settings_parents)
        .set_subtree_data(SETTINGS, serde_json::to_vec(&doc).unwrap())
        .set_height(10)
        .build()
        .unwrap();
    let middle = entry.id();
    fx.engine().put(entry).await.unwrap();
    fx.engine()
        .update_verification_status(&middle, VerificationStatus::Verified)
        .await
        .unwrap();
    let admin = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::new(fx.target_admin.clone()));
    let txn = admin.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(m.root_id(), vec![middle.clone()]))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let attempt = fx
        .delegated(
            &fx.tips().await,
            &[(m.root_id(), &[middle]), (fx.identity.root_id(), &fx.i0)],
            &fx.member,
        )
        .await;
    let (_, auth) = fx.pinned_settings().await;
    let mut resolver = DelegationResolver::new();
    let SigKey::Delegation { path, hint } = &attempt.auth().key else {
        panic!("expected delegation")
    };
    let result = resolver
        .resolve_delegation_path(path, hint, &auth, &fx.instance)
        .await;
    assert!(matches!(result, Err(Error::Auth(e))
        if matches!(&*e, AuthError::InvalidDelegationTips { claimed_tips, .. } if claimed_tips.is_empty())));
    fx.reject(&attempt, "empty nested pointer cannot authorize a claim")
        .await;
}

/// The floor cannot be established from partial history: an ancestor missing
/// locally surfaces as the retriable `DelegatedTreeUnsynced`, never as a
/// verdict either way.
#[tokio::test]
async fn floor_with_missing_ancestor_is_retriable_not_a_verdict() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i1).await;
    // Deliberately *not* stored: a's ID is a dangling parent.
    let c = fx.via_identity(&[a.id()], &fx.i0).await;
    let err = fx
        .validate(&c)
        .await
        .expect_err("missing ancestor must not produce a verdict");
    match err {
        Error::Auth(e) => match *e {
            AuthError::DelegatedTreeUnsynced { tree_id, missing } => {
                assert_eq!(&tree_id, fx.target.root_id());
                assert_eq!(missing, vec![a.id()]);
            }
            other => panic!("expected DelegatedTreeUnsynced, got {other:?}"),
        },
        other => panic!("expected Auth error, got {other:?}"),
    }
}

/// Signed settings edges must represent the main-parent causal frontier.
/// The resulting floor must not be inferred by folding main ancestry when
/// the actual settings store would see a different DAG.
#[tokio::test]
async fn settings_parent_frontier_omission_and_extraneous_edge_fail_remote() {
    let fx = fixture().await;
    let base = fx.tips().await;
    let write = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    let raw = write.data(SETTINGS).unwrap().clone();
    let metadata = write.metadata().unwrap().to_vec();
    let height = write.height();
    let make = |parents: Vec<ID>, subtree_parents: Vec<ID>| {
        let e = Entry::builder(fx.target.root_id().clone())
            .set_parents(parents)
            .set_subtree_data(SETTINGS, raw.clone())
            .set_subtree_parents(SETTINGS, subtree_parents)
            .set_metadata(metadata.clone())
            .set_height(height + 1)
            .build()
            .unwrap();
        Fixture::sign(
            e,
            SigKey::from_pubkey(&fx.target_admin.public_key()),
            &fx.target_admin,
        )
    };
    let omitted = make(base.clone(), vec![]);
    assert_eq!(
        fx.submit_remote(omitted.clone()).await,
        VerificationStatus::Failed
    );
    assert!(
        !fx.target
            .snapshot()
            .await
            .unwrap()
            .tips()
            .contains(&omitted.id())
    );

    // A concurrent settings write exists locally but is not a main ancestor
    // of the entry whose settings parents forge a link to it.
    let sibling = settings_entry(&fx, Doc::new()).await;
    let sibling = fx.accept(&sibling, "valid settings sibling").await;
    let extraneous = make(base, vec![sibling]);
    assert_eq!(
        fx.submit_remote(extraneous.clone()).await,
        VerificationStatus::Failed
    );
    assert!(
        !fx.target
            .snapshot()
            .await
            .unwrap()
            .tips()
            .contains(&extraneous.id())
    );
}

/// A present but not locally Verified dependency is not a proof, even when
/// all of its bytes and its main-tree ancestry are already held locally.
#[tokio::test]
async fn delegated_present_unverified_tip_is_retryable() {
    let fx = fixture().await;
    let tip = &fx.i1[0];
    fx.engine()
        .update_verification_status(tip, VerificationStatus::Unverified)
        .await
        .unwrap();
    let entry = fx.via_identity(&fx.tips().await, &fx.i1).await;
    assert!(
        matches!(fx.validate(&entry).await, Err(Error::Auth(e)) if e.is_delegated_tree_unsynced())
    );
    let pointer = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    assert!(
        matches!(fx.validate(&pointer).await, Err(Error::Auth(e)) if e.is_delegated_tree_unsynced())
    );
    fx.engine()
        .update_verification_status(tip, VerificationStatus::Verified)
        .await
        .unwrap();
    fx.accept(&entry, "explicit retry after verifying dependency")
        .await;
}

/// A current-tip cache and a Verified tip are not an ancestry proof. Even
/// inconsistent low-level status labels cannot hide an unsettled ancestor
/// from delegated validation (normal demotion cascades to descendants).
#[tokio::test]
async fn delegated_verified_tip_requires_every_ancestor_verified() {
    for status in [VerificationStatus::Unverified, VerificationStatus::Failed] {
        let fx = fixture().await;
        let latest = advance(&fx.identity, "identity-v2").await;
        let claim = fx.via_identity(&fx.tips().await, &latest).await;
        let ancestor = &fx.i1[0];
        fx.engine()
            .update_verification_status(ancestor, status)
            .await
            .unwrap();
        assert_eq!(
            fx.engine()
                .get_verification_status(&latest[0])
                .await
                .unwrap(),
            VerificationStatus::Unverified,
            "demotion must invalidate dependent labels"
        );
        // A missing Verified proof is never accepted, even if a cached projection exists.
        assert!(!matches!(fx.validate(&claim).await, Ok(true)));
        if status == VerificationStatus::Unverified {
            fx.identity.verify().await.unwrap();
            fx.accept(&claim, "reverification restores complete ancestry")
                .await;
        } else {
            fx.identity.verify().await.unwrap();
            assert_eq!(
                fx.engine()
                    .get_verification_status(&latest[0])
                    .await
                    .unwrap(),
                VerificationStatus::Failed
            );
            fx.reject(&claim, "Failed ancestor remains terminal").await;
        }
    }
}

/// Clearing a disposable projection must reconstruct a Verified parent's
/// state, never supply an empty floor for its descendant.
#[tokio::test]
async fn verified_parent_cache_miss_rebuilds_floor() {
    let fx = fixture().await;
    let parent = fx.via_identity(&fx.tips().await, &fx.i1).await;
    let parent = fx.accept(&parent, "advanced parent").await;
    fx.engine().clear_derived_store_state().await.unwrap();
    let child = fx.via_identity(&[parent], &fx.i0).await;
    fx.reject(&child, "cold Verified parent still pins i1")
        .await;
}

#[tokio::test]
async fn published_projection_cannot_promote_unverified_parent() {
    let fx = fixture().await;
    let parent = fx.via_identity(&fx.tips().await, &fx.i1).await;
    assert!(
        fx.validate(&parent).await.unwrap(),
        "validation publishes projection"
    );
    let parent_id = parent.id();
    // Build before storing: fixture snapshot reads auto-verify raw tips.
    let child = fx
        .via_identity(std::slice::from_ref(&parent_id), &fx.i0)
        .await;
    fx.engine().put(parent).await.unwrap();
    assert_eq!(
        fx.engine()
            .get_verification_status(&parent_id)
            .await
            .unwrap(),
        VerificationStatus::Unverified
    );
    let mut projection = DerivedFloors::for_entry(fx.engine(), &child);
    assert!(
        matches!(projection.prepare(&child).await, Err(Error::Auth(e))
        if e.is_delegated_tree_unsynced())
    );
    fx.engine()
        .update_verification_status(&parent_id, VerificationStatus::Verified)
        .await
        .unwrap();
    projection.prepare(&child).await.unwrap();
    assert_eq!(
        projection.floor_for(fx.identity.root_id()),
        crate::Snapshot::from(fx.i1)
    );
}

// The metadata is signed but untrusted: a data-store tip cannot erase the
// authenticated settings carried by its main parents.
#[tokio::test]
async fn non_settings_pin_cannot_promote_unsigned_child() {
    let fx = fixture().await;
    let parent = fx.direct(&fx.tips().await).await;
    let parent_id = fx.accept(&parent, "data parent").await;
    let child = fx
        .data_entry_with_pin(
            std::slice::from_ref(&parent_id),
            std::slice::from_ref(&parent_id),
        )
        .await;
    assert_eq!(fx.submit_remote(child).await, VerificationStatus::Failed);
}

#[tokio::test]
async fn revoked_signer_cannot_pin_old_settings_below_revocation() {
    let fx = fixture().await;
    let signer = PrivateKey::generate();
    let txn = fx.target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &signer.public_key(),
            AuthKey::active(Some("temporary"), Permission::Write(5)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let (old_pin, _) = fx.pinned_settings().await;
    let before = fx.tips().await;
    let key = SigKey::from_pubkey(&signer.public_key());
    let sibling = Fixture::sign(
        fx.data_entry_with_pin(&before, &old_pin).await,
        key.clone(),
        &signer,
    );

    let txn = fx.target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&signer.public_key())
        .await
        .unwrap();
    let revocation = txn.commit().await.unwrap();
    // Sibling's parents predate revocation even though the verifier has seen it.
    assert_eq!(
        fx.submit_remote(sibling).await,
        VerificationStatus::Verified
    );
    let child = Fixture::sign(
        fx.data_entry_with_pin(&[revocation], &old_pin).await,
        key,
        &signer,
    );
    assert_eq!(fx.submit_remote(child).await, VerificationStatus::Failed);
}

#[tokio::test]
async fn missing_main_ancestor_defers_forged_pin_decision_until_retry() {
    let fx = fixture().await;
    let missing_parent = fx.direct(&fx.tips().await).await;
    let child = fx
        .data_entry_with_pin(&[missing_parent.id()], &[missing_parent.id()])
        .await;
    // Keep the forged-pin verdict independent of the height rule even though
    // this parent is deliberately not stored until after the first attempt.
    let child = Entry::builder(fx.target.root_id().clone())
        .add_parent(missing_parent.id())
        .set_height(missing_parent.height() + 1)
        .set_subtree_data("data", child.data("data").unwrap().clone())
        .set_metadata(child.metadata().unwrap().clone())
        .build()
        .unwrap();
    let child_id = child.id();
    assert_eq!(
        fx.submit_remote(child).await,
        VerificationStatus::Unverified
    );
    assert_eq!(
        fx.submit_remote(missing_parent).await,
        VerificationStatus::Verified
    );
    fx.target.verify().await.unwrap();
    assert_eq!(
        fx.engine()
            .get_verification_status(&child_id)
            .await
            .unwrap(),
        VerificationStatus::Failed
    );
}

#[tokio::test]
async fn settings_write_pins_pre_write_frontier_not_own_tip() {
    let fx = fixture().await;
    let (prior, _) = fx.pinned_settings().await;
    // A signed settings write against the authentic pre-write pin is valid.
    let subtree_parents = prior.clone();
    let mut settings = Doc::new();
    settings.set("name", "new");
    let data = serde_json::to_vec(&settings).unwrap();
    // Build the two variants with identical causal main parents.
    let parents = fx.tips().await;
    let height = fx.data_entry(&parents).await.height();
    let metadata = |tips: &[ID]| {
        serde_json::to_vec(&serde_json::json!({"settings_tips": tips, "entropy": null})).unwrap()
    };
    let valid = Fixture::sign(
        Entry::builder(fx.target.root_id().clone())
            .set_height(height)
            .set_parents(parents.clone())
            .set_subtree_data(SETTINGS, data.clone())
            .set_subtree_parents(SETTINGS, subtree_parents.clone())
            .set_metadata(metadata(&prior))
            .build()
            .unwrap(),
        SigKey::from_pubkey(&fx.target_admin.public_key()),
        &fx.target_admin,
    );
    assert_eq!(fx.submit_remote(valid).await, VerificationStatus::Verified);
    // Pinning the as-yet-uncommitted settings write itself must not grant it authority.
    let own = Entry::builder(fx.target.root_id().clone())
        .set_height(height)
        .set_parents(parents)
        .set_subtree_data(SETTINGS, data)
        .set_subtree_parents(SETTINGS, subtree_parents)
        .set_metadata(metadata(&[]))
        .build()
        .unwrap();
    let child = Fixture::sign(
        own,
        SigKey::from_pubkey(&fx.target_admin.public_key()),
        &fx.target_admin,
    );
    assert_eq!(fx.submit_remote(child).await, VerificationStatus::Failed);
}

#[tokio::test]
async fn non_genesis_settings_write_cannot_bootstrap_its_own_signer() {
    let instance = new_instance().await;
    let backend = instance.require_local_engine().unwrap();
    let root = Entry::root_builder().build().unwrap();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    backend
        .update_verification_status(&root_id, VerificationStatus::Verified)
        .await
        .unwrap();
    let signer = PrivateKey::generate();
    let mut auth = AuthSettings::new();
    auth.add_key(
        &signer.public_key(),
        AuthKey::active(Some("new"), Permission::Admin(0)),
    )
    .unwrap();
    let mut settings = Doc::new();
    settings.set("auth", auth.as_doc().clone());
    let entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(1)
        .set_subtree_data(SETTINGS, serde_json::to_vec(&settings).unwrap())
        .set_metadata(
            serde_json::to_vec(&serde_json::json!({"settings_tips": [], "entropy": null})).unwrap(),
        )
        .build()
        .unwrap();
    let entry = Fixture::sign(entry, SigKey::from_pubkey(&signer.public_key()), &signer);
    let id = entry.id();
    instance
        .put_remote_entries(&root_id, vec![entry])
        .await
        .unwrap();
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Failed
    );
}
