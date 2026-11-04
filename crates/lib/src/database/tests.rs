//! Tests for the database module.

use super::*;
use crate::{
    auth::{
        crypto::{PrivateKey, generate_keypair, sign_entry},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
    },
    backend::database::InMemory,
    store::DocStore,
};

#[tokio::test]
async fn test_find_sigkeys_returns_sorted_by_permission() -> Result<()> {
    // Create instance
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await?;

    // Generate a test key
    let (signing_key, public_key) = generate_keypair();

    // Create database (Database::create bootstraps signing key as Admin(0))
    let db = Database::create(&instance, signing_key, Doc::new()).await?;

    // Add global Write(10) key via follow-up transaction (bootstrap key stays at Admin(0))
    let txn = db.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    settings_store
        .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
        .await?;
    txn.commit().await?;

    // Call find_sigkeys
    let results = Database::find_sigkeys(&instance, db.root_id(), &public_key).await?;

    // Verify we got 2 entries (direct key + global)
    assert_eq!(results.len(), 2, "Should find direct key and global option");

    // Verify they're sorted by permission, highest first
    // Admin(0) > Write(10)
    assert_eq!(
        results[0].1,
        Permission::Admin(0),
        "First should be Admin(0) from bootstrap key"
    );
    assert_eq!(
        results[1].1,
        Permission::Write(10),
        "Second should be Write(10) from global"
    );

    // Verify the SigKey types
    assert!(
        results[0].0.has_pubkey_hint(&public_key),
        "First should be direct pubkey hint"
    );
    assert!(results[1].0.is_global(), "Second should be global hint");

    Ok(())
}

#[tokio::test]
async fn test_create_bootstraps_signing_key_as_admin_zero() -> Result<()> {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await?;

    let (signing_key, signing_pubkey) = generate_keypair();

    // Create database (signing key is bootstrapped as Admin(0))
    let db = Database::create(&instance, signing_key, Doc::new()).await?;

    // Verify the signing key was bootstrapped as Admin(0)
    let results = Database::find_sigkeys(&instance, db.root_id(), &signing_pubkey).await?;
    assert_eq!(results.len(), 1, "Signing key should be present in auth");
    assert_eq!(
        results[0].1,
        Permission::Admin(0),
        "Signing key should be Admin(0)"
    );

    Ok(())
}

#[tokio::test]
async fn test_non_genesis_first_auth_cannot_authorize_its_signer_locally() -> Result<()> {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await?;
    let backend = instance.require_local_engine()?;
    let root = Entry::root_builder().build()?;
    let root_id = root.id();
    backend.put(root).await?;
    backend
        .update_verification_status(&root_id, VerificationStatus::Verified)
        .await?;

    let signer = PrivateKey::generate();
    let db = Database::open(&instance, &root_id)
        .await?
        .with_key(signer.clone());
    let txn = db.new_transaction().await?;
    txn.get_settings()?
        .set_auth_key(
            &signer.public_key(),
            AuthKey::active(Some("new"), Permission::Admin(0)),
        )
        .await?;
    let result = txn.commit().await;
    assert!(
        matches!(result, Err(Error::Transaction(ref e)) if matches!(**e, crate::transaction::TransactionError::EntryValidationFailed)),
        "a non-genesis first auth write must fail authorization, got {result:?}"
    );
    assert_eq!(backend.get_tree(&root_id).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_genesis_first_auth_survives_reverification() -> Result<()> {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await?;
    let (signer, _) = generate_keypair();
    let db = Database::create(&instance, signer, Doc::new()).await?;
    let id = db.root_id();
    let backend = instance.require_local_engine()?;
    assert_eq!(
        backend.get_verification_status(id).await?,
        VerificationStatus::Verified
    );
    instance.demote_to_unverified(id, id).await?;
    assert_eq!(
        backend.get_verification_status(id).await?,
        VerificationStatus::Unverified
    );
    db.verify().await?;
    assert_eq!(
        backend.get_verification_status(id).await?,
        VerificationStatus::Verified
    );
    Ok(())
}

#[tokio::test]
async fn test_create_rejects_preconfigured_auth() -> Result<()> {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await?;

    let (signing_key, _) = generate_keypair();

    let (_, other_pubkey) = generate_keypair();

    // Pre-configure auth in settings — this should be rejected
    let mut settings = Doc::new();
    settings.set("name", "test_reject");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        &other_pubkey,
        AuthKey::active(Some("other_user"), Permission::Write(5)),
    )?;
    settings.set("auth", auth_settings.as_doc().clone());

    // Database::create should return an error
    let result = Database::create(&instance, signing_key, settings).await;
    assert!(result.is_err(), "Should reject preconfigured auth");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must not contain auth configuration"),
        "Error should mention auth configuration, got: {err_msg}"
    );

    Ok(())
}

// ===== Write Callback Tests =====

/// Helper: create an Instance + Database for callback tests
async fn setup_callback_test() -> (Instance, Database) {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let (signing_key, _) = generate_keypair();
    let db = Database::create(&instance, signing_key, Doc::new())
        .await
        .unwrap();
    (instance, db)
}

struct DelegatedVerificationFixture {
    delegated_signing_key: PrivateKey,
    delegated_root: ID,
    delegated_entries: Vec<Entry>,
    delegated_entry_id: ID,
    target_root: ID,
    target_history: Vec<Entry>,
    target_entry: Entry,
}

async fn delegated_verification_fixture() -> DelegatedVerificationFixture {
    let (source, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();

    let delegated_signing_key = PrivateKey::generate();
    let delegated_pubkey = delegated_signing_key.public_key();
    let delegated_db = Database::create(&source, delegated_signing_key.clone(), Doc::new())
        .await
        .unwrap();
    let txn = delegated_db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &delegated_pubkey,
            AuthKey::active(Some("delegate"), Permission::Admin(0)),
        )
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let txn = delegated_db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_name("delegated authority")
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let delegated_tips = delegated_db.snapshot().await.unwrap().into_tips();

    let target_signing_key = PrivateKey::generate();
    let target_db = Database::create(&source, target_signing_key.clone(), Doc::new())
        .await
        .unwrap();
    let txn = target_db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(0),
                min: None,
            },
            tree: TreeReference {
                root: delegated_db.root_id().clone(),
                tips: delegated_tips.clone(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let target_via_delegation = Database::open(&source, target_db.root_id())
        .await
        .unwrap()
        .with_key(DatabaseKey::with_identity(
            delegated_signing_key.clone(),
            SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: delegated_db.root_id().clone(),
                    tips: delegated_tips,
                }],
                hint: KeyHint::from_pubkey(&delegated_pubkey),
            },
        ));
    let txn = target_via_delegation.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("delegated", "entry")
        .await
        .unwrap();
    let delegated_entry_id = txn.commit().await.unwrap();

    let backend = source.require_local_engine().unwrap();
    let delegated_root = delegated_db.root_id().clone();
    let delegated_entries = backend.get_tree(&delegated_root).await.unwrap();
    let target_root = target_db.root_id().clone();
    let mut target_history = backend.get_tree(&target_root).await.unwrap();
    let target_entry_index = target_history
        .iter()
        .position(|entry| entry.id() == delegated_entry_id)
        .unwrap();
    let target_entry = target_history.remove(target_entry_index);

    DelegatedVerificationFixture {
        delegated_signing_key,
        delegated_root,
        delegated_entries,
        delegated_entry_id,
        target_root,
        target_history,
        target_entry,
    }
}

async fn put_verified(instance: &Instance, entry: Entry) {
    let backend = instance.require_local_engine().unwrap();
    let id = entry.id();
    backend.put(entry).await.unwrap();
    backend
        .update_verification_status(&id, VerificationStatus::Verified)
        .await
        .unwrap();
}

async fn ingest_target_fixture(
    receiver: &Instance,
    fixture: &DelegatedVerificationFixture,
) -> Database {
    for entry in fixture.target_history.clone() {
        put_verified(receiver, entry).await;
    }
    receiver
        .put_remote_entries(&fixture.target_root, vec![fixture.target_entry.clone()])
        .await
        .unwrap();
    Database::open(receiver, &fixture.target_root)
        .await
        .unwrap()
}

async fn assert_retryable_and_invisible(
    receiver: &Instance,
    target_db: &Database,
    fixture: &DelegatedVerificationFixture,
) {
    let backend = receiver.require_local_engine().unwrap();
    assert_eq!(
        backend
            .get_verification_status(&fixture.delegated_entry_id)
            .await
            .unwrap(),
        VerificationStatus::Unverified,
        "missing delegated history must remain retryable"
    );
    assert!(
        !target_db
            .snapshot()
            .await
            .unwrap()
            .contains(&fixture.delegated_entry_id),
        "an entry with incomplete delegated proof must stay outside the Verified frontier"
    );
}

async fn ingest_delegated_history(receiver: &Instance, entries: impl IntoIterator<Item = Entry>) {
    let backend = receiver.require_local_engine().unwrap();
    for entry in entries {
        backend.put(entry).await.unwrap();
    }
}

async fn delegated_dependency_error(
    receiver: &Instance,
    fixture: &DelegatedVerificationFixture,
) -> Error {
    let target_db = Database::open(receiver, &fixture.target_root)
        .await
        .unwrap();
    let settings = target_db
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();
    AuthValidator::new()
        .validate_entry(&fixture.target_entry, &settings, Some(receiver))
        .await
        .expect_err("incomplete delegated history must surface its dependency")
}

fn assert_delegated_dependency(err: &Error, expected_tree: &ID, expected_missing: &[ID]) {
    match err {
        Error::Auth(auth) => match &**auth {
            crate::auth::errors::AuthError::DelegatedTreeUnsynced { tree_id, missing } => {
                assert_eq!(
                    tree_id, expected_tree,
                    "dependency must name the delegated database"
                );
                assert_eq!(
                    missing, expected_missing,
                    "dependency must name the exact known gaps"
                );
            }
            other => panic!("expected DelegatedTreeUnsynced, got: {other:?}"),
        },
        other => panic!("expected DelegatedTreeUnsynced, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_remote_ingest_missing_delegated_root_stays_retryable() {
    let fixture = delegated_verification_fixture().await;
    let (receiver, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("receiver"),
    )
    .await
    .unwrap();

    let target_db = ingest_target_fixture(&receiver, &fixture).await;
    assert_retryable_and_invisible(&receiver, &target_db, &fixture).await;
    let missing = Snapshot::from(vec![
        fixture.delegated_root.clone(),
        fixture.delegated_entries.last().unwrap().id(),
    ]);
    assert_delegated_dependency(
        &delegated_dependency_error(&receiver, &fixture).await,
        &fixture.delegated_root,
        &missing,
    );

    ingest_delegated_history(&receiver, fixture.delegated_entries.clone()).await;
    Database::open(&receiver, &fixture.delegated_root)
        .await
        .unwrap()
        .verify()
        .await
        .unwrap();
    let report = target_db.verify().await.unwrap();
    assert_eq!(
        report.failed, 0,
        "completed proof must not fail: {report:?}"
    );
    assert_eq!(
        receiver
            .require_local_engine()
            .unwrap()
            .get_verification_status(&fixture.delegated_entry_id)
            .await
            .unwrap(),
        VerificationStatus::Verified,
        "the retained entry must promote after its delegated tree arrives"
    );
}

#[tokio::test]
async fn test_remote_ingest_missing_delegated_tip_stays_retryable_then_promotes() {
    let fixture = delegated_verification_fixture().await;
    let (receiver, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("receiver"),
    )
    .await
    .unwrap();

    ingest_delegated_history(&receiver, fixture.delegated_entries.iter().take(1).cloned()).await;
    let target_db = ingest_target_fixture(&receiver, &fixture).await;
    assert_retryable_and_invisible(&receiver, &target_db, &fixture).await;
    let missing_tip = fixture.delegated_entries.last().unwrap().id();
    assert_delegated_dependency(
        &delegated_dependency_error(&receiver, &fixture).await,
        &fixture.delegated_root,
        std::slice::from_ref(&missing_tip),
    );

    ingest_delegated_history(&receiver, fixture.delegated_entries.iter().skip(1).cloned()).await;
    Database::open(&receiver, &fixture.delegated_root)
        .await
        .unwrap()
        .verify()
        .await
        .unwrap();
    let report = target_db.verify().await.unwrap();
    assert_eq!(
        report.failed, 0,
        "completed proof must not fail: {report:?}"
    );
    assert_eq!(
        receiver
            .require_local_engine()
            .unwrap()
            .get_verification_status(&fixture.delegated_entry_id)
            .await
            .unwrap(),
        VerificationStatus::Verified,
        "the retained entry must promote after its delegated proof arrives"
    );
}

#[tokio::test]
async fn test_remote_ingest_missing_delegated_intermediate_stays_retryable_then_promotes() {
    let fixture = delegated_verification_fixture().await;
    assert!(
        fixture.delegated_entries.len() >= 2,
        "fixture needs delegated root and a descendant tip"
    );
    let (receiver, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("receiver"),
    )
    .await
    .unwrap();

    let root = fixture.delegated_entries.first().unwrap().clone();
    let tip = fixture.delegated_entries.last().unwrap().clone();
    ingest_delegated_history(&receiver, [root]).await;
    ingest_delegated_history(&receiver, [tip]).await;
    let target_db = ingest_target_fixture(&receiver, &fixture).await;
    assert_retryable_and_invisible(&receiver, &target_db, &fixture).await;
    let missing_intermediate: Vec<ID> = fixture
        .delegated_entries
        .iter()
        .skip(1)
        .take(fixture.delegated_entries.len() - 2)
        .map(Entry::id)
        .collect();
    assert_delegated_dependency(
        &delegated_dependency_error(&receiver, &fixture).await,
        &fixture.delegated_root,
        &missing_intermediate,
    );

    ingest_delegated_history(
        &receiver,
        fixture
            .delegated_entries
            .iter()
            .skip(1)
            .take(fixture.delegated_entries.len() - 2)
            .cloned(),
    )
    .await;
    Database::open(&receiver, &fixture.delegated_root)
        .await
        .unwrap()
        .verify()
        .await
        .unwrap();
    let report = target_db.verify().await.unwrap();
    assert_eq!(
        report.failed, 0,
        "completed proof must not fail: {report:?}"
    );
    assert_eq!(
        receiver
            .require_local_engine()
            .unwrap()
            .get_verification_status(&fixture.delegated_entry_id)
            .await
            .unwrap(),
        VerificationStatus::Verified,
        "the retained entry must promote after its delegated proof arrives"
    );
}

#[tokio::test]
async fn test_local_commit_missing_delegated_history_is_rejected_without_storage() {
    let fixture = delegated_verification_fixture().await;
    let (receiver, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("receiver"),
    )
    .await
    .unwrap();
    for entry in fixture.target_history.clone() {
        put_verified(&receiver, entry).await;
    }

    let target = Database::open(&receiver, &fixture.target_root)
        .await
        .unwrap()
        .with_key(DatabaseKey::with_identity(
            fixture.delegated_signing_key.clone(),
            fixture.target_entry.auth().key.clone(),
        ));
    let txn = target.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("local", "missing proof")
        .await
        .unwrap();
    let err = txn
        .commit()
        .await
        .expect_err("local commit must fail closed when delegated proof is unavailable");
    assert!(
        matches!(
            err,
            Error::Auth(ref auth) if auth.is_delegated_tree_unsynced()
        ),
        "local commit should surface the retriable proof error, got: {err}"
    );
    let backend = receiver.require_local_engine().unwrap();
    assert_eq!(
        backend.get_tree(&fixture.target_root).await.unwrap().len(),
        fixture.target_history.len(),
        "a failed local commit must not store an unverifiable entry"
    );
}

#[tokio::test]
async fn test_remote_ingest_invalid_delegated_signature_fails_definitively() {
    let fixture = delegated_verification_fixture().await;
    let (receiver, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("receiver"),
    )
    .await
    .unwrap();
    ingest_delegated_history(&receiver, fixture.delegated_entries.clone()).await;
    Database::open(&receiver, &fixture.delegated_root)
        .await
        .unwrap()
        .verify()
        .await
        .unwrap();
    for entry in fixture.target_history.clone() {
        put_verified(&receiver, entry).await;
    }

    let wrong_key = PrivateKey::generate();
    let invalid = fixture.target_entry.clone().with_auth(|auth| {
        auth.signature = Some(sign_entry(&fixture.target_entry, &wrong_key).unwrap())
    });
    let invalid_id = invalid.id();
    receiver
        .put_remote_entries(&fixture.target_root, vec![invalid])
        .await
        .unwrap();

    assert_eq!(
        receiver
            .require_local_engine()
            .unwrap()
            .get_verification_status(&invalid_id)
            .await
            .unwrap(),
        VerificationStatus::Failed,
        "a definitively invalid signature must remain terminal"
    );
}

// ===== ids_added DAG-diff helper =====

#[tokio::test]
async fn test_ids_added_empty_when_cursors_equal() {
    let (_instance, db) = setup_callback_test().await;
    let tips = db.snapshot().await.unwrap();
    let added = db.ids_added(&tips, &tips).await.unwrap();
    assert!(added.is_empty(), "equal cursors should yield empty diff");
}

#[tokio::test]
async fn test_ids_added_single_commit() {
    let (_instance, db) = setup_callback_test().await;
    let prev = db.snapshot().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&prev, &post).await.unwrap();

    assert_eq!(
        added,
        vec![id1.clone()],
        "single commit should add exactly the new entry"
    );
}

#[tokio::test]
async fn test_ids_added_multi_commit_topo_order() {
    let (_instance, db) = setup_callback_test().await;
    let prev = db.snapshot().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v3").await.unwrap();
    let id3 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&prev, &post).await.unwrap();

    assert_eq!(added.len(), 3, "three commits should add three entries");
    let pos1 = added.iter().position(|i| i == &id1).expect("id1 in added");
    let pos2 = added.iter().position(|i| i == &id2).expect("id2 in added");
    let pos3 = added.iter().position(|i| i == &id3).expect("id3 in added");
    assert!(
        pos1 < pos2,
        "id1 (parent) must precede id2 (child) in topo order"
    );
    assert!(
        pos2 < pos3,
        "id2 (parent) must precede id3 (child) in topo order"
    );
}

#[tokio::test]
async fn test_ids_added_skips_entries_before_cursor() {
    let (_instance, db) = setup_callback_test().await;

    // Commit one entry, then snapshot the cursor *after* it.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();
    let after_first = db.snapshot().await.unwrap();

    // Two more commits past that cursor.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v3").await.unwrap();
    let id3 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&after_first, &post).await.unwrap();

    assert!(!added.contains(&id1), "entry at cursor must be excluded");
    assert!(
        added.contains(&id2),
        "post-cursor entry id2 must be included"
    );
    assert!(
        added.contains(&id3),
        "post-cursor entry id3 must be included"
    );
    assert_eq!(added.len(), 2, "exactly the two post-cursor entries");
}

#[tokio::test]
async fn test_ids_added_empty_previous_returns_full_closure() {
    let (_instance, db) = setup_callback_test().await;

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&Snapshot::EMPTY, &post).await.unwrap();

    // With empty cursor, every ancestor reachable from post_tips is "added",
    // which for a fresh database is the root + every committed entry.
    assert!(
        added.contains(&id1),
        "empty cursor should include all reachable entries; missing id1"
    );
    assert!(
        added.contains(db.root_id()),
        "empty cursor should include the root entry"
    );
}

/// Regression: a **backward** cursor bracket — `previous_tips` strictly newer
/// than `post_tips` — must not expand into the full ancestor closure.
///
/// `ids_added` walks parents from `post_tips` and halts at members of
/// `previous_tips`. When `previous_tips` is newer, its members are
/// *descendants* of the walk's starting point, so the walk never reaches the
/// boundary and runs all the way to the root — reporting the entire history as
/// "added". The doc comment on `ids_added` claims a stale `previous_tips` "can
/// only over-report ... the conservative direction"; that reasoning covers a
/// stale-behind cursor, not one that is ahead of `post_tips`, where the
/// over-report degenerates to O(full history). On a connected instance every
/// step is a permission-checked wire round-trip, so this is a fetch storm.
///
/// Nothing was *added* going backward, so the correct answer is empty.
#[tokio::test]
async fn test_ids_added_backward_bracket_does_not_report_full_history() {
    let (_instance, db) = setup_callback_test().await;

    let mut committed = Vec::new();
    let mut early: Option<Snapshot> = None;
    for i in 0..6 {
        let txn = db.new_transaction().await.unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        store.set("k", format!("v{i}")).await.unwrap();
        committed.push(txn.commit().await.unwrap());
        if i == 1 {
            early = Some(db.snapshot().await.unwrap());
        }
    }
    let early = early.unwrap();
    let late = db.snapshot().await.unwrap();
    assert_ne!(early, late, "test setup: cursors must differ");

    // Backward bracket: previous = the LATER cursor, post = the EARLIER one.
    let added = db.ids_added(&late, &early).await.unwrap();

    assert!(
        added.is_empty(),
        "backward bracket must report nothing added; got {} ids for a {}-commit tree \
         (full-history walk: root + every entry)",
        added.len(),
        committed.len()
    );
}

// Latest-Snapshot watch regressions (including Unverified/Failed exclusion)
// live in tests/it/database/watches.rs and instance/watch.rs.

/// An explicit historical boundary bypasses access-time auto-verification,
/// so this checks the commit's parent gate rather than a healed live snapshot.
/// A cached floor from before demotion must not promote a child over an
/// Unverified parent and hide that parent from the targeted verify walk.
#[tokio::test]
async fn test_local_historical_commit_rejects_unverified_parent_without_storage() {
    let (instance, db) = setup_callback_test().await;
    let txn = db.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("k", "v1")
        .await
        .unwrap();
    let parent = txn.commit().await.unwrap();
    instance
        .demote_to_unverified(db.root_id(), &parent)
        .await
        .unwrap();
    let engine = instance.require_local_engine().unwrap();
    let before = engine.get_tree(db.root_id()).await.unwrap().len();
    let boundary = Snapshot::from([parent.clone()]);
    let txn = db.new_transaction_at(&boundary).await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("k", "v2")
        .await
        .unwrap();
    let error = txn
        .commit()
        .await
        .expect_err("a cached floor is not a Verified parent");
    assert!(matches!(error, Error::Auth(e) if e.is_delegated_tree_unsynced()));
    assert_eq!(
        engine.get_verification_status(&parent).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(
        engine.get_tree(db.root_id()).await.unwrap().len(),
        before,
        "rejected local child must not be stored"
    );

    let report = db.verify().await.unwrap();
    assert_eq!(report.verified, 1);
    assert_eq!(report.failed, 0);
    let txn = db.new_transaction_at(&boundary).await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("k", "v2")
        .await
        .unwrap();
    let child = txn.commit().await.unwrap();
    assert_eq!(
        engine.get_verification_status(&child).await.unwrap(),
        VerificationStatus::Verified
    );
}

// ===== Height monotonicity at ingest =====

/// Re-encode `entry` with `height` and re-sign it with `key`: a validly signed
/// entry whose only defect is the height label, as a malicious writer holding
/// a legitimate key could produce.
fn with_crafted_height(entry: &Entry, height: u64, key: &PrivateKey) -> Entry {
    let mut value = serde_json::to_value(entry).unwrap();
    value["tree"]["h"] = serde_json::json!(height);
    let crafted: Entry = serde_json::from_value(value).unwrap();
    assert_eq!(
        crafted.height(),
        height,
        "test setup: height must be rewritten"
    );
    let crafted = crafted.with_auth(|auth| auth.signature = None);
    let signature = sign_entry(&crafted, key).unwrap();
    crafted.with_auth(|auth| auth.signature = Some(signature))
}

/// Commit three entries and return the tip, its parent, and a signing key the
/// database accepts. The tip is the template for crafted siblings.
async fn setup_height_test() -> (Instance, Database, PrivateKey, Entry, Entry) {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let (signing_key, _) = generate_keypair();
    let db = Database::create(&instance, signing_key.clone(), Doc::new())
        .await
        .unwrap();
    let mut ids = Vec::new();
    for i in 0..3 {
        let txn = db.new_transaction().await.unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        store.set("k", format!("v{i}")).await.unwrap();
        ids.push(txn.commit().await.unwrap());
    }
    let tip = instance.get(&ids[2]).await.unwrap();
    let parent = instance.get(&ids[1]).await.unwrap();
    assert_eq!(tip.parents().unwrap(), vec![parent.id()]);
    assert!(
        parent.height() > 0,
        "test setup: parent must be above the root"
    );
    (instance, db, signing_key, tip, parent)
}

async fn ingest_status(instance: &Instance, db: &Database, entry: Entry) -> VerificationStatus {
    let id = entry.id();
    instance
        .put_remote_entries(db.root_id(), vec![entry])
        .await
        .unwrap();
    instance
        .require_local_engine()
        .unwrap()
        .get_verification_status(&id)
        .await
        .unwrap()
}

/// A validly signed entry whose height does not exceed its parent's is
/// rejected at verification, both below and equal to the parent height.
#[tokio::test]
async fn test_verify_fails_entry_with_non_monotone_height() {
    let (instance, db, key, tip, parent) = setup_height_test().await;

    for height in [0, parent.height()] {
        let crafted = with_crafted_height(&tip, height, &key);
        assert_eq!(
            ingest_status(&instance, &db, crafted).await,
            VerificationStatus::Failed,
            "entry with height {height} under a parent of height {} must fail",
            parent.height()
        );
    }
}

/// Heights above `parent + 1` are legitimate (the timestamp strategy produces
/// them), so the check must accept them; this also proves the re-signed
/// template is otherwise valid.
#[tokio::test]
async fn test_verify_accepts_entry_with_height_gap() {
    let (instance, db, key, tip, parent) = setup_height_test().await;

    let crafted = with_crafted_height(&tip, parent.height() + 1_000, &key);
    assert_eq!(
        ingest_status(&instance, &db, crafted).await,
        VerificationStatus::Verified
    );
}

/// The `ids_added` backward-bracket guard can skip a crafted non-monotone
/// entry. Verification rejects that entry, keeping it out of the default
/// Verified frontier even though it remains in the raw DAG.
#[tokio::test]
async fn test_ids_added_guard_hides_only_failed_crafted_entries() {
    let (instance, db, key, tip, parent) = setup_height_test().await;

    let crafted = with_crafted_height(&tip, 0, &key);
    let crafted_id = crafted.id();
    assert_eq!(
        ingest_status(&instance, &db, crafted).await,
        VerificationStatus::Failed
    );

    let prev = Snapshot::new(vec![parent.id()]);
    let post = Snapshot::new(vec![crafted_id]);
    let added = db.ids_added(&prev, &post).await.unwrap();
    assert!(
        added.is_empty(),
        "guard prunes the crafted entry by height; got {added:?}"
    );
}

fn with_height_test_parents(entry: &Entry, parents: &[ID]) -> Entry {
    let mut value = serde_json::to_value(entry).unwrap();
    value["tree"]["parents"] = serde_json::json!(parents);
    serde_json::from_value(value).unwrap()
}

/// Every main parent matters, regardless of parent order. The valid merge
/// control has the same ancestry and auth metadata as the rejected merges.
#[tokio::test]
async fn test_verify_height_checks_every_parent() {
    let (instance, db, key, tip, parent) = setup_height_test().await;
    let high = with_crafted_height(&tip, parent.height() + 100, &key);
    assert_eq!(
        ingest_status(&instance, &db, high.clone()).await,
        VerificationStatus::Verified
    );

    for parents in [vec![parent.id(), high.id()], vec![high.id(), parent.id()]] {
        let template = with_height_test_parents(&tip, &parents);
        for height in [parent.height() + 1, high.height()] {
            let invalid = with_crafted_height(&template, height, &key);
            assert_eq!(
                ingest_status(&instance, &db, invalid).await,
                VerificationStatus::Failed
            );
        }
        let valid = with_crafted_height(&template, high.height() + 1, &key);
        assert_eq!(
            ingest_status(&instance, &db, valid).await,
            VerificationStatus::Verified
        );
    }
}

/// Child-first ingest must not confuse an unknown height with an invalid one.
/// When the parent arrives, re-verification rejects bad heights and descendants,
/// but promotes the otherwise identical monotone branch.
#[tokio::test]
async fn test_verify_height_after_missing_parent_arrives() {
    for monotone in [false, true] {
        let (instance, db, key, tip, parent) = setup_height_test().await;
        let late_parent = with_crafted_height(&tip, parent.height() + 100, &key);
        let template = with_height_test_parents(&tip, &[late_parent.id()]);
        let height = late_parent.height() + u64::from(monotone);
        let child = with_crafted_height(&template, height, &key);
        let template = with_height_test_parents(&tip, &[child.id()]);
        let descendant = with_crafted_height(&template, height + 1, &key);
        let engine = instance.require_local_engine().unwrap();

        // Reverse arrival order exercises the topological verification walk,
        // not height sorting: equal/decreasing labels cannot fool it.
        for entry in [descendant.clone(), child.clone()] {
            assert_eq!(
                ingest_status(&instance, &db, entry).await,
                VerificationStatus::Unverified
            );
        }
        assert_eq!(
            ingest_status(&instance, &db, late_parent.clone()).await,
            VerificationStatus::Verified
        );
        let expected = if monotone {
            VerificationStatus::Verified
        } else {
            VerificationStatus::Failed
        };
        for entry in [&child, &descendant] {
            assert_eq!(
                engine.get_verification_status(&entry.id()).await.unwrap(),
                expected
            );
            assert_eq!(
                engine.get(&entry.id()).await.unwrap().id(),
                entry.id(),
                "ingest retains rejected bytes"
            );
        }
        let snapshot = db.snapshot().await.unwrap();
        assert_eq!(snapshot.tips().contains(&descendant.id()), monotone);
        assert_eq!(snapshot.tips().contains(&late_parent.id()), !monotone);
        // Re-receipt cannot erase a rejection (or demote an accepted entry).
        assert_eq!(ingest_status(&instance, &db, child).await, expected);
    }
}

/// A known bad parent-height edge is definitive even if a different parent
/// has not arrived yet; missing proof must not mask a provable violation.
#[tokio::test]
async fn test_verify_height_failure_is_not_masked_by_missing_parent() {
    let (instance, db, key, tip, parent) = setup_height_test().await;
    let missing = with_crafted_height(&tip, parent.height() + 100, &key);
    for parents in [
        vec![parent.id(), missing.id()],
        vec![missing.id(), parent.id()],
    ] {
        let template = with_height_test_parents(&tip, &parents);
        let child = with_crafted_height(&template, parent.height(), &key);
        assert_eq!(
            ingest_status(&instance, &db, child).await,
            VerificationStatus::Failed
        );
    }
}
