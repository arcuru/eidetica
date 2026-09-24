//! Settings metadata tests
//!
//! This module contains tests for settings metadata management including
//! settings tips tracking, metadata propagation, and historical validation.

use eidetica::{
    Entry, Snapshot,
    crdt::{Doc, doc::Value},
    store::DocStore,
};

use crate::helpers::test_local_instance_with_user_and_key;

#[tokio::test]
async fn test_settings_tips_in_metadata() {
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create initial settings
    let mut settings = Doc::new();
    settings.set("name", "test_tree".to_string());

    // Create a tree with authentication
    let tree = user.create_database(settings, &key_id).await.unwrap();

    // Create an operation to add some data
    let txn1 = tree.new_transaction().await.unwrap();
    let kv = txn1.get_store::<DocStore>("data").await.unwrap();
    kv.set("key1", "value1").await.unwrap();
    let entry1_id = txn1.commit().await.unwrap();

    // Get the entry and check metadata
    let entry1 = tree.get_entry(&entry1_id).await.unwrap();
    let metadata = entry1.metadata().expect("Entry should have metadata");

    // Parse metadata and verify settings_tips field exists
    let metadata_obj: serde_json::Value = serde_json::from_slice(metadata).unwrap();
    let settings_tips_array = metadata_obj
        .get("settings_tips")
        .expect("Should have settings_tips");
    assert!(
        !settings_tips_array.as_array().unwrap().is_empty(),
        "Settings tips should not be empty"
    );

    // Create another operation to modify settings
    let txn2 = tree.new_transaction().await.unwrap();
    let settings_store = txn2.get_store::<DocStore>("_settings").await.unwrap();
    settings_store
        .set("description", "A test tree")
        .await
        .unwrap();
    let entry2_id = txn2.commit().await.unwrap();

    // Create a third operation that doesn't modify settings
    let txn3 = tree.new_transaction().await.unwrap();
    let kv3 = txn3.get_store::<DocStore>("data").await.unwrap();
    kv3.set("key2", "value2").await.unwrap();
    let entry3_id = txn3.commit().await.unwrap();

    // Get the entries and verify settings tips
    let entry2 = tree.get_entry(&entry2_id).await.unwrap();
    let entry3 = tree.get_entry(&entry3_id).await.unwrap();

    // Parse metadata from entries
    let metadata2 = entry2.metadata().expect("Entry2 should have metadata");
    let metadata3 = entry3.metadata().expect("Entry3 should have metadata");

    let metadata2_obj: serde_json::Value = serde_json::from_slice(metadata2).unwrap();
    let metadata3_obj: serde_json::Value = serde_json::from_slice(metadata3).unwrap();

    let settings_tips2 = metadata2_obj
        .get("settings_tips")
        .expect("Should have settings_tips");
    let settings_tips3 = metadata3_obj
        .get("settings_tips")
        .expect("Should have settings_tips");

    assert!(
        !settings_tips2.as_array().unwrap().is_empty(),
        "Settings tips should not be empty after settings update"
    );
    assert!(
        !settings_tips3.as_array().unwrap().is_empty(),
        "Settings tips should not be empty"
    );

    // Entry 3 should have different settings tips (should include entry2)
    let tips3_array = settings_tips3.as_array().unwrap();
    assert!(
        tips3_array.contains(&serde_json::Value::String(entry2_id.to_string())),
        "Entry 3 should have entry 2 in its settings tips"
    );
}

#[tokio::test]
async fn test_entry_get_settings_from_subtree() {
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create initial settings with some data
    let mut settings = Doc::new();
    settings.set("name", "test_tree".to_string());
    settings.set("version", "1.0".to_string());

    // Create a tree
    let tree = user
        .create_database(settings.clone(), &key_id)
        .await
        .unwrap();

    // Get the root entry and verify it has _settings subtree
    let root_entry = tree.get_root().await.unwrap();

    // Entry shouldn't know about settings - that's Transaction's job
    // But we can verify the entry has the _settings subtree data
    let settings_data = root_entry.data("_settings").unwrap();
    let parsed_settings: Doc = serde_json::from_slice(settings_data).unwrap();

    // Verify the settings contain what we expect
    match parsed_settings.get("name").unwrap() {
        Value::Text(s) => assert_eq!(s, "test_tree"),
        _ => panic!("Expected string value for name"),
    }
    match parsed_settings.get("version").unwrap() {
        Value::Text(s) => assert_eq!(s, "1.0"),
        _ => panic!("Expected string value for version"),
    }

    // Transaction should be able to get settings properly
    let txn = tree.new_transaction().await.unwrap();
    let txn_settings = txn.get_settings().unwrap();
    let name = txn_settings.get_name().await.unwrap();
    assert_eq!(name, "test_tree");
}

#[tokio::test]
async fn test_settings_tips_propagation() {
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create a tree
    let settings = Doc::new();
    let tree = user.create_database(settings, &key_id).await.unwrap();

    // Create a chain of entries
    let txn1 = tree.new_transaction().await.unwrap();
    let kv = txn1.get_store::<DocStore>("data").await.unwrap();
    kv.set("entry", "1").await.unwrap();
    let entry1_id = txn1.commit().await.unwrap();

    // Modify settings
    let txn2 = tree.new_transaction().await.unwrap();
    let settings_store = txn2.get_store::<DocStore>("_settings").await.unwrap();
    settings_store.set("updated", "true").await.unwrap();
    let entry2_id = txn2.commit().await.unwrap();

    // Create another entry after settings change
    let txn3 = tree.new_transaction().await.unwrap();
    let kv = txn3.get_store::<DocStore>("data").await.unwrap();
    kv.set("entry", "3").await.unwrap();
    let entry3_id = txn3.commit().await.unwrap();

    // Get all entries
    let entry1 = tree.get_entry(&entry1_id).await.unwrap();
    let entry2 = tree.get_entry(&entry2_id).await.unwrap();
    let entry3 = tree.get_entry(&entry3_id).await.unwrap();

    // Parse settings tips from metadata
    let parse_tips = |entry: &Entry| -> Vec<String> {
        if let Some(metadata_str) = entry.metadata()
            && let Ok(metadata_obj) = serde_json::from_slice::<serde_json::Value>(metadata_str)
            && let Some(tips_array) = metadata_obj.get("settings_tips")
        {
            return tips_array
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
        }
        Vec::new()
    };

    let tips1 = parse_tips(&entry1);
    let tips2 = parse_tips(&entry2);
    let tips3 = parse_tips(&entry3);

    // Entry 1 and 2 should have the same initial settings tips
    assert_eq!(
        tips1, tips2,
        "First two entries should have same settings tips"
    );

    // Entry 3 should have different settings tips (after settings update)
    assert_ne!(
        tips2, tips3,
        "Entry after settings update should have different tips"
    );

    // Entry 3's tips should include entry 2 (the settings update)
    assert!(
        tips3.contains(&entry2_id.to_string()),
        "New settings tips should include the settings update entry"
    );
}

#[tokio::test]
async fn test_settings_metadata_with_complex_operations() {
    // Test settings metadata handling with complex operations
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("complex_key")).await;

    // Create tree with initial settings
    let mut initial_settings = Doc::new();
    initial_settings.set("name", "ComplexTree".to_string());
    initial_settings.set("version", "1.0".to_string());
    let tree = user
        .create_database(initial_settings, &key_id)
        .await
        .unwrap();

    // Create several data operations
    let mut data_entry_ids = Vec::new();
    for i in 0..3 {
        let txn = tree.new_transaction().await.unwrap();
        let data_store = txn.get_store::<DocStore>("data").await.unwrap();
        data_store.set("counter", i.to_string()).await.unwrap();
        data_store
            .set(format!("data_{i}"), format!("value_{i}"))
            .await
            .unwrap();
        let entry_id = txn.commit().await.unwrap();
        data_entry_ids.push(entry_id);
    }

    // Update settings
    let settings_op = tree.new_transaction().await.unwrap();
    let settings_store = settings_op
        .get_store::<DocStore>("_settings")
        .await
        .unwrap();
    settings_store
        .set("description", "Updated with metadata")
        .await
        .unwrap();
    settings_store.set("version", "2.0").await.unwrap();
    let settings_entry_id = settings_op.commit().await.unwrap();

    // Create more data operations after settings update
    let mut post_settings_entry_ids = Vec::new();
    for i in 3..6 {
        let txn = tree.new_transaction().await.unwrap();
        let data_store = txn.get_store::<DocStore>("data").await.unwrap();
        data_store.set("counter", i.to_string()).await.unwrap();
        data_store
            .set(format!("data_{i}"), format!("value_{i}"))
            .await
            .unwrap();
        let entry_id = txn.commit().await.unwrap();
        post_settings_entry_ids.push(entry_id);
    }

    // Helper function to parse settings tips from entry
    let parse_settings_tips = |entry: &Entry| -> Vec<String> {
        if let Some(metadata_str) = entry.metadata() {
            let metadata_obj: serde_json::Value = serde_json::from_slice(metadata_str).unwrap();
            if let Some(tips_array) = metadata_obj.get("settings_tips") {
                return tips_array
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
            }
        }
        Vec::new()
    };

    // Pre-settings entries should have same settings tips
    let entry0 = tree.get_entry(&data_entry_ids[0]).await.unwrap();
    let pre_tips = parse_settings_tips(&entry0);
    for entry_id in &data_entry_ids[1..] {
        let entry = tree.get_entry(entry_id).await.unwrap();
        let tips = parse_settings_tips(&entry);
        assert_eq!(pre_tips, tips, "Pre-settings entries should have same tips");
    }

    // Post-settings entries should have different tips (including settings update)
    let entry_post0 = tree.get_entry(&post_settings_entry_ids[0]).await.unwrap();
    let post_tips = parse_settings_tips(&entry_post0);
    assert_ne!(
        pre_tips, post_tips,
        "Post-settings entries should have different tips"
    );
    assert!(
        post_tips.contains(&settings_entry_id.to_string()),
        "Post-settings entries should include settings update"
    );

    // All post-settings entries should have same tips
    for entry_id in &post_settings_entry_ids[1..] {
        let entry = tree.get_entry(entry_id).await.unwrap();
        let tips = parse_settings_tips(&entry);
        assert_eq!(
            post_tips, tips,
            "All post-settings entries should have same tips"
        );
    }
}

#[tokio::test]
async fn test_settings_metadata_with_branching() {
    // Test settings metadata with branching scenarios
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("branch_key")).await;

    let tree = user.create_database(Doc::new(), &key_id).await.unwrap();

    // Create base entry
    let base_op = tree.new_transaction().await.unwrap();
    let base_store = base_op.get_store::<DocStore>("data").await.unwrap();
    base_store.set("base", "true").await.unwrap();
    let base_id = base_op.commit().await.unwrap();

    // Create two branches from base
    let branch1_op = tree
        .new_transaction_at(&Snapshot::from(std::slice::from_ref(&base_id)))
        .await
        .unwrap();
    let branch1_store = branch1_op.get_store::<DocStore>("data").await.unwrap();
    branch1_store.set("branch", "1").await.unwrap();
    let branch1_id = branch1_op.commit().await.unwrap();

    let branch2_op = tree
        .new_transaction_at(&Snapshot::from(std::slice::from_ref(&base_id)))
        .await
        .unwrap();
    let branch2_store = branch2_op.get_store::<DocStore>("data").await.unwrap();
    branch2_store.set("branch", "2").await.unwrap();
    let branch2_id = branch2_op.commit().await.unwrap();

    // Update settings on one branch
    let settings_op = tree
        .new_transaction_at(&Snapshot::from(std::slice::from_ref(&branch1_id)))
        .await
        .unwrap();
    let settings_store = settings_op
        .get_store::<DocStore>("_settings")
        .await
        .unwrap();
    settings_store
        .set("branch_settings", "updated")
        .await
        .unwrap();
    let settings_id = settings_op.commit().await.unwrap();

    // Create merge operation
    let merge_tips = vec![settings_id.clone(), branch2_id.clone()];
    let merge_op = tree
        .new_transaction_at(&Snapshot::from(&merge_tips))
        .await
        .unwrap();
    let merge_store = merge_op.get_store::<DocStore>("data").await.unwrap();
    merge_store.set("merged", "true").await.unwrap();
    let merge_id = merge_op.commit().await.unwrap();

    // Verify settings tips in merge operation
    let merge_entry = tree.get_entry(&merge_id).await.unwrap();
    let metadata_str = merge_entry
        .metadata()
        .expect("Merge entry should have metadata");
    let metadata_obj: serde_json::Value = serde_json::from_slice(metadata_str).unwrap();
    let settings_tips = metadata_obj
        .get("settings_tips")
        .expect("Should have settings_tips")
        .as_array()
        .unwrap();

    // Merge should have settings tips that include the settings update
    let settings_tips_strings: Vec<String> = settings_tips
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        settings_tips_strings.contains(&settings_id.to_string()),
        "Merge should include settings update in tips"
    );
}

#[tokio::test]
async fn test_metadata_consistency_across_operations() {
    // Test that metadata is consistently tracked across different operation types
    let (_instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("consistency_key")).await;

    let mut settings = Doc::new();
    settings.set("initial", "true".to_string());
    let tree = user.create_database(settings, &key_id).await.unwrap();

    // Create authenticated operation (tree already configured with key_id)
    let auth_op = tree.new_transaction().await.unwrap();
    let auth_store = auth_op.get_store::<DocStore>("auth_data").await.unwrap();
    auth_store.set("authenticated", "true").await.unwrap();
    let auth_id = auth_op.commit().await.unwrap();

    // Create regular operation
    let regular_op = tree.new_transaction().await.unwrap();
    let regular_store = regular_op
        .get_store::<DocStore>("regular_data")
        .await
        .unwrap();
    regular_store.set("regular", "true").await.unwrap();
    let regular_id = regular_op.commit().await.unwrap();

    // Both should have consistent metadata
    let auth_entry = tree.get_entry(&auth_id).await.unwrap();
    let regular_entry = tree.get_entry(&regular_id).await.unwrap();

    assert!(
        auth_entry.metadata().is_some(),
        "Auth entry should have metadata"
    );
    assert!(
        regular_entry.metadata().is_some(),
        "Regular entry should have metadata"
    );

    // Parse and compare settings tips
    let get_settings_tips = |entry: &Entry| -> Vec<String> {
        let metadata_str = entry.metadata().unwrap();
        let metadata_obj: serde_json::Value = serde_json::from_slice(metadata_str).unwrap();
        metadata_obj
            .get("settings_tips")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };

    let auth_tips = get_settings_tips(&auth_entry);
    let regular_tips = get_settings_tips(&regular_entry);

    // Since no settings were changed between operations, tips should be same
    assert_eq!(
        auth_tips, regular_tips,
        "Operations without settings changes should have same tips"
    );
}

/// A raw backend matrix check: signed metadata is not a license to replace
/// the _settings frontier of the entry's main parents.
#[tokio::test]
async fn test_remote_historical_pin_matches_main_parent_frontier() {
    use crate::helpers::test_backend;
    use eidetica::auth::{
        crypto::{PrivateKey, sign_entry},
        types::{AuthKey, Permission, SigKey},
    };
    use eidetica::backend::VerificationStatus;
    use eidetica::{Database, Instance, NewUser};

    let (instance, _) =
        Instance::create_backend(test_backend().await, NewUser::passwordless("admin"))
            .await
            .unwrap();
    let admin = PrivateKey::generate();
    let db = Database::create(&instance, admin, Doc::new())
        .await
        .unwrap();
    let engine = instance.backend().local_engine().unwrap();
    let root = db.root_id().clone();
    let txn = db.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("k", "v")
        .await
        .unwrap();
    let data_id = txn.commit().await.unwrap();
    let metadata = |tips: &[eidetica::entry::ID]| {
        serde_json::to_vec(&serde_json::json!({
            "settings_tips": tips, "entropy": null
        }))
        .unwrap()
    };
    let forged = Entry::builder(root.clone())
        .add_parent(data_id.clone())
        .set_height(2)
        .set_metadata(metadata(&[data_id]))
        .set_subtree_data("data", b"forged")
        .build()
        .unwrap();
    let forged_id = forged.id();
    engine.put(forged).await.unwrap();
    db.verify().await.unwrap();
    assert_eq!(
        engine.get_verification_status(&forged_id).await.unwrap(),
        VerificationStatus::Failed
    );

    let signer = PrivateKey::generate();
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(
            &signer.public_key(),
            AuthKey::active(Some("temporary"), Permission::Write(1)),
        )
        .await
        .unwrap();
    let grant = txn.commit().await.unwrap();
    let key = SigKey::from_pubkey(&signer.public_key());
    let signed = |parent: eidetica::entry::ID| {
        let entry = Entry::builder(root.clone())
            .add_parent(parent)
            .set_height(3)
            .set_metadata(metadata(std::slice::from_ref(&grant)))
            .set_subtree_data("data", b"stale-key")
            .build()
            .unwrap()
            .with_auth(|auth| auth.key = key.clone());
        let signature = sign_entry(&entry, &signer).unwrap();
        entry.with_auth(|auth| auth.signature = Some(signature))
    };
    let sibling = signed(grant.clone());
    let txn = db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&signer.public_key())
        .await
        .unwrap();
    let revocation = txn.commit().await.unwrap();
    let sibling_id = sibling.id();
    engine.put(sibling).await.unwrap();
    db.verify().await.unwrap();
    assert_eq!(
        engine.get_verification_status(&sibling_id).await.unwrap(),
        VerificationStatus::Verified
    );
    let child = signed(revocation);
    let child_id = child.id();
    engine.put(child).await.unwrap();
    db.verify().await.unwrap();
    assert_eq!(
        engine.get_verification_status(&child_id).await.unwrap(),
        VerificationStatus::Failed
    );
}

/// Old-branch commits pin their main parents' pre-write settings, not the live head.
#[tokio::test]
async fn test_historical_transaction_pins_main_parent_settings() {
    use eidetica::backend::VerificationStatus;
    use eidetica::constants::SETTINGS;

    let (instance, mut user, key_id) =
        test_local_instance_with_user_and_key("historical_user", Some("historical_key")).await;
    let db = user.create_database(Doc::new(), &key_id).await.unwrap();
    let root = db.root_id().clone();
    let engine = instance.backend().local_engine().unwrap();

    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>(SETTINGS)
        .await
        .unwrap()
        .set("version", "new")
        .await
        .unwrap();
    let newer = tx.commit().await.unwrap();

    let old_parents = Snapshot::from([root.clone()]);
    let tx = db.new_transaction_at(&old_parents).await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("old", "branch")
        .await
        .unwrap();
    let historical = tx.commit().await.unwrap();

    let pin = |entry: &Entry| -> Snapshot {
        let value: serde_json::Value = serde_json::from_slice(entry.metadata().unwrap()).unwrap();
        serde_json::from_value(value["settings_tips"].clone()).unwrap()
    };
    let expected_old = engine
        .store_snapshot_at(db.root_id(), SETTINGS, &old_parents)
        .await
        .unwrap();
    let expected_new = engine
        .store_snapshot_at(db.root_id(), SETTINGS, &Snapshot::from([newer.clone()]))
        .await
        .unwrap();
    assert_eq!(expected_old, Snapshot::from([root]));
    assert_eq!(expected_new, Snapshot::from([newer.clone()]));
    assert_eq!(pin(&db.get_entry(&historical).await.unwrap()), expected_old);

    let tx = db
        .new_transaction_at(&Snapshot::from([newer]))
        .await
        .unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("new", "branch")
        .await
        .unwrap();
    let sibling = tx.commit().await.unwrap();
    assert_eq!(pin(&db.get_entry(&sibling).await.unwrap()), expected_new);

    let merge_parents = Snapshot::from([historical.clone(), sibling]);
    let tx = db.new_transaction_at(&merge_parents).await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("merged", "yes")
        .await
        .unwrap();
    let merged = tx.commit().await.unwrap();
    let expected_merge = engine
        .store_snapshot_at(db.root_id(), SETTINGS, &merge_parents)
        .await
        .unwrap();
    assert_eq!(pin(&db.get_entry(&merged).await.unwrap()), expected_merge);

    assert_eq!(
        engine.get_verification_status(&historical).await.unwrap(),
        VerificationStatus::Verified
    );
    instance
        .demote_to_unverified(db.root_id(), &historical)
        .await
        .unwrap();
    db.verify().await.unwrap();
    assert_eq!(
        engine.get_verification_status(&historical).await.unwrap(),
        VerificationStatus::Verified
    );
}
