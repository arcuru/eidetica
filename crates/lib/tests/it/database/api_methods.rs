//! Tree API method tests
//!
//! This module contains tests for Tree API methods including entry retrieval,
//! authentication, validation, and error handling.

use eidetica::{
    Database, Instance, Snapshot,
    auth::{
        crypto::generate_keypair,
        types::{AuthKey, KeyStatus, Permission, SigKey},
    },
    backend::{VerificationStatus, database::InMemory},
    crdt::Doc,
    entry::ID,
    store::DocStore,
};

use super::helpers::*;
use crate::helpers::*;

/// Test basic entry retrieval functionality
#[tokio::test]
async fn test_get_entry_basic() {
    let (_instance, tree, key_id) = setup_tree_with_user_key_local().await;

    // Create an entry using helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("test_key", "test_value")]).await;

    // Test get_entry
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    assert_eq!(entry.id(), entry_id);
    assert_eq!(entry.sig.key, SigKey::from_pubkey(&key_id));
    assert!(entry.sig.sig.is_some());
}

/// Test get_entries with multiple entries
#[tokio::test]
async fn test_get_entries_multiple() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key_local().await;

    // Create multiple entries using helper
    let entry_ids = create_linear_chain(&tree, "data", 3).await;

    // Test get_entries
    let entries = tree
        .get_entries(&entry_ids)
        .await
        .expect("Failed to get entries");
    assert_eq!(entries.len(), 3);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.id(), entry_ids[i]);
    }
}

/// Test comprehensive error handling for entry retrieval
#[tokio::test]
async fn test_entry_retrieval_error_handling() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key_local().await;

    // Create one valid entry using helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("key", "value")]).await;

    // Test get_entry with existing entry (should succeed)
    assert!(tree.get_entry(&entry_id).await.is_ok());

    // Test get_entry with non-existent entry (should fail with NotFound)
    let non_existent = ID::from_bytes("non_existent_entry");
    let result = tree.get_entry(non_existent.clone()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test get_entries with mixed valid/invalid entries
    let entry_ids = vec![entry_id.clone(), non_existent.clone()];
    let result = tree.get_entries(entry_ids).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test auth verification with non-existent entry
    let result = tree.verify_entry_signature(non_existent).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

/// Test tree validation - entries from different trees should be rejected
#[tokio::test]
async fn test_tree_validation_rejects_foreign_entries() {
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Doc::new();
    settings1.set("name", "tree1".to_string());
    let tree1 = user
        .create_database(settings1, &key_id)
        .await
        .expect("Failed to create tree1");

    let mut settings2 = Doc::new();
    settings2.set("name", "tree2".to_string());
    let tree2 = user
        .create_database(settings2, &key_id)
        .await
        .expect("Failed to create tree2");

    // Create an entry in tree1
    let txn1 = tree1
        .new_transaction()
        .await
        .expect("Failed to create transaction in tree1");
    let store1 = txn1
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree in tree1");
    store1
        .set("key", "value1")
        .await
        .expect("Failed to set value in tree1");
    let entry1_id = txn1
        .commit()
        .await
        .expect("Failed to commit transaction in tree1");

    // Create an entry in tree2
    let txn2 = tree2
        .new_transaction()
        .await
        .expect("Failed to create transaction in tree2");
    let store2 = txn2
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree in tree2");
    store2
        .set("key", "value2")
        .await
        .expect("Failed to set value in tree2");
    let entry2_id = txn2
        .commit()
        .await
        .expect("Failed to commit transaction in tree2");

    // Verify tree1 can access its own entry
    assert!(tree1.get_entry(&entry1_id).await.is_ok());

    // Verify tree2 can access its own entry
    assert!(tree2.get_entry(&entry2_id).await.is_ok());

    // Verify tree1 cannot access tree2's entry
    let result = tree1.get_entry(&entry2_id).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));

    // Verify tree2 cannot access tree1's entry
    let result = tree2.get_entry(&entry1_id).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));
}

/// Test tree validation with get_entries
#[tokio::test]
async fn test_tree_validation_get_entries() {
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Doc::new();
    settings1.set("name", "tree1".to_string());
    let tree1 = user
        .create_database(settings1, &key_id)
        .await
        .expect("Failed to create tree1");

    let mut settings2 = Doc::new();
    settings2.set("name", "tree2".to_string());
    let tree2 = user
        .create_database(settings2, &key_id)
        .await
        .expect("Failed to create tree2");

    // Create entries in tree1
    let mut tree1_entries = Vec::new();
    for i in 0..2 {
        let txn = tree1
            .new_transaction()
            .await
            .expect("Failed to create transaction in tree1");
        let store = txn
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get subtree in tree1");
        store
            .set("key", format!("value1_{i}"))
            .await
            .expect("Failed to set value in tree1");
        let entry_id = txn
            .commit()
            .await
            .expect("Failed to commit transaction in tree1");
        tree1_entries.push(entry_id);
    }

    // Create an entry in tree2
    let txn2 = tree2
        .new_transaction()
        .await
        .expect("Failed to create transaction in tree2");
    let store2 = txn2
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree in tree2");
    store2
        .set("key", "value2")
        .await
        .expect("Failed to set value in tree2");
    let entry2_id = txn2
        .commit()
        .await
        .expect("Failed to commit transaction in tree2");

    // Verify tree1 can get all its own entries
    let entries = tree1
        .get_entries(&tree1_entries)
        .await
        .expect("Failed to get tree1 entries");
    assert_eq!(entries.len(), 2);

    // Verify get_entries fails when trying to get entries from different trees
    let mixed_entries = vec![tree1_entries[0].clone(), entry2_id];
    let result = tree1.get_entries(&mixed_entries).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));
}

/// Test authentication helpers with signed entries
#[tokio::test]
async fn test_auth_helpers_signed_entries() {
    let (_instance, tree, key_id) = setup_tree_with_user_auth().await;
    let key_id_str = key_id.to_string();

    // Create signed entry using helper
    let entry_id = add_authenticated_data(&tree, "data", &[("key", "value")]).await;

    // Test entry auth access using helper
    assert_entry_authentication(&tree, &entry_id, &key_id_str).await;

    // Test entry belongs to tree
    assert_entry_belongs_to_tree(&tree, &entry_id).await;

    // Test manual auth checks
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    let sig_info = &entry.sig;
    let hint = sig_info.hint();
    let pubkey_str_opt = hint.pubkey.as_ref().map(|k| k.to_string());
    assert!(
        pubkey_str_opt.as_deref() == Some(key_id_str.as_str())
            || hint.name.as_deref() == Some(key_id_str.as_str())
    );
    assert!(
        pubkey_str_opt.as_deref() != Some("OTHER_KEY") && hint.name.as_deref() != Some("OTHER_KEY")
    );
}

/// Test authentication helpers with default authenticated entries
#[tokio::test]
async fn test_auth_helpers_default_authenticated_entries() {
    let (_instance, tree, key_id) = setup_tree_with_user_key_local().await;
    let key_id_str = key_id.to_string();

    // Create entry using default authentication helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("key", "value")]).await;

    // Test entry auth access using helper
    assert_entry_authentication(&tree, &entry_id, &key_id_str).await;

    // Test manual auth checks
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    let sig_info = &entry.sig;
    let hint = sig_info.hint();
    let pubkey_str_opt = hint.pubkey.as_ref().map(|k| k.to_string());
    assert!(
        pubkey_str_opt.as_deref() == Some(key_id_str.as_str())
            || hint.name.as_deref() == Some(key_id_str.as_str())
    );
    assert!(
        pubkey_str_opt.as_deref() != Some("OTHER_KEY") && hint.name.as_deref() != Some("OTHER_KEY")
    );
}

/// Test verify_entry_signature with different authentication scenarios
#[tokio::test]
async fn test_verify_entry_signature_auth_scenarios() {
    let (_instance, tree, key_id) = setup_tree_with_user_auth().await;
    let key_id_str = key_id.to_string();

    // Test 1: Create entry signed with valid key using helper
    let signed_entry_id = add_authenticated_data(&tree, "data", &[("key", "value1")]).await;

    // Should verify successfully using helper
    assert_entry_authentication(&tree, &signed_entry_id, &key_id_str).await;

    // Test 2: Create unsigned entry using helper
    let unsigned_entry_id = add_data_to_subtree(&tree, "data", &[("key", "value2")]).await;

    // Should be valid (backward compatibility for unsigned entries)
    let is_valid_unsigned = tree
        .verify_entry_signature(&unsigned_entry_id)
        .await
        .expect("Failed to verify unsigned entry");
    assert!(is_valid_unsigned);
}

/// Test verify_entry_signature with unauthorized key
#[tokio::test]
async fn test_verify_entry_signature_unauthorized_key() {
    // Create user with first key (will be authorized)
    let (instance, mut user, authorized_key_id) =
        test_local_instance_with_user_and_key("test_user", Some("AUTHORIZED_KEY")).await;

    // Add second key (will NOT be authorized in the database)
    let unauthorized_key_id = user
        .add_private_key(Some("UNAUTHORIZED_KEY"))
        .await
        .expect("Failed to add unauthorized key");

    // Create database with ONLY the authorized key
    let mut settings = Doc::new();
    settings.set("name", "AuthenticatedTree");
    let tree = user
        .create_database(settings, &authorized_key_id)
        .await
        .expect("Failed to create tree");

    // Test with authorized key (should succeed) using helper
    let authorized_entry_id = add_authenticated_data(&tree, "data", &[("key", "value1")]).await;

    assert_entry_authentication(&tree, &authorized_entry_id, &authorized_key_id.to_string()).await;

    // Test with unauthorized key (open succeeds but permission check should fail)
    let unauthorized_signing_key = user
        .get_signing_key(&unauthorized_key_id)
        .expect("Failed to get unauthorized signing key");

    // Database::open succeeds (no key validation at open time)
    // but current_permission should fail because the key is not in the tree's auth settings
    let unauthorized_db = Database::open(&instance, tree.root_id())
        .await
        .expect("open should succeed without key validation")
        .with_key(unauthorized_signing_key);

    let perm_result = unauthorized_db.current_permission().await;
    assert!(perm_result.is_err());
    let error_msg = perm_result.unwrap_err().to_string();
    assert!(
        error_msg.contains("not found") || error_msg.contains("no global permission"),
        "Expected error about key not found, got: {error_msg}"
    );
}

/// Test that verify_entry_signature validates against tree auth configuration
#[tokio::test]
async fn test_verify_entry_signature_validates_tree_auth() {
    let (_instance, tree, key_id) = setup_tree_with_user_auth().await;

    // Create a signed entry using helper
    let entry_id = add_authenticated_data(&tree, "data", &[("key", "value")]).await;

    // Verify the entry using helper - should validate against tree's auth settings
    assert_entry_authentication(&tree, &entry_id, &key_id.to_string()).await;

    // Note: In the future, this test should also verify that:
    // 1. Entries remain valid even if the key is later revoked (historical validation)
    // 2. Entry metadata contains the settings tips that were active when it was created
    // 3. Validation uses those historical settings rather than current settings
}

/// Test tree queries functionality
#[tokio::test]
async fn test_tree_queries() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key_local().await;

    // Get initial entries
    let initial_entries = tree
        .get_all_entries()
        .await
        .expect("Failed to get initial entries");
    let initial_count = initial_entries.len();
    assert!(initial_count >= 1); // At least the root entry

    // Create a few entries
    let mut entry_ids = Vec::new();
    for i in 0..3 {
        let txn = tree
            .new_transaction()
            .await
            .expect("Failed to create transaction");
        let store = txn
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .await
            .expect("Failed to set value");
        let entry_id = txn.commit().await.expect("Failed to commit transaction");
        entry_ids.push(entry_id);
    }

    // Test get_all_entries
    let all_entries = tree
        .get_all_entries()
        .await
        .expect("Failed to get all entries");
    assert_eq!(all_entries.len(), initial_count + 3);

    // Verify all our created entries are in the result
    for entry_id in &entry_ids {
        let found = all_entries.iter().any(|entry| entry.id() == *entry_id);
        assert!(found, "Entry {entry_id} not found in all_entries");
    }
}

/// Test performance: batch get_entries vs individual get_entry calls
#[tokio::test]
async fn test_batch_vs_individual_retrieval() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key_local().await;

    // Create multiple entries
    let mut entry_ids = Vec::new();
    for i in 0..5 {
        let txn = tree
            .new_transaction()
            .await
            .expect("Failed to create transaction");
        let store = txn
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .await
            .expect("Failed to set value");
        let entry_id = txn.commit().await.expect("Failed to commit transaction");
        entry_ids.push(entry_id);
    }

    // Test individual retrieval
    let mut individual_entries = Vec::new();
    for entry_id in &entry_ids {
        let entry = tree.get_entry(entry_id).await.expect("Failed to get entry");
        individual_entries.push(entry);
    }

    // Test batch retrieval
    let batch_entries = tree
        .get_entries(&entry_ids)
        .await
        .expect("Failed to get entries");

    // Results should be identical
    assert_eq!(individual_entries.len(), batch_entries.len());
    for (individual, batch) in individual_entries.iter().zip(batch_entries.iter()) {
        assert_eq!(individual.id(), batch.id());
        assert_eq!(individual.sig, batch.sig);
    }
}

/// `Database::verify()` re-validates `Unverified` entries against the
/// `_settings` they pin (Pieces 1+2), and a plain read opportunistically
/// triggers it when a tip is still `Unverified`.
#[tokio::test]
async fn test_database_verify_promotes_unverified_and_access_hook() {
    let (instance, tree, _key_id) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    // A normally-committed entry is stored Verified by the local validation
    // pass.
    let id = add_data_to_subtree(&tree, "data", &[("k", "v")]).await;
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified
    );

    // Simulate the entry having arrived over sync: force it Unverified.
    backend
        .update_verification_status(&id, VerificationStatus::Unverified)
        .await
        .unwrap();
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Unverified
    );

    // Explicit verify(): reconstructs the pinned `_settings` and promotes
    // the (validly signed, authorised) entry back to Verified.
    let report = tree.verify().await.unwrap();
    assert!(
        report.verified >= 1 && report.failed == 0,
        "expected promotion to Verified, got {report:?}"
    );
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified
    );

    // Access-time hook: force Unverified again, then a plain `get_tips()`
    // must opportunistically verify it.
    backend
        .update_verification_status(&id, VerificationStatus::Unverified)
        .await
        .unwrap();
    let tips = tree.snapshot().await.unwrap().into_tips();
    assert!(!tips.is_empty(), "the verified tip must remain visible");
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified,
        "reading tips should have opportunistically verified the entry"
    );
}

/// The default handle exposes only the Verified frontier: a non-`Verified`
/// interior entry hides its (otherwise `Verified`) descendants, and the read
/// falls back to the nearest `Verified` ancestor. `.allow_unverified()` reads
/// past it. No entry is left `Unverified`, so the access-time verify hook
/// never fires — the cut is purely the frontier logic.
#[tokio::test]
async fn test_verified_frontier_prefix_cut_and_allow_unverified() {
    let (instance, tree, _key_id) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    // Linear main-branch DAG: root -> a -> b -> c, all locally authored and
    // therefore Verified.
    let a = add_data_to_subtree(&tree, "data", &[("step", "a")]).await;
    let b = add_data_to_subtree(&tree, "data", &[("step", "b")]).await;
    let c = add_data_to_subtree(&tree, "data", &[("step", "c")]).await;
    for id in [&a, &b, &c] {
        assert_eq!(
            backend.get_verification_status(id).await.unwrap(),
            VerificationStatus::Verified
        );
    }

    // Knock out the interior entry `b`.
    backend
        .update_verification_status(&b, VerificationStatus::Failed)
        .await
        .unwrap();

    // Default view: `b` is not in the Verified prefix, so neither is its
    // descendant `c` (ancestor-closed). The frontier falls back to `a`.
    let tips = tree.snapshot().await.unwrap().into_tips();
    assert_eq!(tips, vec![a.clone()], "frontier must cut back to `a`");
    assert!(!tips.contains(&b) && !tips.contains(&c));

    // allow_unverified view: raw tip `c` is visible (only `Failed` `b` is
    // dropped from the set; `c` itself is still a Verified tip).
    let loose_tips = tree
        .clone()
        .allow_unverified()
        .snapshot()
        .await
        .unwrap()
        .into_tips();
    assert_eq!(loose_tips, vec![c.clone()], "loose view keeps raw tip `c`");

    // The setting is per-handle and composes with the original handle being
    // unchanged.
    assert_eq!(tree.snapshot().await.unwrap().into_tips(), vec![a]);
}

/// Verification is prefix-closed: it must be impossible for an entry to be
/// `Verified` while an ancestor is not. A `Failed` ancestor taints the whole
/// branch — `verify()` must mark the descendant `Failed`, never promote it,
/// even though the descendant's own signature is perfectly valid.
#[tokio::test]
async fn test_verify_is_prefix_closed() {
    let (instance, tree, _key_id) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    // root -> a -> b, all locally authored ⇒ all Verified.
    let a = add_data_to_subtree(&tree, "data", &[("step", "a")]).await;
    let b = add_data_to_subtree(&tree, "data", &[("step", "b")]).await;

    // Taint the ancestor `a`, then make the descendant `b` look pending (as
    // if it had just arrived over sync). `b`'s own signature is still valid.
    backend
        .update_verification_status(&a, VerificationStatus::Failed)
        .await
        .unwrap();
    backend
        .update_verification_status(&b, VerificationStatus::Unverified)
        .await
        .unwrap();

    let report = tree.verify().await.unwrap();

    // `b` must NOT be promoted — its history is compromised. It is quarantined
    // (Failed) rather than left Unverified, so the taint propagates forward.
    assert_eq!(
        backend.get_verification_status(&b).await.unwrap(),
        VerificationStatus::Failed,
        "a descendant of a Failed ancestor must never be Verified"
    );
    assert!(report.failed >= 1 && report.verified == 0, "{report:?}");

    // Invariant sweep: no Verified entry may have a non-Verified ancestor.
    for entry in tree.get_all_entries().await.unwrap() {
        if backend.get_verification_status(&entry.id()).await.unwrap()
            != VerificationStatus::Verified
        {
            continue;
        }
        for p in entry.parents().unwrap_or_default() {
            assert_eq!(
                backend.get_verification_status(&p).await.unwrap(),
                VerificationStatus::Verified,
                "Verified entry {} has non-Verified parent {p}",
                entry.id()
            );
        }
    }
}

/// Copy every entry of `src` into `dst_backend` via plain `put` — i.e. exactly
/// as they would arrive over sync: stored `Unverified`, no status asserted.
async fn sync_all_unverified(
    src: &Database,
    dst_backend: &std::sync::Arc<dyn eidetica::instance::backend::Backend>,
) -> Vec<ID> {
    let entries = src.get_all_entries().await.unwrap();
    let ids: Vec<ID> = entries.iter().map(|e| e.id()).collect();
    for e in entries {
        dst_backend.put(e).await.unwrap();
    }
    ids
}

/// The motivating scenario: a second instance receives a tree's entries over
/// sync (all `Unverified`), and a single `verify()` must promote the whole
/// history root→…→tip, prefix-closed (a child is only reached after its
/// parent is `Verified`).
#[tokio::test]
async fn test_cross_instance_sync_then_verify_cascade() {
    let (_instance_a, tree_a, _key) = setup_tree_with_user_key_local().await;
    // Linear history root -> a -> b -> c, all Verified on instance A.
    add_data_to_subtree(&tree_a, "data", &[("s", "a")]).await;
    add_data_to_subtree(&tree_a, "data", &[("s", "b")]).await;
    let c = add_data_to_subtree(&tree_a, "data", &[("s", "c")]).await;
    let root = tree_a.root_id().clone();

    // Arrive on instance B as Unverified.
    let instance_b = test_local_instance().await;
    let backend_b = instance_b.backend();
    let ids = sync_all_unverified(&tree_a, backend_b).await;
    assert!(ids.len() >= 4, "expected root+a+b+c, got {}", ids.len());
    for id in &ids {
        assert_eq!(
            backend_b.get_verification_status(id).await.unwrap(),
            VerificationStatus::Unverified
        );
    }

    let db_b = Database::open(&instance_b, &root).await.unwrap();
    let report = db_b.verify().await.unwrap();

    // The entire copied history must verify in one pass; nothing left behind.
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.still_unverified, 0, "{report:?}");
    assert_eq!(report.verified, ids.len(), "whole history must promote");
    for id in &ids {
        assert_eq!(
            backend_b.get_verification_status(id).await.unwrap(),
            VerificationStatus::Verified,
            "{id} should have cascaded to Verified"
        );
    }
    // The tip is now visible in the default (Verified-frontier) view on B.
    assert_eq!(db_b.snapshot().await.unwrap().into_tips(), vec![c]);
}

/// An entry whose pinned `_settings` ancestor set is not fully held locally
/// must stay `Unverified` (counted in `still_unverified`) — *not* `Failed` —
/// and a later pass must promote it once the missing settings entry arrives.
/// This is the entire reason `Unverified` and `Failed` are distinct states.
#[tokio::test]
async fn test_incomplete_pinned_settings_stays_unverified_then_recovers() {
    let (_instance_a, tree_a, _key) = setup_tree_with_user_key_local().await;
    let root = tree_a.root_id().clone();

    // A settings change `s` (a distinct `_settings`-bearing entry), then a
    // data entry `d` that pins `s`.
    let (_sk2, pub2) = generate_keypair();
    add_auth_key(
        &tree_a,
        &pub2,
        AuthKey::active(Some("k2"), Permission::Write(10)),
    )
    .await;
    let s = tree_a.snapshot().await.unwrap().into_tips()[0].clone();
    let d = add_data_to_subtree(&tree_a, "data", &[("k", "v")]).await;

    // Sync everything to B *except* the settings entry `s`. `get_tree` is a
    // flat in-tree filter, so `d` is still visible to verify() — its pinned
    // `_settings` closure is just incomplete.
    let instance_b = test_local_instance().await;
    let backend_b = instance_b.backend();
    for e in tree_a.get_all_entries().await.unwrap() {
        if e.id() == s {
            continue;
        }
        backend_b.put(e).await.unwrap();
    }
    let db_b = Database::open(&instance_b, &root).await.unwrap();

    let report = db_b.verify().await.unwrap();
    assert!(
        report.still_unverified >= 1,
        "d's pinned _settings is incomplete → still_unverified, got {report:?}"
    );
    assert_eq!(
        backend_b.get_verification_status(&d).await.unwrap(),
        VerificationStatus::Unverified,
        "incomplete pin must NOT be quarantined as Failed"
    );

    // The missing settings entry arrives; a re-verify now completes the pin
    // and the entry (and `s`) cascade to Verified.
    let s_entry = tree_a.get_entry(&s).await.unwrap();
    backend_b.put(s_entry).await.unwrap();
    let report2 = db_b.verify().await.unwrap();
    assert!(report2.verified >= 1 && report2.failed == 0, "{report2:?}");
    assert_eq!(
        backend_b.get_verification_status(&d).await.unwrap(),
        VerificationStatus::Verified,
        "d must verify once its pinned _settings is held"
    );
}

/// Prefix-closure across a *merge*: `root → (a, b) → c`. A `Failed` parent
/// taints the merge child even though the other parent verifies fine, and the
/// Verified frontier cuts at the surviving verified branch.
#[tokio::test]
async fn test_diamond_dag_taint_propagation() {
    let (instance, tree, _key) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();
    let root = tree.root_id().clone();

    let a = add_data_to_subtree(&tree, "data", &[("p", "a")]).await;
    let b = create_branch_from_entry(&tree, &root, "data", &[("p", "b")]).await;
    // Merge a and b into c.
    let c = {
        let txn = tree
            .new_transaction_at(&Snapshot::from(&[a.clone(), b.clone()]))
            .await
            .unwrap();
        txn.get_store::<DocStore>("data")
            .await
            .unwrap()
            .set("p", "c")
            .await
            .unwrap();
        txn.commit().await.unwrap()
    };
    assert_eq!(
        tree.get_entry(&c).await.unwrap().parents().unwrap().len(),
        2,
        "c must be a real merge of a and b"
    );

    // Taint one parent; make the merge child look freshly synced.
    backend
        .update_verification_status(&a, VerificationStatus::Failed)
        .await
        .unwrap();
    backend
        .update_verification_status(&c, VerificationStatus::Unverified)
        .await
        .unwrap();

    let report = tree.verify().await.unwrap();
    assert_eq!(
        backend.get_verification_status(&c).await.unwrap(),
        VerificationStatus::Failed,
        "merge child of a Failed parent must be Failed, not promoted"
    );
    assert_eq!(
        backend.get_verification_status(&b).await.unwrap(),
        VerificationStatus::Verified,
        "the untainted parent is unaffected"
    );
    assert!(report.failed >= 1, "{report:?}");

    // Frontier cuts to the surviving verified branch `b`; the loose view
    // still drops Failed `c`, leaving nothing (c is the only raw tip).
    assert_eq!(tree.snapshot().await.unwrap().into_tips(), vec![b]);
    assert!(
        tree.clone()
            .allow_unverified()
            .snapshot()
            .await
            .unwrap()
            .is_empty(),
        "Failed is dropped even in the allow_unverified view"
    );
}

/// `allow_unverified()` composes with the handle's configured signing key:
/// it can read *and write* past an unverifiable region, while the default
/// handle's Verified-frontier view stays unaffected by that write.
#[tokio::test]
async fn test_allow_unverified_composes_with_key_for_writes() {
    let (instance, tree, _key) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    let a = add_data_to_subtree(&tree, "data", &[("s", "a")]).await;
    let b = add_data_to_subtree(&tree, "data", &[("s", "b")]).await;

    // Taint the interior entry `a`. Default frontier now cuts back before
    // `a`; `b` (still Verified, but reachable only through Failed `a`) is
    // hidden from the default view.
    backend
        .update_verification_status(&a, VerificationStatus::Failed)
        .await
        .unwrap();
    let default_tips = tree.snapshot().await.unwrap().into_tips();
    assert!(
        !default_tips.contains(&b),
        "default view must not expose state behind a Failed ancestor"
    );

    // The loose handle keeps the signing key (set via `..self`) and sees the
    // raw tip `b`, so it can commit on top of it.
    let loose = tree.clone().allow_unverified();
    assert_eq!(loose.snapshot().await.unwrap().into_tips(), vec![b.clone()]);
    let e = add_data_to_subtree(&loose, "data", &[("s", "e")]).await;

    // The new entry was locally validated → Verified, and built on `b`.
    assert_eq!(
        backend.get_verification_status(&e).await.unwrap(),
        VerificationStatus::Verified
    );
    assert_eq!(
        tree.get_entry(&e).await.unwrap().parents().unwrap(),
        vec![b]
    );
    // Loose view advances to `e`; the default frontier is still cut (the new
    // entry sits behind the Failed `a`, so it remains hidden by default).
    assert_eq!(loose.snapshot().await.unwrap().into_tips(), vec![e.clone()]);
    assert!(!tree.snapshot().await.unwrap().into_tips().contains(&e));
}

/// The access-time auto-verify hook in `get_tips` must handle *multiple*
/// diverged `Unverified` tips, not just a single one, promoting all of them
/// in the opportunistic pass.
#[tokio::test]
async fn test_access_hook_promotes_multiple_unverified_tips() {
    let (instance, tree, _key) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();
    let root = tree.root_id().clone();

    // Two diverged tips off the root.
    let a = add_data_to_subtree(&tree, "data", &[("b", "a")]).await;
    let b = create_branch_from_entry(&tree, &root, "data", &[("b", "b")]).await;

    backend
        .update_verification_status(&a, VerificationStatus::Unverified)
        .await
        .unwrap();
    backend
        .update_verification_status(&b, VerificationStatus::Unverified)
        .await
        .unwrap();

    // A plain default read: the hook fires because tips are Unverified and
    // must promote *both* diverged tips.
    let tips = tree.snapshot().await.unwrap().into_tips();
    assert_eq!(
        tips.len(),
        2,
        "both diverged tips should be visible: {tips:?}"
    );
    assert!(tips.contains(&a) && tips.contains(&b));
    for id in [&a, &b] {
        assert_eq!(
            backend.get_verification_status(id).await.unwrap(),
            VerificationStatus::Verified,
            "the access hook must verify every Unverified tip, not just one"
        );
    }
}

/// `VerifyReport` counts must be exact (no double-counting) and `verify()`
/// must be idempotent: a pass over an already-fully-`Verified` database
/// reports all-zero and changes nothing.
#[tokio::test]
async fn test_verify_report_counts_and_idempotent() {
    let (instance, tree, _key) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    // root -> a -> b -> c -> d, all Verified by local commit.
    let a = add_data_to_subtree(&tree, "data", &[("s", "a")]).await;
    let b = add_data_to_subtree(&tree, "data", &[("s", "b")]).await;
    let c = add_data_to_subtree(&tree, "data", &[("s", "c")]).await;
    let d = add_data_to_subtree(&tree, "data", &[("s", "d")]).await;

    // Idempotent: nothing is Unverified, so a pass is a no-op.
    assert_eq!(
        tree.verify().await.unwrap(),
        eidetica::database::VerifyReport::default(),
        "verify() over an all-Verified DB must report all-zero"
    );

    // Force the whole tail Unverified; one pass must cascade-promote exactly
    // those four (root was already Verified, not re-counted).
    for id in [&a, &b, &c, &d] {
        backend
            .update_verification_status(id, VerificationStatus::Unverified)
            .await
            .unwrap();
    }
    let report = tree.verify().await.unwrap();
    assert_eq!(report.verified, 4, "exactly a,b,c,d promoted: {report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.still_unverified, 0, "{report:?}");
    // Immediately idempotent again.
    assert_eq!(
        tree.verify().await.unwrap(),
        eidetica::database::VerifyReport::default()
    );

    // Failed-taint counting: a Failed interior entry taints its descendants,
    // each counted once under `failed`, none double-counted as verified.
    backend
        .update_verification_status(&b, VerificationStatus::Failed)
        .await
        .unwrap();
    for id in [&c, &d] {
        backend
            .update_verification_status(id, VerificationStatus::Unverified)
            .await
            .unwrap();
    }
    let report = tree.verify().await.unwrap();
    assert_eq!(
        report,
        eidetica::database::VerifyReport {
            verified: 0,
            failed: 2,
            still_unverified: 0
        },
        "c and d each Failed exactly once via taint: {report:?}"
    );
}

/// `verify()` must validate an entry against the `_settings` it *pinned*, not
/// the current settings. An entry signed by a key that is later revoked must
/// still verify, because at the pinned settings that key was authorized.
#[tokio::test]
async fn test_verify_uses_pinned_not_current_settings() {
    let (instance, tree, key_id) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    // `d` is signed by the bootstrap admin key and pins the genesis
    // `_settings` (where that key is Active Admin).
    let d = add_data_to_subtree(&tree, "data", &[("k", "v")]).await;

    // Now revoke that very key in a later settings transaction. (The
    // revocation entry is itself signed while the key is still active, so it
    // commits; from here on the *current* settings no longer authorize it.)
    add_auth_key(
        &tree,
        &key_id,
        AuthKey::new(None, Permission::Admin(0), KeyStatus::Revoked),
    )
    .await;

    // Sanity: current settings really did revoke the key.
    let current = tree
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();
    assert_eq!(
        current.get_key_by_pubkey(&key_id).unwrap().status(),
        &KeyStatus::Revoked,
        "precondition: key must be revoked in current settings"
    );

    // Force `d` Unverified and re-verify. Validation must run against the
    // settings `d` pinned (key Active) — so it verifies, NOT Failed.
    backend
        .update_verification_status(&d, VerificationStatus::Unverified)
        .await
        .unwrap();
    let report = tree.verify().await.unwrap();
    assert!(
        report.failed == 0,
        "must not fail on pinned-valid entry: {report:?}"
    );
    assert_eq!(
        backend.get_verification_status(&d).await.unwrap(),
        VerificationStatus::Verified,
        "entry must verify against pinned (historical) settings, not current"
    );
}

/// A genesis (TOFU) root must still verify after the backend is persisted and
/// reloaded into a fresh `Instance` — the prefix-closed rule bottoms out at a
/// self-authorising root.
#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_genesis_verifies_after_persist_reload() {
    // Bootstrap "u" as the initial user (also Admin), avoiding the
    // service-mode admin login dance — this is a local persist/reload test.
    let (instance, mut user) = Instance::create_backend(
        Box::new(InMemory::new()),
        eidetica::NewUser::passwordless("u"),
    )
    .await
    .unwrap();
    let key_id = user.add_private_key(Some("k")).await.unwrap();
    let mut settings = Doc::new();
    settings.set("name", "persisted");
    let tree = user.create_database(settings, &key_id).await.unwrap();
    let root = tree.root_id().clone();
    let d = add_data_to_subtree(&tree, "data", &[("k", "v")]).await;

    // Persist and reload into a brand-new Instance.
    let path = std::env::temp_dir().join(format!(
        "eidetica_verify_reload_{}_{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let engine1 = instance.backend().engine();
    engine1
        .as_any()
        .downcast_ref::<InMemory>()
        .expect("test backend is InMemory")
        .save_to_file(&path)
        .unwrap();
    let reloaded = InMemory::load_from_file(&path).await.unwrap();
    std::fs::remove_file(&path).ok();
    // Reload an already-initialised backend — Instance::open_backend is load-only.
    let instance2 = Instance::open_backend(Box::new(reloaded)).await.unwrap();
    let backend2 = instance2.backend();

    // Force the whole history Unverified on the reloaded backend.
    for id in [&root, &d] {
        backend2
            .update_verification_status(id, VerificationStatus::Unverified)
            .await
            .unwrap();
    }
    let db2 = Database::open(&instance2, &root).await.unwrap();
    let report = db2.verify().await.unwrap();
    assert!(report.failed == 0, "{report:?}");
    assert_eq!(
        backend2.get_verification_status(&root).await.unwrap(),
        VerificationStatus::Verified,
        "genesis root must self-verify (TOFU) after reload"
    );
    assert_eq!(
        backend2.get_verification_status(&d).await.unwrap(),
        VerificationStatus::Verified,
        "descendant must cascade once root is verified"
    );
}

/// The Verified-frontier cut is correct on a deep linear chain (the algorithm
/// is a single height-ordered pass — O(V+E), not O(depth^2)). An interior
/// `Failed` entry hides the whole tail; the frontier is the entry just before
/// it.
#[tokio::test]
async fn test_deep_chain_frontier_cut() {
    let (instance, tree, _key) = setup_tree_with_user_key_local().await;
    let backend = instance.backend();

    let mut chain = Vec::new();
    for i in 0..30 {
        chain.push(add_data_to_subtree(&tree, "data", &[("i", &i.to_string())]).await);
    }

    // Knock out entry #15; everything from #15 onward is reachable only
    // through it and must drop out of the default view.
    let failed_idx = 15;
    backend
        .update_verification_status(&chain[failed_idx], VerificationStatus::Failed)
        .await
        .unwrap();

    let tips = tree.snapshot().await.unwrap().into_tips();
    assert_eq!(
        tips,
        vec![chain[failed_idx - 1].clone()],
        "frontier must cut to the last all-Verified prefix entry"
    );
    // The loose view keeps the raw tip (#29) — only Failed is dropped, and
    // #29 itself is Verified.
    assert_eq!(
        tree.clone()
            .allow_unverified()
            .snapshot()
            .await
            .unwrap()
            .into_tips(),
        vec![chain[29].clone()]
    );
}
