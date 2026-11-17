//! Literal map entries and path entries must remain distinct across signed sync.

use eidetica::{Cluster, crdt::doc::Value, store::DocStore};

use super::helpers::cluster_shared_database;

#[tokio::test]
async fn test_docstore_literal_keys_sync_and_concurrent_paths() {
    let mut net = Cluster::builder().peers(2).build().await.unwrap();
    let (root, dbs) = cluster_shared_database(&mut net, "literal-sync")
        .await
        .unwrap();
    let keys = ["", ".", "..", "a.b", ".a", "a.", "é", "e\u{301}", "日本.語"];
    let tx = dbs[0].new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    for key in keys {
        store.set_literal(key, key).await.unwrap();
    }
    store.set("a.b", "nested").await.unwrap();
    let id = tx.commit().await.unwrap();
    let original = dbs[0].get_entry(&id).await.unwrap();
    let original_bytes = original.to_dagcbor().unwrap();
    assert!(dbs[0].verify_entry_signature(&id).await.unwrap());
    net.exchange(0, 1, &root).await.unwrap();
    assert_eq!(dbs[1].get_entry(&id).await.unwrap(), original);
    let store = dbs[1].get_store_viewer::<DocStore>("data").await.unwrap();
    for key in keys {
        assert_eq!(store.get_literal(key).await.unwrap(), Value::from(key));
    }

    // Independent literal deletions and nested-path edits must not conflict.
    let left = dbs[0].new_transaction().await.unwrap();
    let left_store = left.get_store::<DocStore>("data").await.unwrap();
    for key in keys {
        assert!(left_store.delete_literal(key).await.unwrap());
    }
    left.commit().await.unwrap();
    let right = dbs[1].new_transaction().await.unwrap();
    right
        .get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("a.b", "changed nested")
        .await
        .unwrap();
    right.commit().await.unwrap();
    assert!(net.converge(&root).await.unwrap());
    for db in &dbs {
        let store = db.get_store_viewer::<DocStore>("data").await.unwrap();
        let all = store.get_all().await.unwrap();
        for key in keys {
            assert!(all.is_tombstone_literal(key));
            assert!(store.get_literal(key).await.is_err());
        }
        assert_eq!(store.get_string("a.b").await.unwrap(), "changed nested");
        // Historical Entry bytes and their authentication are not reconstructed
        // from the current merged document when reading or syncing.
        let historical = db.get_entry(&id).await.unwrap();
        assert_eq!(historical.to_dagcbor().unwrap(), original_bytes);
        assert_eq!(historical.id(), id);
        assert!(db.verify_entry_signature(&id).await.unwrap());
    }
}
