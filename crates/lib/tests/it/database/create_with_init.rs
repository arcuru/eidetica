//! Tests for `Database::create_with_init` — the constructor that runs a
//! caller-supplied initialization callback inside the genesis transaction so
//! initial subtree writes fold into the same entry that establishes the
//! database root.

use eidetica::{Database, Store, auth::crypto::generate_keypair, crdt::Doc, store::DocStore};

use crate::helpers::test_local_instance;

#[tokio::test]
async fn test_init_callback_runs_and_writes_persist() {
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let database = Database::create_with_init(&instance, signing_key, Doc::new(), async |txn| {
        let store = DocStore::open(txn, "config").await?;
        store.set("name", "agent_db").await?;
        store.set("version", "1").await?;
        Ok(())
    })
    .await
    .expect("create_with_init failed");

    // Read back through a fresh transaction — the writes from the init callback
    // should be persisted in the genesis entry.
    let txn = database.new_transaction().await.unwrap();
    let store = DocStore::open(&txn, "config").await.unwrap();
    assert_eq!(store.get("name").await.unwrap(), "agent_db");
    assert_eq!(store.get("version").await.unwrap(), "1");
}

#[tokio::test]
async fn test_genesis_entry_carries_all_initialized_subtrees() {
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let database = Database::create_with_init(&instance, signing_key, Doc::new(), async |txn| {
        DocStore::open(txn, "config").await?.set("a", "1").await?;
        DocStore::open(txn, "meta").await?.set("b", "2").await?;
        Ok(())
    })
    .await
    .unwrap();

    // The genesis entry (the database root) should itself contain the
    // initialized subtrees — this is the load-bearing atomicity claim:
    // initial stores live in the same entry as _settings and _root.
    let root_id = database.root_id();
    let genesis = database.get_entry(root_id).await.unwrap();
    let subtrees = genesis.subtrees();

    assert!(subtrees.iter().any(|s| s == "_settings"));
    assert!(subtrees.iter().any(|s| s == "config"));
    assert!(subtrees.iter().any(|s| s == "meta"));
    // _index gets registered when the first non-system subtree is opened with
    // register; it should also be in the genesis entry.
    assert!(subtrees.iter().any(|s| s == "_index"));
}

#[tokio::test]
async fn test_init_callback_error_aborts_create() {
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let result = Database::create_with_init(&instance, signing_key, Doc::new(), async |_txn| {
        Err(std::io::Error::other("init failed").into())
    })
    .await;

    assert!(
        result.is_err(),
        "create_with_init should propagate init errors"
    );
}

#[tokio::test]
async fn test_create_delegates_to_create_with_init() {
    // Plain `Database::create` should behave identically to
    // `create_with_init` with a no-op callback: only `_settings` and `_root`
    // in the genesis entry, no `_index` (since no subtrees were touched).
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let database = Database::create(&instance, signing_key, Doc::new())
        .await
        .unwrap();

    let root_id = database.root_id();
    let genesis = database.get_entry(root_id).await.unwrap();
    let subtrees = genesis.subtrees();

    assert!(subtrees.iter().any(|s| s == "_settings"));
    assert!(!subtrees.iter().any(|s| s == "_index"));
}

#[tokio::test]
async fn test_init_callback_cannot_open_settings() {
    // The init callback runs with system subtrees locked, so any attempt to
    // open `_settings` via the public Store API must fail. This prevents an
    // init closure from clobbering the settings Doc that `create_with_init`
    // staged from its `initial_settings` argument.
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let result = Database::create_with_init(&instance, signing_key, Doc::new(), async |txn| {
        let _ = DocStore::open(txn, "_settings").await?;
        Ok(())
    })
    .await;

    assert!(
        result.is_err(),
        "create_with_init should reject _settings opens inside init callback"
    );
}

#[tokio::test]
async fn test_init_callback_cannot_open_root() {
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let result = Database::create_with_init(&instance, signing_key, Doc::new(), async |txn| {
        let _ = DocStore::open(txn, "_root").await?;
        Ok(())
    })
    .await;

    assert!(
        result.is_err(),
        "create_with_init should reject _root opens inside init callback"
    );
}

#[tokio::test]
async fn test_init_callback_cannot_open_index() {
    // `_index` is touched legitimately by `Store::register` via `Registry`
    // (which constructs DocStore directly via `DocStore::load`, bypassing
    // `get_store`). The public `get_store`/`Store::open` path must still
    // reject `_index` so callers can't manipulate the index directly.
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let result = Database::create_with_init(&instance, signing_key, Doc::new(), async |txn| {
        let _ = DocStore::open(txn, "_index").await?;
        Ok(())
    })
    .await;

    assert!(
        result.is_err(),
        "create_with_init should reject _index opens inside init callback"
    );
}

#[tokio::test]
async fn test_init_callback_can_register_normal_stores_while_locked() {
    // The lock only blocks `_*` names. Registering a normal store still
    // writes to `_index` internally (via `Registry::set_entry` →
    // `DocStore::set` → `update_subtree("_index", ...)`), but that path
    // bypasses `get_store` and is unaffected by the lock.
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let database = Database::create_with_init(&instance, signing_key, Doc::new(), async |txn| {
        DocStore::open(txn, "config").await?.set("k", "v").await?;
        Ok(())
    })
    .await
    .expect("normal store registration must work despite the lock");

    let genesis = database.get_entry(database.root_id()).await.unwrap();
    let subtrees = genesis.subtrees();
    assert!(subtrees.iter().any(|s| s == "config"));
    assert!(subtrees.iter().any(|s| s == "_index"));
}

#[tokio::test]
async fn test_system_subtree_lock_released_after_init() {
    // After `create_with_init` returns, transactions opened on the resulting
    // Database are unlocked: callers can still open `_settings` via
    // `get_store::<DocStore>` (relied on by tests and helpers throughout the
    // crate). The lock is scoped strictly to the init callback.
    let instance = test_local_instance().await;
    let (signing_key, _) = generate_keypair();

    let database = Database::create_with_init(&instance, signing_key, Doc::new(), async |_| Ok(()))
        .await
        .unwrap();

    let txn = database.new_transaction().await.unwrap();
    let _settings = DocStore::open(&txn, "_settings")
        .await
        .expect("post-init transactions must not be locked");
}
