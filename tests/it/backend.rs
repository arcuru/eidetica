use eideticadb::backend::{Backend, InMemoryBackend};
use eideticadb::entry::Entry;
use eideticadb::Error;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[test]
fn test_in_memory_backend_basic_operations() {
    let mut backend = InMemoryBackend::new();

    // Create and insert a test entry
    let data = "test_data".to_string();
    let entry = Entry::root_builder(data).build();
    let id = entry.id();

    // Put the entry
    let put_result = backend.put(entry);
    assert!(put_result.is_ok());

    // Get the entry back
    let get_result = backend.get(&id);
    assert!(get_result.is_ok());
    let retrieved_entry = get_result.unwrap();
    assert_eq!(retrieved_entry.id(), id);

    // Check all_roots
    let roots_result = backend.all_roots();
    assert!(roots_result.is_ok());
    let roots = roots_result.unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], id);
}

#[test]
fn test_in_memory_backend_tree_operations() {
    let mut backend = InMemoryBackend::new();

    // Create a root entry
    let root_data = "root_data".to_string();
    let root_entry = Entry::root_builder(root_data).build();
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Create child entries
    let child1_data = "child1_data".to_string();
    let child1_entry = Entry::builder(root_id.clone(), child1_data)
        .add_parent(root_id.clone())
        .build();
    let child1_id = child1_entry.id();
    backend.put(child1_entry).unwrap();

    let child2_data = "child2_data".to_string();
    let child2_entry = Entry::builder(root_id.clone(), child2_data)
        .add_parent(child1_id.clone())
        .build();
    let child2_id = child2_entry.id();
    backend.put(child2_entry).unwrap();

    // Test get_tips
    let tips_result = backend.get_tips(&root_id);
    assert!(tips_result.is_ok());
    let tips = tips_result.unwrap();
    assert_eq!(tips.len(), 1);
    assert!(tips.contains(&child2_id));

    // Test get_tree
    let tree_result = backend.get_tree(&root_id);
    assert!(tree_result.is_ok());
    let tree = tree_result.unwrap();
    assert_eq!(tree.len(), 3); // root + 2 children

    // Verify tree contains all three entries
    let tree_ids: Vec<String> = tree.iter().map(|e| e.id()).collect();
    assert!(tree_ids.contains(&root_id));
    assert!(tree_ids.contains(&child1_id));
    assert!(tree_ids.contains(&child2_id));
}

#[test]
fn test_in_memory_backend_subtree_operations() {
    let mut backend = InMemoryBackend::new();

    // Create a root entry with a subtree
    let root_data = "root_data".to_string();
    let root_entry = Entry::root_builder(root_data)
        .set_subtree_data("subtree1".to_string(), "root_subtree1_data".to_string())
        .build();
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Create child entry with subtree
    let child_data = "child_data".to_string();
    let child_entry = Entry::builder(root_id.clone(), child_data)
        .add_parent(root_id.clone())
        .set_subtree_data("subtree1".to_string(), "child_subtree1_data".to_string())
        .add_subtree_parent("subtree1", root_id.clone())
        .build();
    let child_id = child_entry.id();
    backend.put(child_entry).unwrap();

    // Test get_subtree_tips
    let subtree_tips_result = backend.get_subtree_tips(&root_id, "subtree1");
    assert!(subtree_tips_result.is_ok());
    let subtree_tips = subtree_tips_result.unwrap();
    assert_eq!(subtree_tips.len(), 1);
    assert_eq!(subtree_tips[0], child_id);

    // Test get_subtree
    let subtree_result = backend.get_subtree(&root_id, "subtree1");
    assert!(subtree_result.is_ok());
    let subtree = subtree_result.unwrap();
    assert_eq!(subtree.len(), 2); // root + child
}

#[test]
fn test_in_memory_backend_save_and_load() {
    // Create a temporary file path
    let temp_dir = env!("CARGO_MANIFEST_DIR");
    let file_path = PathBuf::from(temp_dir).join("test_backend_save.json");

    // Setup: Create a backend with some data
    {
        let mut backend = InMemoryBackend::new();
        let entry = Entry::root_builder("test_data".to_string()).build();
        backend.put(entry).unwrap();

        // Save to file
        let save_result = backend.save_to_file(&file_path);
        assert!(save_result.is_ok());
    }

    // Verify file exists
    assert!(file_path.exists());

    // Load from file
    let load_result = InMemoryBackend::load_from_file(&file_path);
    assert!(load_result.is_ok());
    let loaded_backend = load_result.unwrap();

    // Verify data was loaded correctly
    let roots = loaded_backend.all_roots().unwrap();
    assert_eq!(roots.len(), 1);

    // Cleanup
    fs::remove_file(file_path).unwrap();
}

#[test]
fn test_in_memory_backend_error_handling() {
    let backend = InMemoryBackend::new();

    // Test retrieving a non-existent entry
    let non_existent_id = "non_existent_id".to_string();
    let get_result = backend.get(&non_existent_id);
    assert!(get_result.is_err());

    // For some backend implementations like InMemoryBackend, get_tips might return
    // an empty vector instead of an error when the tree doesn't exist
    // Let's verify it returns either an error or an empty vector
    // FIXME: Code smell, backends should be consistent. Update this test once the API is defined.
    let tips_result = backend.get_tips(&non_existent_id);
    if let Ok(tips) = tips_result {
        // If it returns Ok, it should be an empty vector
        assert!(tips.is_empty());
    } else {
        // If it returns an error, that's also acceptable
        assert!(tips_result.is_err());
    }

    // Similarly, get_subtree might return an empty vector for non-existent trees
    let subtree_result = backend.get_subtree(&non_existent_id, "non_existent_subtree");
    if let Ok(entries) = subtree_result {
        assert!(entries.is_empty());
    } else {
        assert!(subtree_result.is_err());
    }

    // Similar to get_tips, get_subtree_tips might return an empty vector for non-existent trees
    let subtree_tips_result = backend.get_subtree_tips(&non_existent_id, "non_existent_subtree");
    if let Ok(tips) = subtree_tips_result {
        assert!(tips.is_empty());
    } else {
        assert!(subtree_tips_result.is_err());
    }
}

#[test]
fn test_in_memory_backend_complex_tree_structure() {
    let mut backend = InMemoryBackend::new();

    // Create a root entry
    let root_data = "root_data".to_string();
    let root_entry = Entry::root_builder(root_data).build();
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Create a diamond pattern: root -> A, B -> C
    // First level children
    let a_data = "a_data".to_string();
    let a_entry = Entry::builder(root_id.clone(), a_data)
        .add_parent(root_id.clone())
        .build();
    let a_id = a_entry.id();
    backend.put(a_entry).unwrap();

    let b_data = "b_data".to_string();
    let b_entry = Entry::builder(root_id.clone(), b_data)
        .add_parent(root_id.clone())
        .build();
    let b_id = b_entry.id();
    backend.put(b_entry).unwrap();

    // Second level: one child with two parents
    let c_data = "c_data".to_string();
    let c_entry = Entry::builder(root_id.clone(), c_data)
        .add_parent(a_id.clone())
        .add_parent(b_id.clone())
        .build();
    let c_id = c_entry.id();
    backend.put(c_entry).unwrap();

    // Test get_tips - should only return C since it has no children
    let tips_result = backend.get_tips(&root_id);
    assert!(tips_result.is_ok());
    let tips = tips_result.unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], c_id);

    // Test get_tree - should return all 4 entries in topological order
    let tree_result = backend.get_tree(&root_id);
    assert!(tree_result.is_ok());
    let tree = tree_result.unwrap();
    assert_eq!(tree.len(), 4);

    // The root should be first in topological order
    assert_eq!(tree[0].id(), root_id);

    // C should be last as it depends on both A and B
    assert_eq!(tree[3].id(), c_id);

    // Verify A and B are in between (order between them could vary)
    // FIXME: This will be consistent once the API is defined. Update this test once the total ordering is fully implemented.
    let middle_ids: Vec<String> = vec![tree[1].id(), tree[2].id()];
    assert!(middle_ids.contains(&a_id));
    assert!(middle_ids.contains(&b_id));

    // Now test a new entry: add D which has C as a parent
    let d_data = "d_data".to_string();
    let d_entry = Entry::builder(root_id.clone(), d_data)
        .add_parent(c_id.clone())
        .build();
    let d_id = d_entry.id();
    backend.put(d_entry).unwrap();

    // Tips should now be D
    let final_tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(final_tips.len(), 1);
    assert_eq!(final_tips[0], d_id);
}

#[test]
fn test_backend_get_tree_from_tips() {
    let mut backend = InMemoryBackend::new();
    let root_id = "tree_root".to_string();

    // Create entries: root -> e1 -> e2a, e2b
    let root_entry = Entry::builder(root_id.clone(), "root_data".to_string())
        .add_parent(root_id.clone())
        .build();
    let root_entry_id = root_entry.id();
    backend.put(root_entry).unwrap();

    let e1_entry = Entry::builder(root_id.clone(), "e1_data".to_string())
        .add_parent(root_entry_id.clone())
        .build();
    let e1_id = e1_entry.id();
    backend.put(e1_entry).unwrap();

    let e2a_entry = Entry::builder(root_id.clone(), "e2a_data".to_string())
        .add_parent(e1_id.clone())
        .build();
    let e2a_id = e2a_entry.id();
    backend.put(e2a_entry).unwrap();

    let e2b_entry = Entry::builder(root_id.clone(), "e2b_data".to_string())
        .add_parent(e1_id.clone())
        .build();
    let e2b_id = e2b_entry.id();
    backend.put(e2b_entry).unwrap();

    // --- Test with single tip e2a ---
    let tree_e2a = backend
        .get_tree_from_tips(&root_id, &[e2a_id.clone()])
        .expect("Failed to get tree from tip e2a");
    assert_eq!(tree_e2a.len(), 3, "Tree from e2a should have root, e1, e2a");
    let ids_e2a: Vec<_> = tree_e2a.iter().map(|e| e.id()).collect();
    assert!(ids_e2a.contains(&root_entry_id));
    assert!(ids_e2a.contains(&e1_id));
    assert!(ids_e2a.contains(&e2a_id));
    assert!(!ids_e2a.contains(&e2b_id)); // Should not contain e2b

    // Verify topological order (root -> e1 -> e2a)
    assert_eq!(tree_e2a[0].id(), root_entry_id);
    assert_eq!(tree_e2a[1].id(), e1_id);
    assert_eq!(tree_e2a[2].id(), e2a_id);

    // --- Test with both tips e2a and e2b ---
    let tree_both = backend
        .get_tree_from_tips(&root_id, &[e2a_id.clone(), e2b_id.clone()])
        .expect("Failed to get tree from tips e2a, e2b");
    assert_eq!(
        tree_both.len(),
        4,
        "Tree from both tips should have all 4 entries"
    );
    let ids_both: Vec<_> = tree_both.iter().map(|e| e.id()).collect();
    assert!(ids_both.contains(&root_entry_id));
    assert!(ids_both.contains(&e1_id));
    assert!(ids_both.contains(&e2a_id));
    assert!(ids_both.contains(&e2b_id));

    // Verify topological order (root -> e1 -> {e2a, e2b})
    assert_eq!(tree_both[0].id(), root_entry_id);
    assert_eq!(tree_both[1].id(), e1_id);
    // Order of e2a and e2b might vary, check they are last two
    let last_two: Vec<_> = vec![tree_both[2].id(), tree_both[3].id()];
    assert!(last_two.contains(&e2a_id));
    assert!(last_two.contains(&e2b_id));

    // --- Test with non-existent tip ---
    let tree_bad_tip = backend
        .get_tree_from_tips(&root_id, &["bad_tip_id".to_string()])
        .expect("Failed to get tree with non-existent tip");
    assert!(
        tree_bad_tip.is_empty(),
        "Getting tree from non-existent tip should return empty vector"
    );

    // --- Test with non-existent tree root ---
    let bad_root_string = "bad_root".to_string();
    let tree_bad_root = backend
        .get_tree_from_tips(&bad_root_string, &[e1_id.clone()])
        .expect("Failed to get tree with non-existent root");
    assert!(
        tree_bad_root.is_empty(),
        "Getting tree from non-existent root should return empty vector"
    );
}

#[test]
fn test_backend_get_subtree_from_tips() {
    let mut backend = InMemoryBackend::new();
    let subtree_name_string = "my_subtree".to_string();

    // Create entries: root -> e1 -> e2a, e2b
    // root: has subtree
    // e1: no subtree
    // e2a: has subtree
    // e2b: has subtree

    let entry_root = Entry::root_builder("root_data".to_string())
        .set_subtree_data(subtree_name_string.clone(), "root_sub_data".to_string())
        .build();
    let root_entry_id = entry_root.id();
    backend.put(entry_root).unwrap();

    let e1 = Entry::builder(root_entry_id.clone(), "e1_data".to_string())
        .add_parent(root_entry_id.clone())
        .build();
    let e1_id = e1.id();
    backend.put(e1).unwrap();

    let e2a = Entry::builder(root_entry_id.clone(), "e2a_data".to_string())
        .add_parent(e1_id.clone())
        .set_subtree_data(subtree_name_string.clone(), "e2a_sub_data".to_string())
        .add_subtree_parent(subtree_name_string.as_str(), root_entry_id.clone())
        .build();
    let e2a_id = e2a.id();
    backend.put(e2a).unwrap();

    let e2b = Entry::builder(root_entry_id.clone(), "e2b_data".to_string())
        .add_parent(e1_id.clone())
        .set_subtree_data(subtree_name_string.clone(), "e2b_sub_data".to_string())
        .add_subtree_parent(subtree_name_string.as_str(), root_entry_id.clone())
        .build();
    let e2b_id = e2b.id();
    backend.put(e2b).unwrap();

    // --- Test with single tip e2a ---
    let subtree_e2a = backend
        .get_subtree_from_tips(&root_entry_id, &subtree_name_string, &[e2a_id.clone()])
        .expect("Failed to get subtree from tip e2a");
    // Should contain root and e2a (which have the subtree), but not e1 (no subtree) or e2b (not in history of tip e2a)
    assert_eq!(
        subtree_e2a.len(),
        2,
        "Subtree from e2a should have root, e2a"
    );
    let ids_e2a: Vec<_> = subtree_e2a.iter().map(|e| e.id()).collect();
    assert!(ids_e2a.contains(&root_entry_id));
    assert!(!ids_e2a.contains(&e1_id)); // e1 doesn't have the subtree
    assert!(ids_e2a.contains(&e2a_id));
    assert!(!ids_e2a.contains(&e2b_id)); // e2b is not an ancestor of e2a

    // Verify topological order (root -> e2a)
    assert_eq!(subtree_e2a[0].id(), root_entry_id);
    assert_eq!(subtree_e2a[1].id(), e2a_id);

    // --- Test with both tips e2a and e2b ---
    let subtree_both = backend
        .get_subtree_from_tips(
            &root_entry_id,
            &subtree_name_string,
            &[e2a_id.clone(), e2b_id.clone()],
        )
        .expect("Failed to get subtree from tips e2a, e2b");
    // Should contain root, e2a, e2b (all have the subtree)
    assert_eq!(
        subtree_both.len(),
        3,
        "Subtree from both tips should have root, e2a, e2b"
    );
    let ids_both: Vec<_> = subtree_both.iter().map(|e| e.id()).collect();
    assert!(ids_both.contains(&root_entry_id));
    assert!(!ids_both.contains(&e1_id));
    assert!(ids_both.contains(&e2a_id));
    assert!(ids_both.contains(&e2b_id));

    // Verify topological order (root -> {e2a, e2b})
    assert_eq!(subtree_both[0].id(), root_entry_id);
    let last_two: Vec<_> = vec![subtree_both[1].id(), subtree_both[2].id()];
    assert!(last_two.contains(&e2a_id));
    assert!(last_two.contains(&e2b_id));

    // --- Test with non-existent subtree name ---
    let bad_name_string = "bad_name".to_string();
    let subtree_bad_name =
        backend.get_subtree_from_tips(&root_entry_id, &bad_name_string, &[e2a_id.clone()]);
    assert!(
        subtree_bad_name.is_ok(),
        "Getting subtree with bad name should be ok..."
    );
    assert!(
        subtree_bad_name.unwrap().is_empty(),
        "...but return empty list"
    );
    // --- Test with non-existent tip ---
    let subtree_bad_tip = backend
        .get_subtree_from_tips(
            &root_entry_id,
            &subtree_name_string,
            &["bad_tip_id".to_string()],
        )
        .expect("Failed to get subtree with non-existent tip");
    assert!(
        subtree_bad_tip.is_empty(),
        "Getting subtree from non-existent tip should return empty list"
    );

    // --- Test with non-existent tree root ---
    let bad_root_string_2 = "bad_root".to_string();
    let subtree_bad_root = backend
        .get_subtree_from_tips(&bad_root_string_2, &subtree_name_string, &[e1_id.clone()])
        .expect("Failed to get subtree with non-existent root");
    assert!(
        subtree_bad_root.is_empty(),
        "Getting subtree from non-existent root should return empty list"
    );
}

#[test]
fn test_calculate_entry_height() {
    let mut backend = InMemoryBackend::new();

    // Create a simple tree:
    // root -> A -> B -> C\
    //    \                -> D
    //     \-> E -> F --->/

    let root = Entry::root_builder("{\"name\":\"root\"}".to_string()).build();
    let root_id = root.id();

    let entry_a = Entry::builder(root_id.clone(), "{\"name\":\"A\"}".to_string())
        .add_parent(root_id.clone())
        .build();
    let id_a = entry_a.id();

    let entry_b = Entry::builder(root_id.clone(), "{\"name\":\"B\"}".to_string())
        .add_parent(id_a.clone())
        .build();
    let id_b = entry_b.id();

    let entry_c = Entry::builder(root_id.clone(), "{\"name\":\"C\"}".to_string())
        .add_parent(id_b.clone())
        .build();
    let id_c = entry_c.id();

    let entry_e = Entry::builder(root_id.clone(), "{\"name\":\"E\"}".to_string())
        .add_parent(root_id.clone())
        .build();
    let id_e = entry_e.id();

    let entry_f = Entry::builder(root_id.clone(), "{\"name\":\"F\"}".to_string())
        .add_parent(id_e.clone())
        .build();
    let id_f = entry_f.id();

    let entry_d = Entry::builder(root_id.clone(), "{\"name\":\"D\"}".to_string())
        .add_parent(id_c.clone())
        .add_parent(id_f.clone())
        .build();
    let id_d = entry_d.id();

    // Insert all entries
    backend.put(root).unwrap();
    backend.put(entry_a).unwrap();
    backend.put(entry_b).unwrap();
    backend.put(entry_c).unwrap();
    backend.put(entry_d).unwrap();
    backend.put(entry_e).unwrap();
    backend.put(entry_f).unwrap();

    // Check that the tree was created correctly
    // by verifying the tip is entry D
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_d);

    // Check the full tree contains all 7 entries
    let tree = backend
        .get_tree_from_tips(&root_id, &[id_d.clone()])
        .unwrap();
    assert_eq!(tree.len(), 7, "Tree should contain all 7 entries");

    // Calculate heights map and verify correct heights
    let heights = backend.calculate_heights(&root_id, None).unwrap();

    // Root should have height 0
    assert_eq!(heights.get(&root_id).unwrap_or(&9999), &0);

    // First level entries should have height 1
    assert_eq!(heights.get(&id_a).unwrap_or(&9999), &1);
    assert_eq!(heights.get(&id_e).unwrap_or(&9999), &1);

    // Second level entries should have height 2
    assert_eq!(heights.get(&id_b).unwrap_or(&9999), &2);
    assert_eq!(heights.get(&id_f).unwrap_or(&9999), &2);

    // Third level entries should have height 3
    assert_eq!(heights.get(&id_c).unwrap_or(&9999), &3);

    // D should have a height of 4, not 3
    assert_eq!(heights.get(&id_d).unwrap_or(&9999), &4);
}

#[test]
fn test_load_non_existent_file() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test_data/non_existent_file.json");
    // Ensure file does not exist
    let _ = fs::remove_file(&path); // Ignore error if it doesn't exist

    // Load
    let backend = InMemoryBackend::load_from_file(&path);

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
    let result = InMemoryBackend::load_from_file(&path);

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
    let mut backend = InMemoryBackend::new();

    // Top-level root
    let root_entry = Entry::root_builder("root_data".to_string()).build();
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Child 1
    let child1 = Entry::builder(root_id.clone(), "child1_data".to_string())
        .add_parent(root_id.clone())
        .build();
    let child1_id = child1.id();
    backend.put(child1).unwrap();

    // Child 2
    let child2 = Entry::builder(root_id.clone(), "child2_data".to_string())
        .add_parent(root_id.clone())
        .build();
    let child2_id = child2.id();
    backend.put(child2).unwrap();

    // Grandchild (child of child1)
    let grandchild = Entry::builder(root_id.clone(), "grandchild_data".to_string())
        .add_parent(child1_id.clone())
        .build();
    let grandchild_id = grandchild.id();
    backend.put(grandchild).unwrap();

    // Entry with subtree
    let entry_with_subtree = Entry::builder(root_id.clone(), "entry_with_subtree_data".to_string())
        .set_subtree_data("subtree1".to_string(), "subtree_data".to_string())
        .build();
    let entry_with_subtree_id = entry_with_subtree.id();
    backend.put(entry_with_subtree).unwrap();

    // Save to file
    backend.save_to_file(&file_path).unwrap();

    // Load back into a new backend
    let loaded_backend = InMemoryBackend::load_from_file(&file_path).unwrap();

    // Verify loaded data

    // Check we have the correct root
    let loaded_roots = loaded_backend.all_roots().unwrap();
    assert_eq!(loaded_roots.len(), 1);
    assert_eq!(loaded_roots[0], root_id);

    // Check we can retrieve all entries
    let loaded_tree = loaded_backend.get_tree(&root_id).unwrap();
    assert_eq!(loaded_tree.len(), 5); // root + 2 children + grandchild + entry_with_subtree

    // Check specific entries can be retrieved
    let loaded_root = loaded_backend.get(&root_id).unwrap();
    assert_eq!(loaded_root.get_settings().unwrap(), "root_data");

    let loaded_grandchild = loaded_backend.get(&grandchild_id).unwrap();
    assert_eq!(loaded_grandchild.get_settings().unwrap(), "grandchild_data");

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

#[test]
fn test_sort_entries() {
    let mut backend = InMemoryBackend::new();

    // Create a simple tree with mixed order
    let root = Entry::root_builder("{}".to_string()).build();
    let root_id = root.id();

    let entry_a = Entry::builder(root_id.clone(), "{}".to_string())
        .add_parent(root_id.clone())
        .build();
    let id_a = entry_a.id();

    let entry_b = Entry::builder(root_id.clone(), "{}".to_string())
        .add_parent(id_a.clone())
        .build();
    let id_b = entry_b.id();

    let entry_c = Entry::builder(root_id.clone(), "{}".to_string())
        .add_parent(id_b.clone())
        .build();

    // Store all entries in backend
    backend.put(root.clone()).unwrap();
    backend.put(entry_a.clone()).unwrap();
    backend.put(entry_b.clone()).unwrap();
    backend.put(entry_c.clone()).unwrap();

    // Create a vector with entries in random order
    let mut entries = vec![
        entry_c.clone(),
        root.clone(),
        entry_b.clone(),
        entry_a.clone(),
    ];

    // Sort the entries
    backend
        .sort_entries_by_height(&root_id, &mut entries)
        .unwrap();

    // Check the sorted order: root, A, B, C (by height)
    assert_eq!(entries[0].id(), root_id);
    assert_eq!(entries[1].id(), id_a);
    assert_eq!(entries[2].id(), id_b);
    assert_eq!(entries[3].id(), entry_c.id());

    // Test with an empty vector (should not panic)
    let mut empty_entries = Vec::new();
    backend
        .sort_entries_by_height(&root_id, &mut empty_entries)
        .unwrap();
    assert!(empty_entries.is_empty());
}

#[test]
fn test_save_load_in_memory_backend() {
    let test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test_data");
    fs::create_dir_all(&test_dir).unwrap();
    let path = test_dir.join("test_save_load.json");

    let mut backend1 = InMemoryBackend::new();
    let entry1 = Entry::root_builder("{\"key\":\"value1\"}".to_string()).build();
    let entry2 = Entry::root_builder("{\"key\":\"value2\"}".to_string()).build();

    let id1 = entry1.id();
    let id2 = entry2.id();

    backend1.put(entry1.clone()).unwrap();
    backend1.put(entry2.clone()).unwrap();

    // Save
    backend1.save_to_file(&path).unwrap();

    // Load into a new backend
    let backend2 = InMemoryBackend::load_from_file(&path).unwrap();

    // Verify contents
    assert_eq!(backend2.get(&id1).unwrap(), &entry1);
    assert_eq!(backend2.get(&id2).unwrap(), &entry2);

    // Clean up
    fs::remove_file(path).unwrap();
}

#[test]
fn test_all_roots() {
    let mut backend = InMemoryBackend::new();

    // Initially, there should be no roots
    assert!(backend.all_roots().unwrap().is_empty());

    // Add a simple top-level entry (a root)
    let root1 = Entry::root_builder("root1 data".to_string()).build();
    let root1_id = root1.id();
    backend.put(root1).unwrap();

    let root2 = Entry::root_builder("root2 data".to_string()).build();
    let root2_id = root2.id();
    backend.put(root2).unwrap();

    // Test with two roots
    let roots = backend.all_roots().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));

    // Add a child under root1
    let child = Entry::builder(root1_id.clone(), "child data".to_string())
        .add_parent(root1_id.clone())
        .build();
    backend.put(child).unwrap();

    // Should still have only the two roots
    let roots = backend.all_roots().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));
}

#[test]
fn test_get_tips() {
    let mut backend = InMemoryBackend::new();

    // Create a simple tree structure:
    // Root -> A -> B
    //    \-> C

    let root = Entry::root_builder("root data".to_string()).build();
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // Initially, root is the only tip
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], root_id);

    // Add child A
    let entry_a = Entry::builder(root_id.clone(), "A".to_string())
        .add_parent(root_id.clone())
        .build();
    let id_a = entry_a.id();
    backend.put(entry_a.clone()).unwrap();

    // Now A should be the only tip
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_a);

    // Add child B from A
    let entry_b = Entry::builder(root_id.clone(), "B".to_string())
        .add_parent(id_a.clone())
        .build();
    let id_b = entry_b.id();
    backend.put(entry_b.clone()).unwrap();

    // Now B should be the only tip from that branch
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_b);

    // Add child C directly from Root (creates a branch)
    let entry_c = Entry::builder(root_id.clone(), "C".to_string())
        .add_parent(root_id.clone())
        .build();
    let id_c = entry_c.id();
    backend.put(entry_c.clone()).unwrap();

    // Now should have 2 tips: B and C
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 2);
    assert!(tips.contains(&id_b));
    assert!(tips.contains(&id_c));
}

#[test]
fn test_put_get_entry() {
    let mut backend = InMemoryBackend::new();

    // Test putting and getting an entry
    let entry = Entry::root_builder("test data".to_string()).build();
    let id = entry.id();

    // Put
    let put_result = backend.put(entry.clone());
    assert!(put_result.is_ok());

    // Get
    let get_result = backend.get(&id);
    assert!(get_result.is_ok());
    let retrieved = get_result.unwrap();
    assert_eq!(retrieved, &entry);

    // Try to get a non-existent ID
    let non_existent = "non_existent_id".to_string();
    let invalid_get = backend.get(&non_existent);
    assert!(matches!(invalid_get, Err(Error::NotFound)));
}

#[test]
fn test_get_subtree_tips() {
    let mut backend = InMemoryBackend::new();

    // Create a tree with subtrees
    let root = Entry::root_builder("root data".to_string()).build();
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // Add entry A with subtree "sub1"
    let entry_a = Entry::builder(root_id.clone(), "A".to_string())
        .add_parent(root_id.clone())
        .set_subtree_data("sub1".to_string(), "A sub1 data".to_string())
        .build();
    let id_a = entry_a.id();
    backend.put(entry_a).unwrap();

    // Initially, A is the only tip in subtree "sub1"
    let sub1_tips = backend.get_subtree_tips(&root_id, "sub1").unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_a);

    // Add entry B with subtree "sub1" as child of A
    let entry_b = Entry::builder(root_id.clone(), "B".to_string())
        .add_parent(id_a.clone())
        .set_subtree_data("sub1".to_string(), "B sub1 data".to_string())
        .add_subtree_parent("sub1", id_a.clone())
        .build();
    let id_b = entry_b.id();
    backend.put(entry_b).unwrap();

    // Now B is the only tip in subtree "sub1"
    let sub1_tips = backend.get_subtree_tips(&root_id, "sub1").unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Add entry C with subtree "sub2" (different subtree)
    let entry_c = Entry::builder(root_id.clone(), "C".to_string())
        .add_parent(root_id.clone())
        .set_subtree_data("sub2".to_string(), "C sub2 data".to_string())
        .build();
    let id_c = entry_c.id();
    backend.put(entry_c).unwrap();

    // Check tips for subtree "sub1" (should still be just B)
    let sub1_tips = backend.get_subtree_tips(&root_id, "sub1").unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Check tips for subtree "sub2" (should be just C)
    let sub2_tips = backend.get_subtree_tips(&root_id, "sub2").unwrap();
    assert_eq!(sub2_tips.len(), 1);
    assert_eq!(sub2_tips[0], id_c);

    // Add entry D with both subtrees "sub1" and "sub2"
    let entry_d = Entry::builder(root_id.clone(), "D".to_string())
        .add_parent(id_b.clone())
        .add_parent(id_c.clone())
        .set_subtree_data("sub1".to_string(), "D sub1 data".to_string())
        .add_subtree_parent("sub1", id_b.clone())
        .set_subtree_data("sub2".to_string(), "D sub2 data".to_string())
        .add_subtree_parent("sub2", id_c.clone())
        .build();
    let id_d = entry_d.id();
    backend.put(entry_d).unwrap();

    // Now D should be the tip for both subtrees
    let sub1_tips = backend.get_subtree_tips(&root_id, "sub1").unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_d);

    let sub2_tips = backend.get_subtree_tips(&root_id, "sub2").unwrap();
    assert_eq!(sub2_tips.len(), 1);
    assert_eq!(sub2_tips[0], id_d);
}

#[test]
fn test_get_tree() {
    let mut backend = InMemoryBackend::new();

    // Create a simple tree with three entries
    let root = Entry::root_builder("root data".to_string()).build();
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    let child = Entry::builder(root_id.clone(), "child".to_string())
        .add_parent(root_id.clone())
        .build();
    let child_id = child.id();
    backend.put(child.clone()).unwrap();

    let grandchild = Entry::builder(root_id.clone(), "grandchild".to_string())
        .add_parent(child_id.clone())
        .build();
    backend.put(grandchild.clone()).unwrap();

    // Get the full tree
    let tree = backend.get_tree(&root_id).unwrap();

    // Should have all 3 entries
    assert_eq!(tree.len(), 3);

    // Verify each entry is in the tree
    let has_root = tree.iter().any(|e| e.id() == root_id);
    let has_child = tree.iter().any(|e| e.id() == child_id);
    let has_grandchild = tree.iter().any(|e| e.id() == grandchild.id());

    assert!(has_root);
    assert!(has_child);
    assert!(has_grandchild);
}

#[test]
fn test_get_subtree() {
    let mut backend = InMemoryBackend::new();

    // Create a tree with a subtree
    let root = Entry::root_builder("root data".to_string()).build();
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // Create children with and without subtree data

    // Child 1 - with subtree
    let child1 = Entry::builder(root_id.clone(), "child1".to_string())
        .add_parent(root_id.clone())
        .set_subtree_data("subtree1".to_string(), "child1 data".to_string())
        .build();
    let child1_id = child1.id();
    backend.put(child1.clone()).unwrap();

    // Child 2 - without subtree
    let child2 = Entry::builder(root_id.clone(), "child2".to_string())
        .add_parent(root_id.clone())
        .build();
    let child2_id = child2.id();
    backend.put(child2.clone()).unwrap();

    // Grandchild 1 - with subtree, child of child1
    let grandchild1 = Entry::builder(root_id.clone(), "grandchild1".to_string())
        .add_parent(child1_id.clone())
        .set_subtree_data("subtree1".to_string(), "grandchild1 data".to_string())
        .add_subtree_parent("subtree1", child1_id.clone())
        .build();
    let gc1_id = grandchild1.id();
    backend.put(grandchild1.clone()).unwrap();

    // Grandchild 2 - with subtree, but from different parent (child2)
    let grandchild2 = Entry::builder(root_id.clone(), "grandchild2".to_string())
        .add_parent(child2_id.clone())
        .set_subtree_data("subtree1".to_string(), "grandchild2 data".to_string())
        .build();
    let grandchild2_id = grandchild2.id();
    // No subtree parent set, so it starts a new subtree branch
    backend.put(grandchild2.clone()).unwrap();

    // Get the subtree
    let subtree = backend.get_subtree(&root_id, "subtree1").unwrap();

    // Should have just the 3 entries with subtree data
    assert_eq!(subtree.len(), 3);

    // Check that the right entries are included
    let entry_ids: Vec<String> = subtree.iter().map(|e| e.id().clone()).collect();
    assert!(entry_ids.contains(&child1_id));
    assert!(entry_ids.contains(&gc1_id));
    assert!(entry_ids.contains(&grandchild2_id));

    // Child2 shouldn't be in the subtree (no subtree data)
    assert!(!entry_ids.contains(&child2_id));
}

#[test]
fn test_calculate_subtree_height() {
    let mut backend = InMemoryBackend::new();

    // Create a tree with a subtree that has a different structure
    let root = Entry::root_builder("root data".to_string()).build();
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // A
    let entry_a = Entry::builder(root_id.clone(), "A".to_string())
        .add_parent(root_id.clone())
        .set_subtree_data("sub1".to_string(), "A_sub1".to_string())
        .build();
    let id_a = entry_a.id();
    backend.put(entry_a.clone()).unwrap();

    // B (after A in main tree)
    let entry_b = Entry::builder(root_id.clone(), "B".to_string())
        .add_parent(id_a.clone())
        .set_subtree_data("sub1".to_string(), "B_sub1".to_string())
        .build();
    // B is directly under root in subtree (not under A)
    // So we don't set subtree parents
    let id_b = entry_b.id();
    backend.put(entry_b.clone()).unwrap();

    // C (after B in main tree)
    let entry_c = Entry::builder(root_id.clone(), "C".to_string())
        .add_parent(id_b.clone())
        .set_subtree_data("sub1".to_string(), "C_sub1".to_string())
        .add_subtree_parent("sub1", id_a.clone())
        .add_subtree_parent("sub1", id_b.clone())
        .build();
    let id_c = entry_c.id();
    backend.put(entry_c.clone()).unwrap();

    // Calculate heights for main tree
    let main_heights = backend.calculate_heights(&root_id, None).unwrap();

    // Main tree: root -> A -> B -> C
    assert_eq!(main_heights.get(&root_id).unwrap_or(&9999), &0);
    assert_eq!(main_heights.get(&id_a).unwrap_or(&9999), &1);
    assert_eq!(main_heights.get(&id_b).unwrap_or(&9999), &2);
    assert_eq!(main_heights.get(&id_c).unwrap_or(&9999), &3);

    // Calculate heights for subtree
    let sub_heights = backend.calculate_heights(&root_id, Some("sub1")).unwrap();

    // Subtree structure:
    // A   B
    //  \ /
    //   C
    assert_eq!(sub_heights.get(&id_a).unwrap(), &0);
    assert_eq!(sub_heights.get(&id_b).unwrap(), &0);
    assert_eq!(sub_heights.get(&id_c).unwrap(), &1);
}
