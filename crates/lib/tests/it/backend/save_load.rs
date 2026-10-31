use std::{fs, io::Write, path::Path, sync::Arc};

use tempfile::TempDir;

use eidetica::{
    Entry, Result,
    backend::{BackendImpl, database::InMemory},
};

fn save_backend(backend: &InMemory, path: &Path) -> Result<()> {
    backend.save_to_file(path)
}

async fn load_backend(path: &Path) -> Result<InMemory> {
    InMemory::load_from_file(path).await
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_in_memory_backend_save_and_load() {
    // Create a temporary file path
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test_backend_save.json");

    // Setup: Create a backend with some data
    {
        let backend = Arc::new(InMemory::new());
        let entry = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully");
        backend.put_verified(entry).await.unwrap();

        // Save to file
        let save_result = save_backend(&backend, &file_path);
        assert!(save_result.is_ok());
    }

    // Verify file exists
    assert!(file_path.exists());

    // Load from file
    let load_result = load_backend(&file_path).await;
    assert!(load_result.is_ok());
    let loaded_backend = load_result.unwrap();

    // Verify data was loaded correctly
    let roots = loaded_backend.all_roots().await.unwrap();
    assert_eq!(roots.len(), 1);
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_load_non_existent_file() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("non_existent_file.json");
    // File doesn't exist by default in a new temp dir

    // Load
    let backend = load_backend(&path).await;

    // Verify it's empty
    assert_eq!(backend.unwrap().all_roots().await.unwrap().len(), 0);
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_load_invalid_file() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("invalid_file.json");

    // Create an invalid JSON file
    {
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "{{invalid json").unwrap();
    }

    // Attempt to load
    let result = load_backend(&path).await;

    // Verify it's an error
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{invalid json\n");
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_save_load_with_various_entries() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test_various_entries.json");

    // Setup a tree with multiple entries
    let backend = Arc::new(InMemory::new());

    // Top-level root
    let root_entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root_entry.id();
    backend.put_verified(root_entry).await.unwrap();

    // Child 1
    let child1 = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("child", b"1")
        .build()
        .expect("Child entry should build successfully");
    let child1_id = child1.id();
    backend.put_verified(child1).await.unwrap();

    // Child 2
    let child2 = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("child", b"2")
        .build()
        .expect("Child entry should build successfully");
    let child2_id = child2.id();
    backend.put_verified(child2).await.unwrap();

    // Grandchild (child of child1)
    let grandchild = Entry::builder(root_id.clone())
        .add_parent(child1_id.clone())
        .build()
        .expect("Grandchild entry should build successfully");
    let grandchild_id = grandchild.id();
    backend.put_verified(grandchild).await.unwrap();

    // Entry with subtree
    let entry_with_subtree = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("subtree1", b"subtree_data")
        .build()
        .expect("Entry with subtree should build successfully");
    let entry_with_subtree_id = entry_with_subtree.id();
    backend.put_verified(entry_with_subtree).await.unwrap();

    // Save to file
    save_backend(&backend, &file_path).unwrap();

    // Load back into a new backend
    let loaded_backend = load_backend(&file_path).await.unwrap();

    // Verify loaded data

    // Check we have the correct root
    let loaded_roots = loaded_backend.all_roots().await.unwrap();
    assert_eq!(loaded_roots.len(), 1);
    assert_eq!(loaded_roots[0], root_id);

    // Check we can retrieve all entries
    let loaded_tree = loaded_backend.get_tree(&root_id).await.unwrap();
    assert_eq!(loaded_tree.len(), 5); // root + 2 children + grandchild + entry_with_subtree

    // Check specific entries can be retrieved
    let _loaded_root = loaded_backend.get(&root_id).await.unwrap();
    // Entry is a pure data structure - it shouldn't know about settings
    // Settings logic is handled by Transaction

    let _loaded_grandchild = loaded_backend.get(&grandchild_id).await.unwrap();
    // Entry is a pure data structure - it shouldn't know about settings
    // Settings logic is handled by Transaction

    let loaded_entry_with_subtree = loaded_backend.get(&entry_with_subtree_id).await.unwrap();
    assert_eq!(
        loaded_entry_with_subtree.data("subtree1").unwrap(),
        b"subtree_data"
    );

    // Check tips match
    let orig_tips = backend.snapshot(&root_id).await.unwrap().into_tips();
    let loaded_tips = loaded_backend.snapshot(&root_id).await.unwrap().into_tips();
    assert_eq!(orig_tips.len(), loaded_tips.len());

    // Should have 3 tips (grandchild, entry_with_subtree, and child2)
    assert_eq!(loaded_tips.len(), 3);
    assert!(loaded_tips.contains(&grandchild_id));
    assert!(loaded_tips.contains(&entry_with_subtree_id));
    assert!(loaded_tips.contains(&child2_id));
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_load_wrong_version_fails() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("wrong_version.json");
    let backend = InMemory::new();
    backend
        .put_verified(Entry::root_builder().build().unwrap())
        .await
        .unwrap();
    let mut value = serde_json::to_value(&backend).unwrap();
    for version in [1, 3, 99] {
        value["_v"] = version.into();
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &original).unwrap();
        let error = load_backend(&path).await.unwrap_err();
        let eidetica::Error::Backend(error) = error else {
            panic!("expected a backend deserialization error");
        };
        let eidetica::backend::BackendError::DeserializationFailed { source } = *error else {
            panic!("expected a persistence deserialization error");
        };
        assert!(
            source
                .to_string()
                .contains(&format!("unsupported persistence version {version}"))
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_v0_migration_preserves_entries_and_instance_state_across_save() {
    use eidetica::{
        auth::crypto::PrivateKey,
        backend::{InstanceMetadata, VerificationStatus},
    };
    let backend = InMemory::new();
    let root = Entry::root_builder().build().unwrap();
    let parent = Entry::builder(root.id())
        .add_parent(root.id())
        .build()
        .unwrap();
    let child = Entry::builder(root.id())
        .add_parent(parent.id())
        .build()
        .unwrap();
    let failed = Entry::builder(root.id())
        .add_parent(root.id())
        .set_subtree_data("data", b"rejected")
        .build()
        .unwrap();
    backend.put_verified(root.clone()).await.unwrap();
    for entry in [&parent, &child, &failed] {
        backend.put(entry.clone()).await.unwrap();
    }
    backend
        .update_verification_status(&failed.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    let signing_key = PrivateKey::generate();
    backend
        .set_instance_metadata(&InstanceMetadata {
            id: signing_key.public_key(),
            users_db: root.id(),
            databases_db: root.id(),
            sync_db: Some(root.id()),
        })
        .await
        .unwrap();

    let raw = backend.snapshot(&root.id()).await.unwrap();
    let mut legacy = serde_json::to_value(&backend).unwrap();
    legacy["instance_secrets"] = serde_json::json!({"signing_key": signing_key});
    legacy.as_object_mut().unwrap().remove("verified");
    legacy["verification_status"][child.id().to_string()] = "Verified".into();
    legacy["cache"] = serde_json::json!({"legacy": "disposable"});
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("mainline.json");
    // Mainline files omitted _v; explicit 0 has the same compatibility path.
    for explicit_version in [false, true] {
        if explicit_version {
            legacy["_v"] = 0.into();
        } else {
            legacy.as_object_mut().unwrap().remove("_v");
        }
        let original = serde_json::to_vec(&legacy).unwrap();
        fs::write(&path, &original).unwrap();
        let loaded = load_backend(&path).await.unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "load must not rewrite the source"
        );
        assert_eq!(loaded.snapshot(&root.id()).await.unwrap(), raw);
        assert_eq!(
            loaded.verified_snapshot(&root.id()).await.unwrap().tips(),
            &[root.id()]
        );
        assert_eq!(
            loaded.get_verification_status(&child.id()).await.unwrap(),
            VerificationStatus::Unverified
        );
        assert_eq!(
            loaded.get_verification_status(&failed.id()).await.unwrap(),
            VerificationStatus::Failed
        );
        for entry in [&root, &parent, &child, &failed] {
            assert_eq!(
                loaded.get(&entry.id()).await.unwrap().to_dagcbor().unwrap(),
                entry.to_dagcbor().unwrap()
            );
        }
        save_backend(&loaded, &path).unwrap();
        let current: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(current["_v"], 2);
        for field in ["entries", "instance_metadata", "instance_secrets"] {
            assert_eq!(current[field], legacy[field], "preserve {field}");
        }
        assert!(current.get("cache").is_none());
        let reopened = load_backend(&path).await.unwrap();
        let reopened_value = serde_json::to_value(&reopened).unwrap();
        for field in [
            "_v",
            "entries",
            "verification_status",
            "instance_metadata",
            "instance_secrets",
        ] {
            assert_eq!(
                reopened_value[field], current[field],
                "reopen preserves {field}"
            );
        }
        // Tip caches contain HashSets: compare their canonical API snapshots,
        // not the incidental array ordering in serialized JSON.
        assert_eq!(reopened.snapshot(&root.id()).await.unwrap(), raw);
        for store in ["_root", "data"] {
            let expected = backend.store_snapshot(&root.id(), store).await.unwrap();
            assert_eq!(
                loaded.store_snapshot(&root.id(), store).await.unwrap(),
                expected
            );
            assert_eq!(
                reopened.store_snapshot(&root.id(), store).await.unwrap(),
                expected
            );
        }
        assert_eq!(
            reopened.verified_snapshot(&root.id()).await.unwrap().tips(),
            &[root.id()]
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_load_missing_version_defaults_to_v0() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("missing_version.json");

    // Write JSON without version field - should default to v0
    {
        let mut file = fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"entries":{{}},"verification_status":{{}},"tips":{{}}}}"#
        )
        .unwrap();
    }

    let result = load_backend(&path).await;
    assert!(
        result.is_ok(),
        "Should load file without version (defaults to v0): {:?}",
        result.err()
    );
}

// Test-only: store-and-promote helper (production `put` is Unverified-only).
use crate::helpers::TestVerify;

#[tokio::test]
async fn persisted_trust_reset_survives_restart_and_keeps_entries() {
    use eidetica::backend::VerificationStatus;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("eidetica.json");
    let backend = InMemory::new();
    let root = Entry::root_builder().build().unwrap();
    let id = root.id();
    backend.put_verified(root.clone()).await.unwrap();
    backend.save_to_file(&path).unwrap();
    let loaded = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    loaded.reset_local_verification().await.unwrap();
    loaded.save_to_file(&path).unwrap();
    let reopened = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    assert_eq!(
        reopened.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(reopened.get(&id).await.unwrap(), root);
}

#[tokio::test]
async fn failed_persisted_reset_write_leaves_old_snapshot_retryable() {
    use eidetica::backend::VerificationStatus;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("eidetica.json");
    let backend = InMemory::new();
    let entry = Entry::root_builder().build().unwrap();
    let id = entry.id();
    backend.put(entry.clone()).await.unwrap();
    backend
        .update_verification_status(&id, VerificationStatus::Failed)
        .await
        .unwrap();
    backend.save_to_file(&path).unwrap();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp);
    fs::create_dir(&tmp_path).unwrap(); // force the atomic replacement to fail before rename
    let loaded = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    loaded.reset_local_verification().await.unwrap();
    assert!(loaded.save_to_file(&path).is_err());
    let old = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    assert_eq!(
        old.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Failed
    );
    fs::remove_dir(&tmp_path).unwrap();
    old.reset_local_verification().await.unwrap();
    old.save_to_file(&path).unwrap();
    let reopened = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    assert_eq!(
        reopened.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(reopened.get(&id).await.unwrap(), entry);
}
