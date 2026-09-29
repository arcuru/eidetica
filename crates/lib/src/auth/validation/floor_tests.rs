//! Tests for delegated-auth snapshot floors: transitive per-entry
//! non-regression of pinned delegated-tree snapshots, and forward-only
//! committed delegation pointers.
//!
//! Entries are built by hand (parents, claimed tips, signer) and validated
//! through `AuthValidator::validate_entry` against the target tree's pinned
//! settings, exactly as `Database::verify` does — the validator is the shared
//! seam for local commits and remote ingest.

use std::collections::HashSet;

use super::entry::AuthValidator;
use crate::{
    Database, Entry, Error, Instance, Result,
    auth::{
        crypto::{PrivateKey, PublicKey, sign_entry},
        errors::AuthError,
        settings::AuthSettings,
        types::{
            AuthInfo, AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission,
            PermissionBounds, SigKey, TreeReference,
        },
    },
    backend::{BackendImpl, VerificationStatus},
    constants::SETTINGS,
    crdt::Doc,
    entry::ID,
};

/// A target tree `T` delegating to an identity tree `I`.
///
/// `i0` and `i1` are successive snapshots of `I`; `member` is authorized at
/// both (so a rejection is a floor verdict, never a permission one) unless a
/// test says otherwise. `T` commits the delegation pointer at `i0`.
struct Fixture {
    instance: Instance,
    identity: Database,
    member: PrivateKey,
    member_pub: PublicKey,
    target: Database,
    target_admin: PrivateKey,
    i0: Vec<ID>,
    i1: Vec<ID>,
    nonce: std::cell::Cell<u64>,
}

async fn new_instance() -> Instance {
    use crate::backend::database::InMemory;
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .expect("test instance");
    instance
}

/// Create a tree owned by `admin` with `member` authorized as `member_perm`,
/// returning the tree and its snapshot after the membership write.
async fn identity_tree(
    instance: &Instance,
    admin: &PrivateKey,
    member_pub: &PublicKey,
    member_perm: Permission,
) -> (Database, Vec<ID>) {
    let tree = Database::create(instance, admin.clone(), Doc::new())
        .await
        .unwrap();
    let txn = tree.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(member_pub, AuthKey::active(Some("member"), member_perm))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let snap = tree.snapshot().await.unwrap().into_tips();
    (tree, snap)
}

/// Advance `tree` with an unrelated settings write; returns the new snapshot.
async fn advance(tree: &Database, name: &str) -> Vec<ID> {
    let txn = tree.new_transaction().await.unwrap();
    txn.get_settings().unwrap().set_name(name).await.unwrap();
    txn.commit().await.unwrap();
    tree.snapshot().await.unwrap().into_tips()
}

fn delegation_ref(root: &ID, tips: Vec<ID>) -> DelegatedTreeRef {
    DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            max: Permission::Admin(10),
            min: Some(Permission::Read),
        },
        tree: TreeReference {
            root: root.clone(),
            tips,
        },
    }
}

async fn fixture() -> Fixture {
    let instance = new_instance().await;
    let identity_admin = instance.signing_key().unwrap().clone();
    let member = PrivateKey::generate();
    let member_pub = member.public_key();
    let (identity, i0) = identity_tree(
        &instance,
        &identity_admin,
        &member_pub,
        Permission::Admin(5),
    )
    .await;
    let i1 = advance(&identity, "identity-v1").await;
    assert_ne!(i0, i1);

    let target_admin = PrivateKey::generate();
    let target = Database::create(&instance, target_admin.clone(), Doc::new())
        .await
        .unwrap();
    let txn = target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref(identity.root_id(), i0.clone()))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    Fixture {
        instance,
        identity,
        member,
        member_pub,
        target,
        target_admin,
        i0,
        i1,
        nonce: std::cell::Cell::new(0),
    }
}

impl Fixture {
    fn engine(&self) -> std::sync::Arc<dyn BackendImpl> {
        self.instance.require_local_engine().unwrap()
    }

    /// Verified tips of `T`.
    async fn tips(&self) -> Vec<ID> {
        self.target.snapshot().await.unwrap().into_tips()
    }

    /// The `_settings` state a new entry of `T` pins and validates against.
    async fn pinned_settings(&self) -> (Vec<ID>, AuthSettings) {
        let snapshot = self.target.snapshot().await.unwrap();
        let settings_tips = self
            .engine()
            .store_snapshot_at(self.target.root_id(), SETTINGS, &snapshot)
            .await
            .unwrap()
            .into_tips();
        let auth = self
            .target
            .get_settings()
            .await
            .unwrap()
            .auth_snapshot()
            .await
            .unwrap();
        (settings_tips, auth)
    }

    /// Build an unsigned data entry of `T` on `parents`, pinned to the
    /// current `_settings` state, carrying a unique payload.
    async fn data_entry(&self, parents: &[ID]) -> Entry {
        let (settings_tips, _) = self.pinned_settings().await;
        let mut height = 0u64;
        for p in parents {
            if let Ok(parent) = self.engine().get(p).await {
                height = height.max(parent.height() + 1);
            }
        }
        let n = self.nonce.get();
        self.nonce.set(n + 1);
        let metadata = serde_json::to_vec(&serde_json::json!({
            "settings_tips": settings_tips,
            "entropy": serde_json::Value::Null,
        }))
        .unwrap();
        Entry::builder(self.target.root_id().clone())
            .set_parents(parents.to_vec())
            .set_subtree_data("data", format!("{{\"n\":{n}}}").into_bytes())
            .set_metadata(metadata)
            .set_height(height)
            .build()
            .unwrap()
    }

    fn sign(entry: Entry, key: SigKey, signer: &PrivateKey) -> Entry {
        let entry = entry.with_auth(|auth| auth.key = key);
        let signature = sign_entry(&entry, signer).unwrap();
        entry.with_auth(|auth| auth.signature = Some(signature))
    }

    /// A data entry signed by `signer` through a delegation path.
    async fn delegated(
        &self,
        parents: &[ID],
        steps: &[(&ID, &[ID])],
        signer: &PrivateKey,
    ) -> Entry {
        let key = SigKey::Delegation {
            path: steps
                .iter()
                .map(|(root, tips)| DelegationStep {
                    tree: (*root).clone(),
                    tips: tips.to_vec(),
                })
                .collect(),
            hint: KeyHint::from_pubkey(&signer.public_key()),
        };
        Self::sign(self.data_entry(parents).await, key, signer)
    }

    /// A data entry signed through the single `I` delegation by `member`.
    async fn via_identity(&self, parents: &[ID], tips: &[ID]) -> Entry {
        self.delegated(parents, &[(self.identity.root_id(), tips)], &self.member)
            .await
    }

    /// A data entry signed directly by `T`'s admin key.
    async fn direct(&self, parents: &[ID]) -> Entry {
        let key = SigKey::from_pubkey(&self.target_admin.public_key());
        Self::sign(self.data_entry(parents).await, key, &self.target_admin)
    }

    async fn validate(&self, entry: &Entry) -> Result<bool> {
        let (_, auth) = self.pinned_settings().await;
        AuthValidator::new()
            .validate_entry(entry, &auth, Some(&self.instance))
            .await
    }

    /// Store `entry` as Verified, the state a parent is in when its child is
    /// validated. Returns its ID.
    async fn store(&self, entry: &Entry) -> ID {
        let id = entry.id();
        let engine = self.engine();
        engine.put(entry.clone()).await.unwrap();
        engine
            .update_verification_status(&id, VerificationStatus::Verified)
            .await
            .unwrap();
        id
    }

    /// Validate then store, asserting validity.
    async fn accept(&self, entry: &Entry, what: &str) -> ID {
        assert!(self.validate(entry).await.unwrap(), "{what} must validate");
        self.store(entry).await
    }

    async fn reject(&self, entry: &Entry, what: &str) {
        assert!(
            !self.validate(entry).await.unwrap(),
            "{what} must be rejected"
        );
    }

    async fn submit_remote(&self, entry: Entry) -> VerificationStatus {
        let id = entry.id();
        self.instance
            .put_remote_entries(self.target.root_id(), vec![entry])
            .await
            .unwrap();
        self.engine().get_verification_status(&id).await.unwrap()
    }
}

fn as_set(ids: &[ID]) -> HashSet<ID> {
    ids.iter().cloned().collect()
}

// ===== Inherited floor =====

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
    let synthetic_b = fx.store(&synthetic_b).await;
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

/// The floor walk does not stop at an ancestor that names a *different*
/// delegated tree with a snapshot pinned even further back on this one.
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

// ===== Forward-only committed pointer =====

/// Build a `_settings` entry of `T` (signed by its admin) whose raw data is
/// `settings_doc`, pinned to the current settings state.
async fn settings_entry(fx: &Fixture, settings_doc: Doc) -> Entry {
    let (settings_tips, _) = fx.pinned_settings().await;
    let parents = fx.tips().await;
    let mut height = 0u64;
    for p in &parents {
        height = height.max(fx.engine().get(p).await.unwrap().height() + 1);
    }
    let metadata = serde_json::to_vec(&serde_json::json!({
        "settings_tips": settings_tips,
        "entropy": serde_json::Value::Null,
    }))
    .unwrap();
    let entry = Entry::builder(fx.target.root_id().clone())
        .set_parents(parents)
        .set_subtree_data(SETTINGS, serde_json::to_vec(&settings_doc).unwrap())
        .set_subtree_parents(SETTINGS, settings_tips)
        .set_metadata(metadata)
        .set_height(height)
        .set_auth(AuthInfo {
            key: SigKey::from_pubkey(&fx.target_admin.public_key()),
            signature: None,
        })
        .build()
        .unwrap();
    let signature = sign_entry(&entry, &fx.target_admin).unwrap();
    entry.with_auth(|auth| auth.signature = Some(signature))
}

fn pointer_write(root: &ID, tips: Vec<ID>) -> Doc {
    let mut doc = Doc::new();
    doc.set(
        format!("auth.delegations.{root}"),
        delegation_ref(root, tips),
    );
    doc
}

/// The convenience API commit path: moving the pointer backwards fails the
/// transaction; forward and equal succeed.
#[tokio::test]
async fn pointer_add_delegated_tree_must_move_forward() {
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
    let err = txn
        .commit()
        .await
        .expect_err("moving the pointer backwards must not commit");
    assert!(
        err.to_string().contains("validation failed"),
        "expected entry validation failure, got: {err}"
    );
    let committed = admin_target
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap()
        .get_delegated_tree(fx.identity.root_id())
        .unwrap();
    assert_eq!(as_set(&committed.tree.tips), as_set(&fx.i1));
}

/// A raw `_settings` write (bypassing `add_delegated_tree`) is gated the
/// same way — this is the seam remote ingest also goes through.
#[tokio::test]
async fn pointer_raw_settings_write_must_move_forward() {
    let fx = fixture().await;
    // Advance to i1 first.
    let forward = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&forward, "raw write advancing i0 → i1").await;

    let backwards = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i0.clone())).await;
    fx.reject(&backwards, "raw write regressing i1 → i0").await;

    let equal = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&equal, "raw write re-committing i1").await;
}

/// A partial raw write that sets only `tree.tips` under the delegation key
/// is still a pointer move and is gated.
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
    fx.reject(&entry, "partial tips-only write regressing to i0")
        .await;
}

/// Re-spelling the settings key does not reset the pointer: the floor is
/// matched by the delegated tree's root inside the reference.
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
    fx.reject(&entry, "same root under a new key regressing to i0")
        .await;
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

/// Adding a delegation for the first time has no prior pointer to cover, and
/// touching only the bounds is not a pointer move: neither needs the
/// delegated tree's history locally.
#[tokio::test]
async fn pointer_first_declaration_and_bounds_only_writes_pass() {
    let fx = fixture().await;
    let unknown_root = ID::from_bytes("some-unsynced-tree");
    let unknown_tip = ID::from_bytes("some-unsynced-tip");
    let first = settings_entry(&fx, pointer_write(&unknown_root, vec![unknown_tip])).await;
    fx.accept(&first, "first declaration of an unsynced tree")
        .await;

    let mut bounds_only = Doc::new();
    bounds_only.set(
        format!(
            "auth.delegations.{}.permission_bounds.max",
            fx.identity.root_id()
        ),
        "write:3",
    );
    let entry = settings_entry(&fx, bounds_only).await;
    fx.accept(&entry, "bounds-only write").await;
}

/// The gate is decided against the pre-state the entry pins: a signature
/// through the delegation on a sibling branch still resolves at the old
/// pointer, and a descendant of the advance must cover the new one.
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

/// Concurrent pointer advances are both causal floors of a merge. Atomic CRDT
/// resolution may select one sibling's reference, but the security gate must
/// require the merged write to cover both.
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
    fx.reject(&only_x, "merge pointer covering only one parent floor")
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

/// Removing and later re-adding a delegation does not erase the last
/// committed pointer. The forward-only floor carries through the removal.
#[tokio::test]
async fn pointer_removal_does_not_reset_floor() {
    let fx = fixture().await;
    let forward = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i1.clone())).await;
    fx.accept(&forward, "advance to i1").await;

    let mut removal = Doc::new();
    removal.remove(format!("auth.delegations.{}", fx.identity.root_id()));
    let removal = settings_entry(&fx, removal).await;
    fx.accept(&removal, "remove delegation").await;

    let readd = settings_entry(&fx, pointer_write(fx.identity.root_id(), fx.i0.clone())).await;
    fx.reject(&readd, "re-add below the last committed pointer")
        .await;
}
