//! Explicit genesis authority uses the same native validator as remote ingest.

use crate::helpers::{LocalBackendTestExt, test_instance, test_local_instance};
use eidetica::{
    Database, Entry, Error, ID, Instance,
    auth::{
        AuthSettings,
        crypto::{PrivateKey, verify_entry_signature},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey, TreeReference,
        },
    },
    backend::VerificationStatus,
    constants::SETTINGS,
    crdt::{Codec, Doc, doc::Value},
    database::DatabaseKey,
    store::DocStore,
};

fn settings(auth: AuthSettings) -> Doc {
    let mut settings = Doc::new();
    settings.set("auth", auth.as_doc().clone());
    settings
}

fn delegation(root: &ID, tips: Vec<ID>) -> DelegatedTreeRef {
    DelegatedTreeRef {
        tree: TreeReference {
            root: root.clone(),
            tips,
        },
        permission_bounds: PermissionBounds {
            max: Permission::Admin(0),
            min: None,
        },
    }
}

fn delegated_identity(reference: &DelegatedTreeRef, signer: &PrivateKey) -> SigKey {
    SigKey::Delegation {
        path: vec![DelegationStep {
            tree: reference.tree.root.clone(),
            tips: reference.tree.tips.clone(),
        }],
        hint: KeyHint::from_pubkey(&signer.public_key()),
    }
}

fn raw_genesis(auth: AuthSettings, signer: Option<(&PrivateKey, SigKey)>) -> Entry {
    let entry = Entry::root_builder()
        .set_metadata(br#"{"settings_tips":[],"entropy":null}"#)
        .set_subtree_data(SETTINGS, settings(auth).encode().unwrap())
        .build()
        .unwrap();
    match signer {
        Some((key, identity)) => entry
            .with_auth(|auth| auth.key = identity)
            .sign(key)
            .unwrap(),
        None => entry,
    }
}

async fn ingest(instance: &Instance, entry: Entry) -> VerificationStatus {
    let id = entry.id();
    instance.backend().put(entry).await.unwrap();
    Database::open(instance, &id)
        .await
        .unwrap()
        .verify()
        .await
        .unwrap();
    instance
        .backend()
        .get_verification_status(&id)
        .await
        .unwrap()
}

#[tokio::test]
async fn explicit_direct_genesis_preserves_stronger_owner() {
    let instance = test_instance().await;
    let owner = PrivateKey::generate();
    let app = PrivateKey::generate();
    let mut auth = AuthSettings::new();
    auth.add_key(
        &owner.public_key(),
        AuthKey::active(None, Permission::Admin(0)),
    )
    .unwrap();
    auth.add_key(
        &app.public_key(),
        AuthKey::active(None, Permission::Admin(20)),
    )
    .unwrap();
    let db = Database::create(&instance, app.clone(), settings(auth))
        .await
        .unwrap();
    let genesis = db.get_entry(db.root_id()).await.unwrap();
    verify_entry_signature(&genesis, &app.public_key()).unwrap();
    assert!(verify_entry_signature(&genesis, &owner.public_key()).is_err());
    assert_eq!(
        db.current_permission().await.unwrap(),
        Permission::Admin(20)
    );
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&owner.public_key())
        .await
        .unwrap();
    assert!(
        txn.commit().await.is_err(),
        "genesis must not confer a creator bypass"
    );
}

#[tokio::test]
async fn raw_delegation_only_genesis_requires_signature() {
    let instance = test_local_instance().await;
    let owner = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference).unwrap();
    assert_eq!(
        ingest(&instance, raw_genesis(auth, None)).await,
        VerificationStatus::Failed
    );
}

#[tokio::test]
async fn raw_delegation_only_genesis_accepts_verified_signer() {
    let instance = test_local_instance().await;
    let owner = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let sigkey = delegated_identity(&reference, &owner);
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference).unwrap();
    assert_eq!(
        ingest(&instance, raw_genesis(auth, Some((&owner, sigkey)))).await,
        VerificationStatus::Verified
    );
}

async fn write_data(db: &Database) -> ID {
    let txn = db.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("written", "by selected signer")
        .await
        .unwrap();
    txn.commit().await.unwrap()
}

/// Build an untrusted child on exactly the current causal settings frontier.
async fn raw_child(
    instance: &Instance,
    db: &Database,
    store: &str,
    delta: Doc,
    signer: Option<(&PrivateKey, SigKey)>,
) -> Entry {
    let snapshot = db.snapshot().await.unwrap();
    let tips = snapshot.tips().to_vec();
    assert_eq!(tips.len(), 1, "fixture keeps a linear Verified history");
    let parent = db.get_entry(&tips[0]).await.unwrap();
    let settings_tips = instance
        .backend()
        .store_snapshot_at(db.root_id(), SETTINGS, &snapshot)
        .await
        .unwrap()
        .into_tips();
    let store_tips = instance
        .backend()
        .store_snapshot_at(db.root_id(), store, &snapshot)
        .await
        .unwrap()
        .into_tips();
    let metadata =
        serde_json::to_vec(&serde_json::json!({"settings_tips": settings_tips, "entropy": null}))
            .unwrap();
    let entry = Entry::builder(db.root_id().clone())
        .set_parents(tips)
        .set_height(parent.height() + 1)
        .set_metadata(metadata)
        .set_subtree_parents(store, store_tips)
        .set_subtree_data(store, delta.encode().unwrap())
        .build()
        .unwrap();
    match signer {
        Some((key, identity)) => entry
            .with_auth(|auth| auth.key = identity)
            .sign(key)
            .unwrap(),
        None => entry,
    }
}

async fn ingest_child(instance: &Instance, db: &Database, entry: Entry) -> VerificationStatus {
    let id = entry.id();
    instance.backend().put(entry).await.unwrap();
    db.verify().await.unwrap();
    instance
        .backend()
        .get_verification_status(&id)
        .await
        .unwrap()
}

/// The same tests use a genuinely connected instance under TEST_BACKEND=service.
#[tokio::test]
async fn explicit_delegation_only_uses_actual_lower_priority_signer() {
    let instance = test_instance().await;
    let owner = PrivateKey::generate();
    let app = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    crate::helpers::add_auth_keys(
        &identity,
        &[(
            &app.public_key(),
            AuthKey::active(Some("app"), Permission::Admin(20)),
        )],
    )
    .await;
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference.clone()).unwrap();
    let mut sigkey = delegated_identity(&reference, &app);
    // A name-only hint must not select the service connection's login signer.
    *sigkey.hint_mut() = KeyHint::from_name("app");
    let db = Database::create_with_init(
        &instance,
        DatabaseKey::with_identity(app.clone(), sigkey),
        settings(auth.clone()),
        async |txn| {
            txn.get_store::<DocStore>("config")
                .await?
                .set("initial", "value")
                .await?;
            Ok(())
        },
    )
    .await
    .unwrap();
    let genesis = db.get_entry(db.root_id()).await.unwrap();
    verify_entry_signature(&genesis, &app.public_key()).unwrap();
    assert!(verify_entry_signature(&genesis, &owner.public_key()).is_err());
    assert_eq!(genesis.auth().key.hint().pubkey, Some(app.public_key()));
    assert!(matches!(genesis.auth().key, SigKey::Delegation { .. }));
    assert_eq!(
        db.get_settings()
            .await
            .unwrap()
            .auth_snapshot()
            .await
            .unwrap()
            .as_doc(),
        auth.as_doc()
    );
    // Real commits exercise server-side native validation on connected instances.
    let child = write_data(&db).await;
    verify_entry_signature(&db.get_entry(&child).await.unwrap(), &app.public_key()).unwrap();

    // A direct grant at a permitted lower priority is allowed, but removing the
    // stronger owner-bearing delegation is not, even through a whole Auth map.
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &PrivateKey::generate().public_key(),
            AuthKey::active(None, Permission::Write(30)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let txn = db.new_transaction().await.unwrap();
    txn.get_store::<DocStore>(SETTINGS)
        .await
        .unwrap()
        .set("auth", Doc::atomic())
        .await
        .unwrap();
    assert!(txn.commit().await.is_err());
    write_data(&db).await;
}

#[tokio::test]
async fn explicit_mixed_genesis_has_no_direct_creator_bypass() {
    let instance = test_instance().await;
    let owner = PrivateKey::generate();
    let app = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference.clone()).unwrap();
    auth.add_key(
        &app.public_key(),
        AuthKey::active(None, Permission::Admin(20)),
    )
    .unwrap();
    let db = Database::create(&instance, app.clone(), settings(auth.clone()))
        .await
        .unwrap();
    assert_eq!(
        db.get_settings()
            .await
            .unwrap()
            .auth_snapshot()
            .await
            .unwrap()
            .as_doc(),
        auth.as_doc()
    );
    assert_eq!(
        db.current_permission().await.unwrap(),
        Permission::Admin(20)
    );
    write_data(&db).await;
    let txn = db.new_transaction().await.unwrap();
    let mut weakened = reference.clone();
    weakened.permission_bounds.max = Permission::Write(20);
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(weakened)
        .await
        .unwrap();
    assert!(
        txn.commit().await.is_err(),
        "app must not change stronger delegation"
    );
    // The owner can act through the pre-existing Identity without a new direct grant.
    let owner_identity = delegated_identity(&reference, &owner);
    let acting = eidetica::account::AccountSigner::new(
        db.root_id().clone(),
        DatabaseKey::with_identity(owner.clone(), owner_identity),
    );
    let owner_db = acting.open_database(&instance, db.root_id()).await.unwrap();
    let id = write_data(&owner_db).await;
    verify_entry_signature(&owner_db.get_entry(id).await.unwrap(), &owner.public_key()).unwrap();
}

#[tokio::test]
async fn explicit_auth_never_falls_back_on_unauthorized_or_invalid_configuration() {
    let instance = test_instance().await;
    let signer = PrivateKey::generate();
    for key in [
        AuthKey::active(None, Permission::Read),
        AuthKey::active(None, Permission::Write(0)),
        AuthKey::new(None, Permission::Admin(0), KeyStatus::Revoked),
    ] {
        let mut auth = AuthSettings::new();
        auth.add_key(&signer.public_key(), key).unwrap();
        assert!(
            Database::create(&instance, signer.clone(), settings(auth))
                .await
                .is_err()
        );
    }
    let mut auth = AuthSettings::new();
    auth.add_key(
        &PrivateKey::generate().public_key(),
        AuthKey::active(None, Permission::Admin(0)),
    )
    .unwrap();
    assert!(
        Database::create(&instance, signer.clone(), settings(auth))
            .await
            .is_err()
    );
    for value in [Value::Doc(Doc::new()), "invalid".into()] {
        let mut initial = Doc::new();
        initial.set("auth", value);
        assert!(
            Database::create(&instance, signer.clone(), initial)
                .await
                .is_err()
        );
    }
    let mut deleted = Doc::new();
    deleted.set("auth", Doc::new());
    deleted.remove("auth");
    assert!(
        Database::create(&instance, signer.clone(), deleted)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn explicit_delegation_rejects_wrong_key_path_revocation_and_write_bounds() {
    let instance = test_instance().await;
    let owner = PrivateKey::generate();
    let revoked = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    crate::helpers::add_auth_keys(
        &identity,
        &[(
            &revoked.public_key(),
            AuthKey::active(None, Permission::Admin(20)),
        )],
    )
    .await;
    let txn = identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&revoked.public_key())
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference.clone()).unwrap();
    for key in [revoked, PrivateKey::generate()] {
        let supplied =
            DatabaseKey::with_identity(key.clone(), delegated_identity(&reference, &key));
        assert!(
            Database::create(&instance, supplied, settings(auth.clone()))
                .await
                .is_err()
        );
    }
    let supplied = DatabaseKey::with_identity(
        PrivateKey::generate(),
        delegated_identity(&reference, &owner),
    );
    assert!(
        Database::create(&instance, supplied, settings(auth.clone()))
            .await
            .is_err()
    );
    // Even a separately authorized direct key must not rescue an invalid path.
    auth.add_key(
        &owner.public_key(),
        AuthKey::active(None, Permission::Admin(0)),
    )
    .unwrap();
    let wrong_reference = delegation(&ID::from_bytes("foreign-root"), reference.tree.tips.clone());
    let supplied =
        DatabaseKey::with_identity(owner.clone(), delegated_identity(&wrong_reference, &owner));
    assert!(
        Database::create(&instance, supplied, settings(auth))
            .await
            .is_err()
    );
    let mut bounded = reference.clone();
    bounded.permission_bounds.max = Permission::Write(20);
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(bounded).unwrap();
    let supplied =
        DatabaseKey::with_identity(owner.clone(), delegated_identity(&reference, &owner));
    assert!(
        Database::create(&instance, supplied, settings(auth))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn raw_genesis_missing_unverified_and_foreign_proof_is_not_authority() {
    let source = test_local_instance().await;
    let owner = PrivateKey::generate();
    let identity = Database::create(&source, owner.clone(), Doc::new())
        .await
        .unwrap();
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference.clone()).unwrap();
    let entry = raw_genesis(
        auth.clone(),
        Some((&owner, delegated_identity(&reference, &owner))),
    );
    let receiver = test_local_instance().await;
    let key = DatabaseKey::with_identity(owner.clone(), delegated_identity(&reference, &owner));
    let missing = Database::create(&receiver, key.clone(), settings(auth.clone()))
        .await
        .unwrap_err();
    assert!(matches!(missing, Error::Auth(ref e) if e.is_delegated_tree_unsynced()));
    assert_eq!(
        ingest(&receiver, entry.clone()).await,
        VerificationStatus::Unverified
    );
    let proof = identity.get_entry(identity.root_id()).await.unwrap();
    receiver.backend().put(proof).await.unwrap();
    let unverified = Database::create(&receiver, key.clone(), settings(auth.clone()))
        .await
        .unwrap_err();
    assert!(matches!(unverified, Error::Auth(ref e) if e.is_delegated_tree_unsynced()));
    assert_eq!(
        ingest(&receiver, entry.clone()).await,
        VerificationStatus::Unverified
    );
    // Only an actual verification of the proof permits promotion on retry.
    Database::open(&receiver, identity.root_id())
        .await
        .unwrap()
        .verify()
        .await
        .unwrap();
    assert_eq!(ingest(&receiver, entry).await, VerificationStatus::Verified);
    let created = Database::create(&receiver, key, settings(auth))
        .await
        .unwrap();
    write_data(&created).await;

    let foreign = Database::create(&source, owner.clone(), Doc::new())
        .await
        .unwrap();
    let mut wrong = reference.clone();
    wrong.tree.tips = vec![foreign.root_id().clone()];
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(wrong.clone()).unwrap();
    assert_eq!(
        ingest(
            &source,
            raw_genesis(auth, Some((&owner, delegated_identity(&wrong, &owner))))
        )
        .await,
        VerificationStatus::Failed
    );
    let mut empty = reference;
    empty.tree.tips.clear();
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(empty.clone()).unwrap();
    assert_eq!(
        ingest(
            &source,
            raw_genesis(auth, Some((&owner, delegated_identity(&empty, &owner))))
        )
        .await,
        VerificationStatus::Failed
    );
}

#[tokio::test]
async fn raw_delegation_only_descendants_and_owner_mutations() {
    let instance = test_local_instance().await;
    let owner = PrivateKey::generate();
    let app = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    crate::helpers::add_auth_keys(
        &identity,
        &[(
            &app.public_key(),
            AuthKey::active(None, Permission::Admin(20)),
        )],
    )
    .await;
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference.clone()).unwrap();
    let sigkey = delegated_identity(&reference, &app);
    let root = raw_genesis(auth, Some((&app, sigkey.clone())));
    let root_id = root.id();
    assert_eq!(ingest(&instance, root).await, VerificationStatus::Verified);
    let db = Database::open(&instance, &root_id).await.unwrap();
    let unsigned = raw_child(&instance, &db, "data", Doc::new(), None).await;
    let valid = raw_child(
        &instance,
        &db,
        "data",
        Doc::new(),
        Some((&app, sigkey.clone())),
    )
    .await;
    assert_eq!(
        ingest_child(&instance, &db, unsigned).await,
        VerificationStatus::Failed
    );
    assert_eq!(
        ingest_child(&instance, &db, valid).await,
        VerificationStatus::Verified
    );
    let mut delta = Doc::new();
    // A whole-map atomic replacement cannot resurrect genesis rules.
    let mut empty_auth = Doc::atomic();
    empty_auth.set("keys", Doc::new());
    delta.set("auth", empty_auth);
    let forged = raw_child(
        &instance,
        &db,
        SETTINGS,
        delta,
        Some((&app, sigkey.clone())),
    )
    .await;
    assert_eq!(
        ingest_child(&instance, &db, forged).await,
        VerificationStatus::Failed
    );
    let mut promotion = Doc::new();
    promotion.set(
        format!("auth.keys.{}", app.public_key()),
        AuthKey::active(None, Permission::Admin(0)),
    );
    let forged = raw_child(&instance, &db, SETTINGS, promotion, Some((&app, sigkey))).await;
    assert_eq!(
        ingest_child(&instance, &db, forged).await,
        VerificationStatus::Failed
    );
}

#[tokio::test]
async fn explicit_mixed_genesis_requires_verified_owner_pointer_even_for_direct_signer() {
    let instance = test_instance().await;
    let signer = PrivateKey::generate();
    let identity = Database::create(&instance, signer.clone(), Doc::new())
        .await
        .unwrap();
    let foreign = Database::create(&instance, signer.clone(), Doc::new())
        .await
        .unwrap();
    for tips in [
        vec![ID::from_bytes("missing-proof")],
        vec![foreign.root_id().clone()],
        vec![],
    ] {
        let mut auth = AuthSettings::new();
        auth.add_key(
            &signer.public_key(),
            AuthKey::active(None, Permission::Admin(20)),
        )
        .unwrap();
        auth.add_delegated_tree(delegation(identity.root_id(), tips))
            .unwrap();
        assert!(
            Database::create(&instance, signer.clone(), settings(auth))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn raw_direct_and_mixed_genesis_protect_owner_after_bootstrap() {
    let instance = test_local_instance().await;
    let owner = PrivateKey::generate();
    let app = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new())
        .await
        .unwrap();
    let reference = delegation(
        identity.root_id(),
        identity.snapshot().await.unwrap().into_tips(),
    );
    for mixed in [false, true] {
        let mut auth = AuthSettings::new();
        auth.add_key(
            &app.public_key(),
            AuthKey::active(None, Permission::Admin(20)),
        )
        .unwrap();
        if mixed {
            auth.add_delegated_tree(reference.clone()).unwrap();
        } else {
            auth.add_key(
                &owner.public_key(),
                AuthKey::active(None, Permission::Admin(0)),
            )
            .unwrap();
        }
        let sigkey = SigKey::from_pubkey(&app.public_key());
        let root = raw_genesis(auth, Some((&app, sigkey.clone())));
        let id = root.id();
        assert_eq!(ingest(&instance, root).await, VerificationStatus::Verified);
        let db = Database::open(&instance, &id).await.unwrap();
        let mut replacement = Doc::atomic();
        replacement.set(
            format!("keys.{}", app.public_key()),
            AuthKey::active(None, Permission::Admin(0)),
        );
        let mut delta = Doc::new();
        delta.set("auth", replacement);
        let forged = raw_child(
            &instance,
            &db,
            SETTINGS,
            delta,
            Some((&app, sigkey.clone())),
        )
        .await;
        assert_eq!(
            ingest_child(&instance, &db, forged).await,
            VerificationStatus::Failed
        );
        let good = raw_child(&instance, &db, "data", Doc::new(), Some((&app, sigkey))).await;
        assert_eq!(
            ingest_child(&instance, &db, good).await,
            VerificationStatus::Verified
        );
    }
}
