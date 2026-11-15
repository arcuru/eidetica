//! Explicit genesis authority uses the same native validator as remote ingest.

use eidetica::{
    Database, Entry, Instance,
    auth::{
        AuthSettings,
        crypto::{PrivateKey, verify_entry_signature},
        types::{AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds, SigKey, TreeReference},
    },
    backend::VerificationStatus,
    constants::SETTINGS,
    crdt::{Codec, Doc},
};
use crate::helpers::{LocalBackendTestExt, test_instance, test_local_instance};

fn settings(auth: AuthSettings) -> Doc {
    let mut settings = Doc::new();
    settings.set("auth", auth.as_doc().clone());
    settings
}

fn delegation(root: &eidetica::ID, tips: Vec<eidetica::ID>) -> DelegatedTreeRef {
    DelegatedTreeRef {
        tree: TreeReference { root: root.clone(), tips },
        permission_bounds: PermissionBounds { max: Permission::Admin(0), min: None },
    }
}

fn delegated_identity(reference: &DelegatedTreeRef, signer: &PrivateKey) -> SigKey {
    SigKey::Delegation {
        path: vec![DelegationStep { tree: reference.tree.root.clone(), tips: reference.tree.tips.clone() }],
        hint: KeyHint::from_pubkey(&signer.public_key()),
    }
}

fn raw_genesis(auth: AuthSettings, signer: Option<(&PrivateKey, SigKey)>) -> Entry {
    let entry = Entry::root_builder()
        .set_metadata(br#"{"settings_tips":[],"entropy":null}"#)
        .set_subtree_data(SETTINGS, settings(auth).encode().unwrap())
        .build().unwrap();
    match signer {
        Some((key, identity)) => entry.with_auth(|auth| auth.key = identity).sign(key).unwrap(),
        None => entry,
    }
}

async fn ingest(instance: &Instance, entry: Entry) -> VerificationStatus {
    let id = entry.id();
    instance.backend().put(entry).await.unwrap();
    Database::open(instance, &id).await.unwrap().verify().await.unwrap();
    instance.backend().get_verification_status(&id).await.unwrap()
}

#[tokio::test]
async fn explicit_direct_genesis_preserves_stronger_owner() {
    let instance = test_instance().await;
    let owner = PrivateKey::generate();
    let app = PrivateKey::generate();
    let mut auth = AuthSettings::new();
    auth.add_key(&owner.public_key(), AuthKey::active(None, Permission::Admin(0))).unwrap();
    auth.add_key(&app.public_key(), AuthKey::active(None, Permission::Admin(20))).unwrap();
    let db = Database::create(&instance, app.clone(), settings(auth)).await.unwrap();
    let genesis = db.get_entry(db.root_id()).await.unwrap();
    verify_entry_signature(&genesis, &app.public_key()).unwrap();
    assert!(verify_entry_signature(&genesis, &owner.public_key()).is_err());
    assert_eq!(db.current_permission().await.unwrap(), Permission::Admin(20));
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings().unwrap().revoke_auth_key(&owner.public_key()).await.unwrap();
    assert!(txn.commit().await.is_err(), "genesis must not confer a creator bypass");
}

#[tokio::test]
async fn raw_delegation_only_genesis_requires_signature() {
    let instance = test_local_instance().await;
    let owner = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new()).await.unwrap();
    let reference = delegation(identity.root_id(), identity.snapshot().await.unwrap().into_tips());
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference).unwrap();
    assert_eq!(ingest(&instance, raw_genesis(auth, None)).await, VerificationStatus::Failed);
}

#[tokio::test]
async fn raw_delegation_only_genesis_accepts_verified_signer() {
    let instance = test_local_instance().await;
    let owner = PrivateKey::generate();
    let identity = Database::create(&instance, owner.clone(), Doc::new()).await.unwrap();
    let reference = delegation(identity.root_id(), identity.snapshot().await.unwrap().into_tips());
    let sigkey = delegated_identity(&reference, &owner);
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(reference).unwrap();
    assert_eq!(ingest(&instance, raw_genesis(auth, Some((&owner, sigkey)))).await, VerificationStatus::Verified);
}
