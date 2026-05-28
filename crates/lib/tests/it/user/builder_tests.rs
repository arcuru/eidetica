//! Tests for `User::new_database()` — the chainable `DatabaseBuilder` API.
//!
//! Covers builder ergonomics, atomic multi-store initialization, the
//! extension-trait pattern (`DocStoreInit::initialize_doc` / `empty_doc`),
//! collision detection, and the user-side tracking write.

use eidetica::{
    Store,
    crdt::Doc,
    store::{DocStore, DocStoreInit, PasswordStore},
};

use super::helpers::*;

#[tokio::test]
async fn test_builder_creates_basic_database() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let (db, _key) = user
        .new_database()
        .name("test-builder-db")
        .build()
        .await
        .expect("builder.build failed");

    assert!(!db.root_id().to_string().is_empty());
}

#[tokio::test]
async fn test_builder_empty_doc_registers_store() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let (db, _) = user
        .new_database()
        .name("with-empty-store")
        .empty_doc("config")
        .build()
        .await
        .unwrap();

    // The genesis entry should include `config` as a subtree even though no
    // data was written into it — empty_doc registers the store.
    let genesis = db.get_entry(db.root_id()).await.unwrap();
    let subtrees = genesis.subtrees();
    assert!(subtrees.iter().any(|s| s == "config"));
}

#[tokio::test]
async fn test_builder_initialize_doc_persists_value() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let mut meta = Doc::new();
    meta.set("name", "agent");
    meta.set("version", "1");

    let (db, _) = user
        .new_database()
        .name("with-meta")
        .initialize_doc("meta", meta)
        .build()
        .await
        .unwrap();

    // Each top-level key from the input Doc lands as a DocStore key.
    let txn = db.new_transaction().await.unwrap();
    let store = DocStore::open(&txn, "meta").await.unwrap();
    assert_eq!(store.get_string("name").await.unwrap(), "agent");
    assert_eq!(store.get_string("version").await.unwrap(), "1");
}

#[tokio::test]
async fn test_builder_initialize_doc_preserves_nested_values() {
    // Per-key Value cloning in `initialize_doc` should preserve branch values
    // (nested Doc, List) as well as scalars — the contract is "each top-level
    // key of the input Doc becomes a top-level key of the DocStore, with the
    // original Value variant intact".
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let mut nested = Doc::new();
    nested.set("inner", "deep");
    nested.set("count", 42i64);

    let mut meta = Doc::new();
    meta.set("scalar", "top-level");
    meta.set("nested", nested);

    let (db, _) = user
        .new_database()
        .name("nested-doc")
        .initialize_doc("meta", meta)
        .build()
        .await
        .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = DocStore::open(&txn, "meta").await.unwrap();

    // Top-level scalar round-trips.
    assert_eq!(store.get_string("scalar").await.unwrap(), "top-level");

    // Nested Doc round-trips: walk the path to confirm the branch wasn't
    // flattened or stringified by `initialize_doc`.
    let inner = store.get_path("nested.inner").await.unwrap();
    assert_eq!(inner, "deep");
    let count = store.get_path("nested.count").await.unwrap();
    assert_eq!(count, 42i64);
}

#[tokio::test]
async fn test_builder_multiple_stores_in_single_genesis_entry() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let (db, _) = user
        .new_database()
        .name("multi-store")
        .empty_doc("config")
        .empty_doc("logs")
        .initialize_store::<DocStore, _, _>("custom", |s| async move { s.set("k", "v").await })
        .build()
        .await
        .unwrap();

    // Atomicity proof: all three stores live in the single genesis entry.
    let genesis = db.get_entry(db.root_id()).await.unwrap();
    let subtrees = genesis.subtrees();
    assert!(subtrees.iter().any(|s| s == "config"));
    assert!(subtrees.iter().any(|s| s == "logs"));
    assert!(subtrees.iter().any(|s| s == "custom"));
    assert!(subtrees.iter().any(|s| s == "_settings"));
    assert!(subtrees.iter().any(|s| s == "_index"));
}

#[tokio::test]
async fn test_builder_name_after_settings_overrides_name_in_doc() {
    // Order-dependent contract: `.settings(d)` REPLACES the in-progress
    // settings, so a prior `.name("x")` is discarded; a subsequent `.name("y")`
    // mutates the freshly-installed settings.
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Case 1: settings AFTER name → settings wins (name dropped).
    let mut d = Doc::new();
    d.set("name", "from-doc");
    d.set("extra", "kept");
    let (db1, _) = user
        .new_database()
        .name("from-builder")
        .settings(d)
        .build()
        .await
        .unwrap();
    assert_eq!(db1.get_name().await.unwrap(), "from-doc");

    // Case 2: name AFTER settings → name mutates the installed Doc.
    let mut d2 = Doc::new();
    d2.set("name", "from-doc-2");
    d2.set("extra", "still-kept");
    let (db2, _) = user
        .new_database()
        .settings(d2)
        .name("late-name")
        .build()
        .await
        .unwrap();
    assert_eq!(db2.get_name().await.unwrap(), "late-name");
    // Non-name fields installed via settings() survive the name() call.
    let settings = db2.get_settings().await.unwrap();
    assert_eq!(settings.get_string("extra").await.unwrap(), "still-kept");
}

#[tokio::test]
async fn test_builder_with_key_rejects_key_not_in_user_keystore() {
    // `with_key` accepts a PublicKey but doesn't verify ownership at build
    // time; the failure surfaces inside `build()` when the underlying
    // `create_database_with_init` tries to resolve the signing key.
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Generate a key OUTSIDE the user's key manager.
    let (_, foreign_key) = eidetica::auth::crypto::generate_keypair();

    let result = user
        .new_database()
        .name("foreign-key")
        .with_key(foreign_key)
        .build()
        .await;

    assert!(
        result.is_err(),
        "builder.build should reject a key not in the user's key manager"
    );
}

#[tokio::test]
async fn test_builder_with_existing_key() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Pre-generate a key
    let key_id = user.add_private_key(Some("explicit-key")).await.unwrap();
    let key_id_clone = key_id.clone();

    let (_db, returned_key) = user
        .new_database()
        .name("with-explicit-key")
        .with_key(key_id)
        .build()
        .await
        .unwrap();

    // The builder should return the same key it was given.
    assert_eq!(returned_key, key_id_clone);
}

#[tokio::test]
async fn test_builder_auto_generated_key_uses_label() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let initial_keys = user.list_keys().expect("list_keys").len();

    let (_db, _key) = user
        .new_database()
        .name("with-labeled-key")
        .key_label("labeled-builder-key")
        .build()
        .await
        .unwrap();

    // The builder generated exactly one new key.
    let after_keys = user.list_keys().expect("list_keys").len();
    assert_eq!(after_keys, initial_keys + 1);
}

#[tokio::test]
async fn test_builder_duplicate_store_name_errors() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let result = user
        .new_database()
        .name("dup-store")
        .empty_doc("config")
        .empty_doc("config")
        .build()
        .await;

    assert!(
        result.is_err(),
        "builder.build should reject duplicate store names"
    );
}

#[tokio::test]
async fn test_builder_init_callback_failure_aborts() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let initial_dbs = user.databases().await.expect("databases").len();

    let result = user
        .new_database()
        .name("doomed")
        .initialize_store::<DocStore, _, _>("config", |_s| async move {
            Err(std::io::Error::other("init failed").into())
        })
        .build()
        .await;

    assert!(result.is_err());

    // No database should have been tracked for the user.
    let after_dbs = user.databases().await.expect("databases").len();
    assert_eq!(after_dbs, initial_dbs);
}

#[tokio::test]
async fn test_builder_auto_generated_key_persists_after_init_failure() {
    // Documents the accepted tradeoff: the builder calls `add_private_key`
    // BEFORE running store initializers, so an init failure leaves the
    // generated key in the user's key store. `UserKeyManager` does not
    // expose a removal API, and the same leak exists for the pre-existing
    // `add_private_key` + `create_database` flow. If this changes
    // (rollback or deferred key generation), this assert must flip.
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let initial_keys = user.list_keys().expect("list_keys").len();

    let result = user
        .new_database()
        .name("leaky")
        .key_label("leaked-key")
        .initialize_store::<DocStore, _, _>("config", |_s| async move {
            Err(std::io::Error::other("init failed").into())
        })
        .build()
        .await;
    assert!(result.is_err());

    let after_keys = user.list_keys().expect("list_keys").len();
    assert_eq!(
        after_keys,
        initial_keys + 1,
        "auto-generated key persists when builder.build fails"
    );
}

#[tokio::test]
async fn test_builder_writes_tracked_database_row() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let initial_count = user.databases().await.expect("databases").len();

    let (db, _) = user
        .new_database()
        .name("tracked-db")
        .empty_doc("config")
        .build()
        .await
        .unwrap();

    let after = user.databases().await.expect("databases");
    assert_eq!(after.len(), initial_count + 1);
    assert!(
        after
            .iter()
            .any(|tracked| tracked.database_id == *db.root_id())
    );
}

// ===== PasswordStore via the builder =====
//
// `PasswordStore<S>` overrides `Store::register` to mark itself as
// Uninitialized (empty config in `_index`). The builder path calls
// `S::open` → `register` for new subtrees, so the builder should produce a
// PasswordStore in the Uninitialized state. These tests confirm that path
// works end-to-end: register through the builder, initialize encryption in
// a follow-up transaction, write encrypted data, and verify decryption on
// reopen.

#[tokio::test]
async fn test_builder_registers_password_store_uninitialized() {
    // Registering `PasswordStore<DocStore>` via the builder lands the store
    // in the Uninitialized state — its `_index` config is empty until a
    // caller invokes `initialize(password, ...)` to set up encryption.
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let (db, _) = user
        .new_database()
        .name("with-password-store")
        .initialize_store::<PasswordStore<DocStore>, _, _>("secrets", |_| async move {
            // No-op: registration only. Encryption initialization happens
            // in a follow-up transaction with the password.
            Ok(())
        })
        .build()
        .await
        .expect("builder.build with PasswordStore registration failed");

    // The genesis entry should carry the `secrets` subtree alongside the
    // standard system subtrees.
    let genesis = db.get_entry(db.root_id()).await.unwrap();
    let subtrees = genesis.subtrees();
    assert!(subtrees.iter().any(|s| s == "secrets"));
    assert!(subtrees.iter().any(|s| s == "_index"));
}

#[tokio::test]
async fn test_builder_password_store_initialize_and_roundtrip() {
    // End-to-end: builder registers the store, a follow-up transaction
    // initializes encryption and writes a value, a third transaction
    // reopens with the password and reads the plaintext back. This proves
    // the builder's registration is wire-compatible with PasswordStore's
    // post-genesis initialization flow.
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let (db, _) = user
        .new_database()
        .name("password-roundtrip")
        .initialize_store::<PasswordStore<DocStore>, _, _>("secrets", |_| async move { Ok(()) })
        .build()
        .await
        .unwrap();

    // Initialize encryption + write a value. Uses the standard PasswordStore
    // post-genesis flow.
    {
        let txn = db.new_transaction().await.unwrap();
        let mut encrypted = txn
            .get_store::<PasswordStore<DocStore>>("secrets")
            .await
            .unwrap();
        encrypted.initialize("hunter2", Doc::new()).await.unwrap();
        let inner = encrypted.inner().await.unwrap();
        inner.set("api_key", "s3cr3t").await.unwrap();
        txn.commit().await.unwrap();
    }

    // Reopen, unlock, read.
    {
        let txn = db.new_transaction().await.unwrap();
        let mut encrypted = txn
            .get_store::<PasswordStore<DocStore>>("secrets")
            .await
            .unwrap();
        encrypted.open("hunter2").unwrap();
        let inner = encrypted.inner().await.unwrap();
        let value = inner.get("api_key").await.unwrap();
        assert_eq!(value.as_text(), Some("s3cr3t"));
    }
}

#[tokio::test]
async fn test_builder_password_store_wrong_password_rejected() {
    // After builder registration + encryption initialization, opening with
    // the wrong password must fail. This locks in the security contract:
    // the builder path doesn't accidentally bypass the password check.
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    let (db, _) = user
        .new_database()
        .name("password-wrong")
        .initialize_store::<PasswordStore<DocStore>, _, _>("secrets", |_| async move { Ok(()) })
        .build()
        .await
        .unwrap();

    {
        let txn = db.new_transaction().await.unwrap();
        let mut encrypted = txn
            .get_store::<PasswordStore<DocStore>>("secrets")
            .await
            .unwrap();
        encrypted
            .initialize("correct-password", Doc::new())
            .await
            .unwrap();
        txn.commit().await.unwrap();
    }

    let txn = db.new_transaction().await.unwrap();
    let mut encrypted = txn
        .get_store::<PasswordStore<DocStore>>("secrets")
        .await
        .unwrap();
    let result = encrypted.open("wrong-password");
    assert!(result.is_err(), "wrong password must be rejected");
}
