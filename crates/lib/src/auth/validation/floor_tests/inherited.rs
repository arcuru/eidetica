//! Inherited per-tree floors across parents, signers and delegation paths.

use super::*;

/// Equality: a child may pin exactly the snapshot its parent pinned.
#[tokio::test]
async fn floor_allows_equal_snapshot() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i0).await;
    let a = fx.accept(&a, "parent at i0").await;
    let c = fx.via_identity(&[a], &fx.i0).await;
    fx.accept(&c, "child at i0 under parent at i0").await;
}

/// Staleness is not regression: the verifier holding a newer identity
/// snapshot does not raise the floor. Only ancestry does.
#[tokio::test]
async fn floor_allows_stale_snapshot_when_ancestry_is_older() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i0).await;
    let a = fx.accept(&a, "parent at i0").await;
    // The verifier already knows i1 (the fixture advanced the identity tree),
    // and even a newer i2 — irrelevant to a branch whose ancestry says i0.
    let _i2 = advance(&fx.identity, "identity-v2").await;
    let c = fx.via_identity(&[a], &fx.i0).await;
    fx.accept(&c, "stale child at i0 with i2 known").await;
}

/// Regression below the parent's pinned snapshot is rejected, even though
/// the claimed snapshot sits at the committed pointer floor (i0).
#[tokio::test]
async fn floor_rejects_regression_below_parent() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i1).await;
    let a = fx.accept(&a, "parent at i1").await;
    let c = fx.via_identity(std::slice::from_ref(&a), &fx.i0).await;
    fx.reject(&c, "child at i0 under parent at i1").await;
    // Covering the floor again is fine.
    let ok = fx.via_identity(&[a], &fx.i1).await;
    fx.accept(&ok, "child at i1 under parent at i1").await;
}

/// The local transaction path cannot commit a descendant whose delegated
/// snapshot regresses below its parent.
#[tokio::test]
async fn local_commit_rejects_regression_below_parent() {
    let fx = fixture().await;
    let target = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::with_identity(
            fx.member.clone(),
            SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: fx.identity.root_id().clone(),
                    tips: fx.i1.clone(),
                }],
                hint: KeyHint::from_pubkey(&fx.member_pub),
            },
        ));
    let txn = target.new_transaction().await.unwrap();
    txn.get_store::<crate::store::DocStore>("data")
        .await
        .unwrap()
        .set("floor", "i1")
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let target = target.with_key(crate::database::DatabaseKey::with_identity(
        fx.member.clone(),
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: fx.identity.root_id().clone(),
                tips: fx.i0.clone(),
            }],
            hint: KeyHint::from_pubkey(&fx.member_pub),
        },
    ));
    let txn = target.new_transaction().await.unwrap();
    txn.get_store::<crate::store::DocStore>("data")
        .await
        .unwrap()
        .set("floor", "i0")
        .await
        .unwrap();
    let err = txn
        .commit()
        .await
        .expect_err("local regression must not commit");
    assert!(
        err.to_string().contains("validation failed"),
        "expected entry validation failure, got: {err}"
    );
}

/// Remote ingestion retains a floor regression but quarantines it as a
/// definitive failure rather than retrying it as missing history.
#[tokio::test]
async fn remote_ingest_marks_regression_failed() {
    let fx = fixture().await;
    let parent = fx.via_identity(&fx.tips().await, &fx.i1).await;
    let parent = fx.accept(&parent, "parent at i1").await;
    let child = fx.via_identity(&[parent], &fx.i0).await;
    assert_eq!(
        fx.submit_remote(child).await,
        VerificationStatus::Failed,
        "a proven regression must be terminal on remote ingest"
    );
}

/// The regression the floor exists to stop: a member removed at i1 cannot
/// sign below a parent that already acknowledged i1 by pinning i0, where the
/// member was still authorized.
#[tokio::test]
async fn floor_blocks_removed_member_below_acknowledged_removal() {
    let fx = fixture().await;
    // Revoke the member at a new snapshot i_rev, then have a *different*
    // authorized signer acknowledge it on T.
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
    let i_rev = fx.identity.snapshot().await.unwrap().into_tips();

    let root_tips = fx.tips().await;
    let b = fx
        .delegated(&root_tips, &[(fx.identity.root_id(), &i_rev)], &other)
        .await;
    let b = fx.accept(&b, "other signer at i_rev").await;

    // Sibling of b (not descending from it): the removed member may still use
    // i0 — its ancestry has not acknowledged the removal.
    let sibling = fx.via_identity(&root_tips, &fx.i0).await;
    fx.accept(&sibling, "removed member on a branch still at i0")
        .await;

    // Child of b: i0 is below the acknowledged floor; the removed member
    // cannot resurrect its authority there.
    let c = fx.via_identity(&[b], &fx.i0).await;
    fx.reject(&c, "removed member regressing below i_rev").await;
}

/// Siblings may pin different snapshots above their shared floor.
#[tokio::test]
async fn floor_allows_siblings_to_diverge() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i0).await;
    let a = fx.accept(&a, "parent at i0").await;
    let b = fx.via_identity(std::slice::from_ref(&a), &fx.i1).await;
    fx.accept(&b, "sibling at i1").await;
    let c = fx.via_identity(&[a], &fx.i0).await;
    fx.accept(&c, "sibling at i0").await;
}

/// A merge inherits every parent's floor: with concurrent identity tips
/// `ix` and `it` pinned by the two parents, the child must cover both.
#[tokio::test]
async fn floor_joins_all_parents_over_incomparable_frontiers() {
    let fx = fixture().await;
    // Fork the identity tree at i1 into two concurrent branches ix and it.
    let txn = fx
        .identity
        .new_transaction_at(&crate::Snapshot::from(fx.i1.clone()))
        .await
        .unwrap();
    txn.get_settings().unwrap().set_name("ix").await.unwrap();
    let ix = vec![txn.commit().await.unwrap()];
    let txn = fx
        .identity
        .new_transaction_at(&crate::Snapshot::from(fx.i1.clone()))
        .await
        .unwrap();
    txn.get_settings().unwrap().set_name("it").await.unwrap();
    let it = vec![txn.commit().await.unwrap()];
    assert_ne!(ix, it);

    let root_tips = fx.tips().await;
    let p = fx.via_identity(&root_tips, &ix).await;
    let p = fx.accept(&p, "parent p at ix").await;
    let q = fx.via_identity(&root_tips, &it).await;
    let q = fx.accept(&q, "parent q at it").await;
    let parents = [p, q];

    let only_x = fx.via_identity(&parents, &ix).await;
    fx.reject(&only_x, "merge covering only ix").await;
    let only_y = fx.via_identity(&parents, &it).await;
    fx.reject(&only_y, "merge covering only it").await;

    // Both frontiers named explicitly.
    let both: Vec<ID> = ix.iter().chain(it.iter()).cloned().collect();
    let both_entry = fx.via_identity(&parents, &both).await;
    fx.accept(&both_entry, "merge naming both frontiers").await;

    // Or an identity-side merge that covers both.
    let is = fx.identity.snapshot().await.unwrap().into_tips();
    assert_eq!(
        as_set(&is),
        as_set(&both),
        "identity frontier is {{ix, it}}"
    );
    let txn = fx.identity.new_transaction().await.unwrap();
    txn.get_settings().unwrap().set_name("is").await.unwrap();
    let is = vec![txn.commit().await.unwrap()];
    let merged = fx.via_identity(&parents, &is).await;
    fx.accept(&merged, "merge at an identity snapshot covering both")
        .await;
}

/// A direct-key entry in between does not reset the identity's floor.
#[tokio::test]
async fn floor_carries_through_direct_key_intermediate() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i1).await;
    let a = fx.accept(&a, "a at i1").await;
    let b = fx.direct(&[a]).await;
    let b = fx.accept(&b, "b signed directly").await;
    let c = fx.via_identity(std::slice::from_ref(&b), &fx.i0).await;
    fx.reject(&c, "c at i0 behind a direct-key b").await;
    let ok = fx.via_identity(&[b], &fx.i1).await;
    fx.accept(&ok, "c at i1 behind a direct-key b").await;
}

/// An entry signed through a *different* identity tree in between does not
/// reset the floor either; floors are keyed per delegated tree.
#[tokio::test]
async fn floor_carries_through_other_identity_intermediate() {
    let fx = fixture().await;
    // A second identity tree J with its own member, delegated by T.
    let j_member = PrivateKey::generate();
    let j_admin = PrivateKey::generate();
    let (j, j0) = identity_tree(
        &fx.instance,
        &j_admin,
        &j_member.public_key(),
        Permission::Admin(5),
    )
    .await;
    let admin_target = Database::open(&fx.instance, fx.target.root_id())
        .await
        .unwrap()
        .with_key(crate::database::DatabaseKey::new(fx.target_admin.clone()));
    let txn = admin_target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(j.root_id(), j0.clone()))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let a = fx.via_identity(&fx.tips().await, &fx.i1).await;
    let a = fx.accept(&a, "a via I at i1").await;
    let b = fx.delegated(&[a], &[(j.root_id(), &j0)], &j_member).await;
    let b = fx.accept(&b, "b via J").await;
    let c = fx.via_identity(std::slice::from_ref(&b), &fx.i0).await;
    fx.reject(&c, "c via I at i0 behind b via J").await;
    let ok = fx.via_identity(&[b], &fx.i1).await;
    fx.accept(&ok, "c via I at i1 behind b via J").await;
}

/// Missing history for an unrelated identity used by an intermediate entry
/// must not make the floor for this identity indeterminate. The ancestor's
/// path names the delegated roots explicitly; resolving I does not require
/// materializing J's claimed snapshot.
#[tokio::test]
async fn floor_ignores_missing_other_identity_snapshot() {
    let fx = fixture().await;
    let j_member = PrivateKey::generate();
    let a = fx.via_identity(&fx.tips().await, &fx.i1).await;
    let a = fx.accept(&a, "a via I at i1").await;
    // A verified ancestor from an older peer may name J by root while the
    // local node no longer holds J's claimed tip. Its signature is irrelevant
    // to the floor for I, which must carry through to c.
    let missing_j = ID::from_bytes("missing-j-root");
    let missing_j_tip = ID::from_bytes("missing-j-tip");
    let synthetic_b = fx
        .delegated(&[a], &[(&missing_j, &[missing_j_tip])], &j_member)
        .await;
    let synthetic_b = fx.inject_verified_unchecked(&synthetic_b).await;
    let c = fx.via_identity(&[synthetic_b], &fx.i1).await;
    assert!(
        fx.validate(&c).await.unwrap(),
        "missing J history must not block I's inherited floor"
    );
}

/// Changing the signing key does not erase the floor for the same identity.
#[tokio::test]
async fn floor_is_keyed_by_tree_not_signer() {
    let fx = fixture().await;
    let second = PrivateKey::generate();
    let txn = fx.identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &second.public_key(),
            AuthKey::active(Some("second"), Permission::Admin(5)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let i2 = fx.identity.snapshot().await.unwrap().into_tips();

    let a = fx
        .delegated(&fx.tips().await, &[(fx.identity.root_id(), &i2)], &second)
        .await;
    let a = fx.accept(&a, "a signed by second at i2").await;
    // The original member, at i1 (< i2), on the same identity.
    let c = fx.via_identity(std::slice::from_ref(&a), &fx.i1).await;
    fx.reject(&c, "member regressing to i1 under second's i2")
        .await;
    let ok = fx.via_identity(&[a], &i2).await;
    fx.accept(&ok, "member at i2 under second's i2").await;
}

/// A claimed tip from another tree is still rejected with floors present.
#[tokio::test]
async fn floor_check_still_rejects_wrong_tree_tips() {
    let fx = fixture().await;
    let a = fx.via_identity(&fx.tips().await, &fx.i0).await;
    let a = fx.accept(&a, "a at i0").await;
    let foreign = vec![fx.target.root_id().clone()];
    let c = fx.via_identity(&[a], &foreign).await;
    fx.reject(&c, "tips from the target tree itself").await;
}

/// Nested delegation (T → M → I): floors are tracked per delegated tree for
/// every step of the path.
#[tokio::test]
async fn floor_applies_to_every_step_of_a_nested_path() {
    let fx = fixture().await;
    // Middle tree M delegates to I; T delegates to M.
    let m_admin = PrivateKey::generate();
    let m = Database::create(&fx.instance, m_admin.clone(), Doc::new())
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
    let m1 = advance(&m, "m-v1").await;

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

    let i_root = fx.identity.root_id();
    let a = fx
        .delegated(
            &fx.tips().await,
            &[(m.root_id(), &m1), (i_root, &fx.i1)],
            &fx.member,
        )
        .await;
    let a = fx.accept(&a, "a at (m1, i1)").await;

    let regress_i = fx
        .delegated(
            std::slice::from_ref(&a),
            &[(m.root_id(), &m1), (i_root, &fx.i0)],
            &fx.member,
        )
        .await;
    fx.reject(&regress_i, "inner step regressing i1 → i0").await;

    let regress_m = fx
        .delegated(
            std::slice::from_ref(&a),
            &[(m.root_id(), &m0), (i_root, &fx.i1)],
            &fx.member,
        )
        .await;
    fx.reject(&regress_m, "middle step regressing m1 → m0")
        .await;

    let ok = fx
        .delegated(&[a], &[(m.root_id(), &m1), (i_root, &fx.i1)], &fx.member)
        .await;
    fx.accept(&ok, "child at (m1, i1)").await;
}

/// A direct-key chain does not erase a root's cached derived frontier.
#[tokio::test]
async fn floor_uses_nearest_same_tree_ancestor_on_each_path() {
    let fx = fixture().await;
    let root_tips = fx.tips().await;
    let a = fx.via_identity(&root_tips, &fx.i1).await;
    let a = fx.accept(&a, "a at i1").await;
    // Long direct-key run after a.
    let mut tip = a;
    for _ in 0..5 {
        let d = fx.direct(&[tip]).await;
        tip = fx.accept(&d, "direct run").await;
    }
    let c = fx.via_identity(&[tip], &fx.i0).await;
    fx.reject(&c, "regression across a long direct-key run")
        .await;
}
