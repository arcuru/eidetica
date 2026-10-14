//! Tests for delegated-auth snapshot floors: transitive per-entry
//! non-regression of pinned delegated-tree snapshots, and operator-managed
//! committed delegation pointers.
//!
//! Hand-built entries exercise `AuthValidator::validate_entry` against the
//! target tree's pinned settings. `Fixture::accept` validates at this seam,
//! then injects a Verified label; it does not run `Database::verify`, commit
//! a transaction or ingest a remote entry. Separate cases exercise real local
//! commits and remote ingestion.

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

mod inherited;
mod pointers;
mod proofs_and_pins;

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
        crate::NewAccount::passwordless("admin"),
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
        self.data_entry_with_pin(parents, &settings_tips).await
    }

    async fn data_entry_with_pin(&self, parents: &[ID], settings_tips: &[ID]) -> Entry {
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

    /// Exercise the auth-validator seam against the pinned settings.
    async fn validate(&self, entry: &Entry) -> Result<bool> {
        let (_, auth) = self.pinned_settings().await;
        AuthValidator::new()
            .validate_entry(entry, &auth, Some(&self.instance))
            .await
    }

    /// Inject `entry` and label it Verified without validating it. Returns its ID.
    ///
    /// Bypasses `Database::verify`, local commit and remote ingestion, so tests
    /// can deliberately construct synthetic ancestry or inconsistent status labels.
    async fn inject_verified_unchecked(&self, entry: &Entry) -> ID {
        let id = entry.id();
        let engine = self.engine();
        engine.put(entry.clone()).await.unwrap();
        engine
            .update_verification_status(&id, VerificationStatus::Verified)
            .await
            .unwrap();
        id
    }

    /// Assert auth-validator acceptance, then inject a Verified label unchecked.
    /// This is not `Database::verify`, a local commit or remote ingestion.
    async fn accept(&self, entry: &Entry, what: &str) -> ID {
        assert!(self.validate(entry).await.unwrap(), "{what} must validate");
        self.inject_verified_unchecked(entry).await
    }

    async fn reject(&self, entry: &Entry, what: &str) {
        assert!(
            !self.validate(entry).await.unwrap(),
            "{what} must be rejected"
        );
    }

    /// Exercise real remote ingestion and read the resulting verification status.
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
