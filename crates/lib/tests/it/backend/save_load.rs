use eidetica::Entry;
use eidetica::backend::{BackendDB, database::InMemory};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[test]
fn test_in_memory_backend_save_and_load() {
    // Create a temporary file path
    let temp_dir = env!("CARGO_MANIFEST_DIR");
    let file_path = PathBuf::from(temp_dir).join("test_backend_save.json");

    // Setup: Create a backend with some data
    {
        let backend = InMemory::new();
        let entry = Entry::root_builder().build();
        backend.put_verified(entry).unwrap();

        // Save to file
        let save_result = backend.save_to_file(&file_path);
        assert!(save_result.is_ok());
    }

    // Verify file exists
    assert!(file_path.exists());

    // Load from file
    let load_result = InMemory::load_from_file(&file_path);
    assert!(load_result.is_ok());
    let loaded_backend = load_result.unwrap();

    // Verify data was loaded correctly
    let roots = loaded_backend.all_roots().unwrap();
    assert_eq!(roots.len(), 1);

    // Cleanup
    fs::remove_file(file_path).unwrap();
}

#[test]
fn test_load_non_existent_file() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test_data/non_existent_file.json");
    // Ensure file does not exist
    let _ = fs::remove_file(&path); // Ignore error if it doesn't exist

    // Load
    let backend = InMemory::load_from_file(&path);

    // Verify it's empty
    assert_eq!(backend.unwrap().all_roots().unwrap().len(), 0);
}

#[test]
fn test_load_invalid_file() {
    // Ensure target directory exists
    let test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test_data");
    fs::create_dir_all(&test_dir).unwrap();
    let path = test_dir.join("invalid_file.json");

    // Create an invalid JSON file
    {
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "{{invalid json").unwrap();
    }

    // Attempt to load
    let result = InMemory::load_from_file(&path);

    // Verify it's an error
    assert!(result.is_err());

    // Clean up
    fs::remove_file(&path).unwrap();
}

#[test]
fn test_save_load_with_various_entries() {
    // Create a temporary file path
    let test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test_data");
    fs::create_dir_all(&test_dir).unwrap();
    let file_path = test_dir.join("test_various_entries.json");

    // Setup a tree with multiple entries
    let backend = InMemory::new();

    // Top-level root
    let root_entry = Entry::root_builder().build();
    let root_id = root_entry.id();
    backend.put_verified(root_entry).unwrap();

    // Child 1
    let child1 = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("child", "1")
        .build();
    let child1_id = child1.id();
    backend.put_verified(child1).unwrap();

    // Child 2
    let child2 = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("child", "2")
        .build();
    let child2_id = child2.id();
    backend.put_verified(child2).unwrap();

    // Grandchild (child of child1)
    let grandchild = Entry::builder(root_id.clone())
        .add_parent(child1_id.clone())
        .build();
    let grandchild_id = grandchild.id();
    backend.put_verified(grandchild).unwrap();

    // Entry with subtree
    let entry_with_subtree = Entry::builder(root_id.clone())
        .set_subtree_data("subtree1", "subtree_data")
        .build();
    let entry_with_subtree_id = entry_with_subtree.id();
    backend.put_verified(entry_with_subtree).unwrap();

    // Save to file
    backend.save_to_file(&file_path).unwrap();

    // Load back into a new backend
    let loaded_backend = InMemory::load_from_file(&file_path).unwrap();

    // Verify loaded data

    // Check we have the correct root
    let loaded_roots = loaded_backend.all_roots().unwrap();
    assert_eq!(loaded_roots.len(), 1);
    assert_eq!(loaded_roots[0], root_id);

    // Check we can retrieve all entries
    let loaded_tree = loaded_backend.get_tree(&root_id).unwrap();
    assert_eq!(loaded_tree.len(), 5); // root + 2 children + grandchild + entry_with_subtree

    // Check specific entries can be retrieved
    let _loaded_root = loaded_backend.get(&root_id).unwrap();
    // Entry is a pure data structure - it shouldn't know about settings
    // Settings logic is handled by Transaction

    let _loaded_grandchild = loaded_backend.get(&grandchild_id).unwrap();
    // Entry is a pure data structure - it shouldn't know about settings
    // Settings logic is handled by Transaction

    let loaded_entry_with_subtree = loaded_backend.get(&entry_with_subtree_id).unwrap();
    assert_eq!(
        loaded_entry_with_subtree.data("subtree1").unwrap(),
        "subtree_data"
    );

    // Check tips match
    let orig_tips = backend.get_tips(&root_id).unwrap();
    let loaded_tips = loaded_backend.get_tips(&root_id).unwrap();
    assert_eq!(orig_tips.len(), loaded_tips.len());

    // Should have 3 tips (grandchild, entry_with_subtree, and child2)
    assert_eq!(loaded_tips.len(), 3);
    assert!(loaded_tips.contains(&grandchild_id));
    assert!(loaded_tips.contains(&entry_with_subtree_id));
    assert!(loaded_tips.contains(&child2_id));

    // Cleanup
    fs::remove_file(file_path).unwrap();
}
