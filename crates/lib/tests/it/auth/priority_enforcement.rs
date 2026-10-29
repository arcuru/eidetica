//! Admin priority enforcement on signed `_settings.auth` writes.
//!
//! A signer may only change grants it outranks or equals, and may not create
//! a grant above its own permission. These tests drive real signed entries
//! through both the local commit path and remote ingest/verification, so the
//! check is exercised where it is enforced rather than through helper
//! booleans.

use crate::helpers::LocalBackendTestExt;
use eidetica::{
    Database, Entry, Instance, Result,
    auth::{
        crypto::{PrivateKey, sign_entry},
        types::{
            AuthKey, DelegatedTreeRef, KeyStatus, Permission, PermissionBounds, SigKey,
            TreeReference,
        },
    },
    backend::VerificationStatus,
    constants::SETTINGS,
    crdt::Doc,
    entry::ID,
    store::DocStore,
};

struct Fixture {
    instance: Instance,
    root: ID,
    super_admin: PrivateKey,
    low_admin: PrivateKey,
    peer_admin: PrivateKey,
    writer: PrivateKey,
}

impl Fixture {
    /// Admin(0) creator, plus Admin(20) `low_admin`, an equal-priority
    /// Admin(20) `peer_admin` and a Write(30) `writer`.
    async fn new() -> Self {
        let instance = crate::helpers::test_local_instance().await;
        let super_admin = PrivateKey::generate();
        let db = Database::create(&instance, super_admin.clone(), Doc::new())
            .await
            .unwrap();
        let low_admin = PrivateKey::generate();
        let peer_admin = PrivateKey::generate();
        let writer = PrivateKey::generate();
        crate::helpers::add_auth_keys(
            &db,
            &[
                (
                    &low_admin.public_key(),
                    AuthKey::active(Some("low"), Permission::Admin(20)),
                ),
                (
                    &peer_admin.public_key(),
                    AuthKey::active(Some("peer"), Permission::Admin(20)),
                ),
                (
                    &writer.public_key(),
                    AuthKey::active(Some("writer"), Permission::Write(30)),
                ),
            ],
        )
        .await;
        Self {
            instance,
            root: db.root_id().clone(),
            super_admin,
            low_admin,
            peer_admin,
            writer,
        }
    }

    async fn db_as(&self, key: &PrivateKey) -> Database {
        Database::open(&self.instance, &self.root)
            .await
            .unwrap()
            .with_key(key.clone())
    }

    /// Commit a raw `_settings` write signed by `key` through the normal
    /// transaction path.
    async fn commit_raw(
        &self,
        key: &PrivateKey,
        edit: impl AsyncFnOnce(&DocStore) -> Result<()>,
    ) -> Result<ID> {
        let txn = self.db_as(key).await.new_transaction().await?;
        let settings = txn.get_store::<DocStore>(SETTINGS).await?;
        edit(&settings).await?;
        txn.commit().await
    }

    async fn auth_key(&self, pubkey: &PrivateKey) -> AuthKey {
        let db = Database::open(&self.instance, &self.root).await.unwrap();
        db.get_settings()
            .await
            .unwrap()
            .get_auth_key(&pubkey.public_key())
            .await
            .unwrap()
    }

    /// Craft a signed `_settings` entry with `delta` on the current single
    /// tip, bypassing the local transaction check, as a remote peer could.
    async fn craft_settings_entry(&self, signer: &PrivateKey, delta: &Doc) -> Entry {
        let engine = self.instance.backend().local_engine().unwrap();
        let db = Database::open(&self.instance, &self.root).await.unwrap();
        let tips = db.snapshot().await.unwrap().into_tips();
        assert_eq!(tips.len(), 1, "fixture keeps a linear history");
        let parent = engine.get(&tips[0]).await.unwrap();
        let settings_tips = engine
            .store_snapshot_at(&self.root, SETTINGS, &tips.clone().into())
            .await
            .unwrap()
            .into_tips();
        let metadata = serde_json::to_vec(&serde_json::json!({
            "settings_tips": settings_tips,
            "entropy": null,
        }))
        .unwrap();
        let entry = Entry::builder(self.root.clone())
            .set_parents(tips)
            .set_height(parent.height() + 1)
            .set_metadata(metadata)
            .set_subtree_data(SETTINGS, serde_json::to_vec(delta).unwrap())
            .set_subtree_parents(SETTINGS, settings_tips)
            .build()
            .unwrap()
            .with_auth(|auth| auth.key = SigKey::from_pubkey(&signer.public_key()));
        let signature = sign_entry(&entry, signer).unwrap();
        entry.with_auth(|auth| auth.signature = Some(signature))
    }

    /// Replicate this database to a fresh receiver, ingest `entry` as an
    /// unverified remote write and return the receiver's verdict.
    async fn remote_verdict(&self, entry: Entry) -> VerificationStatus {
        let receiver = crate::helpers::test_local_instance().await;
        for existing in self.instance.backend().get_tree(&self.root).await.unwrap() {
            receiver.backend().put(existing).await.unwrap();
        }
        let id = entry.id();
        receiver.backend().put(entry).await.unwrap();
        Database::open(&receiver, &self.root)
            .await
            .unwrap()
            .verify()
            .await
            .unwrap();
        receiver
            .backend()
            .get_verification_status(&id)
            .await
            .unwrap()
    }
}

fn key_delta(pubkey: &PrivateKey, key: AuthKey) -> Doc {
    let mut delta = Doc::new();
    delta.set(format!("auth.keys.{}", pubkey.public_key()), key);
    delta
}

/// The commit must fail entry validation, not some unrelated earlier check.
fn assert_rejected(result: Result<ID>, what: &str) {
    assert!(
        matches!(
            &result,
            Err(eidetica::Error::Transaction(e))
                if matches!(**e, eidetica::transaction::TransactionError::EntryValidationFailed)
        ),
        "{what} must fail entry validation, got {result:?}"
    );
}

// ===== Local commit: negative controls =====

#[tokio::test]
async fn lower_priority_admin_cannot_revoke_higher_admin() {
    let fx = Fixture::new().await;
    let txn = fx
        .db_as(&fx.low_admin)
        .await
        .new_transaction()
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&fx.super_admin.public_key())
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Admin(20) revoking Admin(0)");
    assert_eq!(
        fx.auth_key(&fx.super_admin).await.status(),
        &KeyStatus::Active
    );
}

#[tokio::test]
async fn lower_priority_admin_cannot_demote_or_replace_higher_admin() {
    let fx = Fixture::new().await;
    for replacement in [
        AuthKey::active(Some("demoted"), Permission::Write(0)),
        AuthKey::active(Some("demoted"), Permission::Admin(20)),
        AuthKey::new(None, Permission::Admin(0), KeyStatus::Revoked),
    ] {
        let txn = fx
            .db_as(&fx.low_admin)
            .await
            .new_transaction()
            .await
            .unwrap();
        txn.get_settings()
            .unwrap()
            .set_auth_key(&fx.super_admin.public_key(), replacement.clone())
            .await
            .unwrap();
        assert_rejected(
            txn.commit().await,
            &format!("Admin(20) replacing Admin(0) with {replacement:?}"),
        );
    }
    // Renaming is a modification too.
    let txn = fx
        .db_as(&fx.low_admin)
        .await
        .new_transaction()
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .rename_auth_key(&fx.super_admin.public_key(), Some("renamed"))
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Admin(20) renaming Admin(0)");
    assert_eq!(
        *fx.auth_key(&fx.super_admin).await.permissions(),
        Permission::Admin(0)
    );
}

#[tokio::test]
async fn admin_cannot_promote_itself_or_grant_stronger_authority() {
    let fx = Fixture::new().await;
    let txn = fx
        .db_as(&fx.low_admin)
        .await
        .new_transaction()
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &fx.low_admin.public_key(),
            AuthKey::active(Some("low"), Permission::Admin(0)),
        )
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Admin(20) self-promotion");

    let newcomer = PrivateKey::generate();
    let txn = fx
        .db_as(&fx.low_admin)
        .await
        .new_transaction()
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &newcomer.public_key(),
            AuthKey::active(None, Permission::Admin(19)),
        )
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Admin(20) granting Admin(19)");

    let txn = fx
        .db_as(&fx.low_admin)
        .await
        .new_transaction()
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Admin(0)))
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Admin(20) granting global Admin(0)");

    let txn = fx
        .db_as(&fx.low_admin)
        .await
        .new_transaction()
        .await
        .unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Admin(0),
                min: None,
            },
            tree: TreeReference {
                root: fx.root.clone(),
                tips: vec![fx.root.clone()],
            },
        })
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Admin(20) delegating Admin(0)");
    assert_eq!(
        *fx.auth_key(&fx.low_admin).await.permissions(),
        Permission::Admin(20)
    );
}

#[tokio::test]
async fn raw_settings_deltas_cannot_remove_higher_admin() {
    let fx = Fixture::new().await;
    let super_path = format!("auth.keys.{}", fx.super_admin.public_key());
    assert_rejected(
        fx.commit_raw(&fx.low_admin, async |s| {
            s.delete(&super_path).await.map(|_| ())
        })
        .await,
        "raw delete of Admin(0)",
    );

    // Whole keys-map replacement that keeps only the signer.
    let mut keys = Doc::atomic();
    keys.set(
        fx.low_admin.public_key().to_string(),
        AuthKey::active(Some("low"), Permission::Admin(20)),
    );
    assert_rejected(
        fx.commit_raw(&fx.low_admin, async |s| s.set("auth.keys", keys).await)
            .await,
        "atomic keys-map replacement",
    );

    // Garbage over the strong key removes it from the parsed view.
    assert_rejected(
        fx.commit_raw(&fx.low_admin, async |s| s.set(&super_path, "gone").await)
            .await,
        "overwriting Admin(0) with a non-key value",
    );
    assert_eq!(
        *fx.auth_key(&fx.super_admin).await.permissions(),
        Permission::Admin(0)
    );
}

#[tokio::test]
async fn writer_still_cannot_touch_settings() {
    let fx = Fixture::new().await;
    let txn = fx.db_as(&fx.writer).await.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &PrivateKey::generate().public_key(),
            AuthKey::active(None, Permission::Write(40)),
        )
        .await
        .unwrap();
    assert_rejected(txn.commit().await, "Write key settings write");
}

// ===== Local commit: positive controls =====

#[tokio::test]
async fn permitted_admin_changes_still_commit() {
    let fx = Fixture::new().await;
    let low = fx.db_as(&fx.low_admin).await;

    // Equal priority: modify and revoke a peer Admin(20).
    let txn = low.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .rename_auth_key(&fx.peer_admin.public_key(), Some("peer-renamed"))
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Lower priority: create, demote and revoke weaker grants.
    let newcomer = PrivateKey::generate();
    let txn = low.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
        .set_auth_key(
            &newcomer.public_key(),
            AuthKey::active(None, Permission::Admin(20)),
        )
        .await
        .unwrap();
    settings
        .set_auth_key(
            &fx.writer.public_key(),
            AuthKey::active(Some("writer"), Permission::Write(40)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let txn = low.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&fx.peer_admin.public_key())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Self-demotion is allowed.
    let txn = low.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &fx.low_admin.public_key(),
            AuthKey::active(Some("low"), Permission::Admin(25)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // The top admin can still promote and revoke anyone.
    let top = fx.db_as(&fx.super_admin).await;
    let txn = top.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &fx.low_admin.public_key(),
            AuthKey::active(Some("low"), Permission::Admin(1)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let txn = top.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&fx.low_admin.public_key())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    assert_eq!(
        fx.auth_key(&fx.peer_admin).await.status(),
        &KeyStatus::Revoked
    );
    assert_eq!(
        *fx.auth_key(&fx.writer).await.permissions(),
        Permission::Write(40)
    );
    assert_eq!(
        fx.auth_key(&fx.low_admin).await.status(),
        &KeyStatus::Revoked
    );

    // Non-auth settings writes are unaffected by priority.
    fx.commit_raw(&fx.super_admin, async |s| s.set("name", "renamed").await)
        .await
        .unwrap();
}

// ===== Remote ingest/verification =====

#[tokio::test]
async fn remote_verification_rejects_priority_violations() {
    let fx = Fixture::new().await;

    // Positive control: the crafted shape verifies for a permitted change,
    // so a Failed verdict below is the priority check, not a broken entry.
    let allowed = fx
        .craft_settings_entry(
            &fx.low_admin,
            &key_delta(&fx.writer, AuthKey::active(None, Permission::Write(31))),
        )
        .await;
    assert_eq!(
        fx.remote_verdict(allowed).await,
        VerificationStatus::Verified
    );

    let revoke = fx
        .craft_settings_entry(
            &fx.low_admin,
            &key_delta(
                &fx.super_admin,
                AuthKey::new(None, Permission::Admin(0), KeyStatus::Revoked),
            ),
        )
        .await;
    assert_eq!(fx.remote_verdict(revoke).await, VerificationStatus::Failed);

    let promote = fx
        .craft_settings_entry(
            &fx.low_admin,
            &key_delta(&fx.low_admin, AuthKey::active(None, Permission::Admin(0))),
        )
        .await;
    assert_eq!(fx.remote_verdict(promote).await, VerificationStatus::Failed);

    let mut delete = Doc::new();
    delete.remove(format!("auth.keys.{}", fx.super_admin.public_key()));
    let delete = fx.craft_settings_entry(&fx.low_admin, &delete).await;
    assert_eq!(fx.remote_verdict(delete).await, VerificationStatus::Failed);

    // Whole-auth replacement drops every existing grant.
    let mut replace = Doc::new();
    let mut auth = Doc::atomic();
    auth.set(
        format!("keys.{}", fx.low_admin.public_key()),
        AuthKey::active(None, Permission::Admin(20)),
    );
    replace.set("auth", auth);
    let replace = fx.craft_settings_entry(&fx.low_admin, &replace).await;
    assert_eq!(fx.remote_verdict(replace).await, VerificationStatus::Failed);
}
