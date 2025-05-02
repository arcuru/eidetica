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
    let entry = Entry::new_top_level(data);
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
    let root_entry = Entry::new_top_level(root_data);
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Create child entries
    let child1_data = "child1_data".to_string();
    let mut child1_entry = Entry::new(root_id.clone(), child1_data);
    child1_entry.set_parents(vec![root_id.clone()]);
    let child1_id = child1_entry.id();
    backend.put(child1_entry).unwrap();

    let child2_data = "child2_data".to_string();
    let mut child2_entry = Entry::new(root_id.clone(), child2_data);
    child2_entry.set_parents(vec![child1_id.clone()]);
    let child2_id = child2_entry.id();
    backend.put(child2_entry).unwrap();

    // Test get_tips
    let tips_result = backend.get_tips(&root_id);
    assert!(tips_result.is_ok());
    let tips = tips_result.unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], child2_id);

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
    let mut root_entry = Entry::new_top_level(root_data);
    let subtree_name = "subtree1";
    root_entry
        .set_subtree_data(subtree_name.to_string(), "subtree_data".to_string())
        .unwrap();
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Create child entry with subtree
    let child_data = "child_data".to_string();
    let mut child_entry = Entry::new(root_id.clone(), child_data);
    child_entry.set_parents(vec![root_id.clone()]);
    child_entry
        .set_subtree_data(subtree_name.to_string(), "child_subtree_data".to_string())
        .unwrap();
    child_entry.set_subtree_parents(subtree_name, vec![root_id.clone()]);
    let child_id = child_entry.id();
    backend.put(child_entry).unwrap();

    // Test get_subtree_tips
    let subtree_tips_result = backend.get_subtree_tips(&root_id, subtree_name);
    assert!(subtree_tips_result.is_ok());
    let subtree_tips = subtree_tips_result.unwrap();
    assert_eq!(subtree_tips.len(), 1);
    assert_eq!(subtree_tips[0], child_id);

    // Test get_subtree
    let subtree_result = backend.get_subtree(&root_id, subtree_name);
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
        let entry = Entry::new_top_level("test_data".to_string());
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
    // FIXME: Code smell, backends should be consistent.
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
    let root_entry = Entry::new_top_level(root_data);
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Create a diamond pattern: root -> A, B -> C
    // First level children
    let a_data = "a_data".to_string();
    let mut a_entry = Entry::new(root_id.clone(), a_data);
    a_entry.set_parents(vec![root_id.clone()]);
    let a_id = a_entry.id();
    backend.put(a_entry).unwrap();

    let b_data = "b_data".to_string();
    let mut b_entry = Entry::new(root_id.clone(), b_data);
    b_entry.set_parents(vec![root_id.clone()]);
    let b_id = b_entry.id();
    backend.put(b_entry).unwrap();

    // Second level: one child with two parents
    let c_data = "c_data".to_string();
    let mut c_entry = Entry::new(root_id.clone(), c_data);
    c_entry.set_parents(vec![a_id.clone(), b_id.clone()]);
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
    let mut d_entry = Entry::new(root_id.clone(), d_data);
    d_entry.set_parents(vec![c_id.clone()]);
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
    let entry_root = Entry::new(root_id.clone(), "root_data".to_string());
    let root_entry_id = entry_root.id();
    backend.put(entry_root).unwrap();

    let mut e1 = Entry::new(root_id.clone(), "e1_data".to_string());
    e1.set_parents(vec![root_entry_id.clone()]);
    let e1_id = e1.id();
    backend.put(e1).unwrap();

    let mut e2a = Entry::new(root_id.clone(), "e2a_data".to_string());
    e2a.set_parents(vec![e1_id.clone()]);
    let e2a_id = e2a.id();
    backend.put(e2a).unwrap();

    let mut e2b = Entry::new(root_id.clone(), "e2b_data".to_string());
    e2b.set_parents(vec![e1_id.clone()]);
    let e2b_id = e2b.id();
    backend.put(e2b).unwrap();

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
    let root_id_string = "tree_root".to_string();
    let subtree_name_string = "my_subtree".to_string();

    // Create entries: root -> e1 -> e2a, e2b
    // root: has subtree
    // e1: no subtree
    // e2a: has subtree
    // e2b: has subtree

    let mut entry_root = Entry::new(root_id_string.clone(), "root_data".to_string());
    entry_root
        .set_subtree_data(subtree_name_string.clone(), "root_sub_data".to_string())
        .unwrap();
    let root_entry_id = entry_root.id();
    backend.put(entry_root).unwrap();

    let mut e1 = Entry::new(root_id_string.clone(), "e1_data".to_string()); // No subtree
    e1.set_parents(vec![root_entry_id.clone()]);
    let e1_id = e1.id();
    backend.put(e1).unwrap();

    let mut e2a = Entry::new(root_id_string.clone(), "e2a_data".to_string());
    e2a.set_parents(vec![e1_id.clone()]);
    e2a.set_subtree_data(subtree_name_string.clone(), "e2a_sub_data".to_string())
        .unwrap();
    e2a.set_subtree_parents(&subtree_name_string, vec![root_entry_id.clone()]);
    let e2a_id = e2a.id();
    backend.put(e2a).unwrap();

    let mut e2b = Entry::new(root_id_string.clone(), "e2b_data".to_string());
    e2b.set_parents(vec![e1_id.clone()]);
    e2b.set_subtree_data(subtree_name_string.clone(), "e2b_sub_data".to_string())
        .unwrap();
    e2b.set_subtree_parents(&subtree_name_string, vec![root_entry_id.clone()]);
    let e2b_id = e2b.id();
    backend.put(e2b).unwrap();

    // --- Test with single tip e2a ---
    let subtree_e2a = backend
        .get_subtree_from_tips(&root_id_string, &subtree_name_string, &[e2a_id.clone()])
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
            &root_id_string,
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
        backend.get_subtree_from_tips(&root_id_string, &bad_name_string, &[e2a_id.clone()]);
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
            &root_id_string,
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

    let root = Entry::new_top_level("{\"name\":\"root\"}".to_string());
    let root_id = root.id();

    let mut entry_a = Entry::new(root_id.clone(), "{\"name\":\"A\"}".to_string());
    entry_a.set_parents(vec![root_id.clone()]);
    let id_a = entry_a.id();

    let mut entry_b = Entry::new(root_id.clone(), "{\"name\":\"B\"}".to_string());
    entry_b.set_parents(vec![id_a.clone()]);
    let id_b = entry_b.id();

    let mut entry_c = Entry::new(root_id.clone(), "{\"name\":\"C\"}".to_string());
    entry_c.set_parents(vec![id_b.clone()]);
    let id_c = entry_c.id();

    let mut entry_e = Entry::new(root_id.clone(), "{\"name\":\"E\"}".to_string());
    entry_e.set_parents(vec![root_id.clone()]);
    let id_e = entry_e.id();

    let mut entry_f = Entry::new(root_id.clone(), "{\"name\":\"F\"}".to_string());
    entry_f.set_parents(vec![id_e.clone()]);
    let id_f = entry_f.id();

    let mut entry_d = Entry::new(root_id.clone(), "{\"name\":\"D\"}".to_string());
    entry_d.set_parents(vec![id_c.clone(), id_f.clone()]);
    let id_d = entry_d.id();

    // Insert all entries
    backend.put(root.clone()).unwrap();
    backend.put(entry_a.clone()).unwrap();
    backend.put(entry_b.clone()).unwrap();
    backend.put(entry_c.clone()).unwrap();
    backend.put(entry_d.clone()).unwrap();
    backend.put(entry_e.clone()).unwrap();
    backend.put(entry_f.clone()).unwrap();

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
    let backend = InMemoryBackend::load_from_file(&path).unwrap();

    // Verify it's empty
    assert_eq!(backend.all_roots().unwrap().len(), 0);
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
    let root_entry = Entry::new_top_level("root_data".to_string());
    let root_id = root_entry.id();
    backend.put(root_entry).unwrap();

    // Child 1
    let mut child1 = Entry::new(root_id.clone(), "child1_data".to_string());
    child1.set_parents(vec![root_id.clone()]);
    let child1_id = child1.id();
    backend.put(child1).unwrap();

    // Child 2
    let mut child2 = Entry::new(root_id.clone(), "child2_data".to_string());
    child2.set_parents(vec![root_id.clone()]);
    let child2_id = child2.id();
    backend.put(child2).unwrap();

    // Grandchild (child of child1)
    let mut grandchild = Entry::new(root_id.clone(), "grandchild_data".to_string());
    grandchild.set_parents(vec![child1_id.clone()]);
    let grandchild_id = grandchild.id();
    backend.put(grandchild).unwrap();

    // Entry with subtree
    let mut entry_with_subtree = Entry::new(root_id.clone(), "entry_with_subtree_data".to_string());
    entry_with_subtree.set_parents(vec![child2_id.clone()]);
    entry_with_subtree
        .set_subtree_data("subtree1".to_string(), "subtree_data".to_string())
        .unwrap();
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

    // Should have 2 tips (grandchild and entry_with_subtree)
    assert_eq!(loaded_tips.len(), 2);
    assert!(loaded_tips.contains(&grandchild_id));
    assert!(loaded_tips.contains(&entry_with_subtree_id));

    // Cleanup
    fs::remove_file(file_path).unwrap();
}

#[test]
fn test_sort_entries() {
    let mut backend = InMemoryBackend::new();

    // Create a simple tree with mixed order
    let root = Entry::new_top_level("{}".to_string());
    let root_id = root.id();

    let mut entry_a = Entry::new(root_id.clone(), "{}".to_string());
    entry_a.set_parents(vec![root_id.clone()]);
    let id_a = entry_a.id();

    let mut entry_b = Entry::new(root_id.clone(), "{}".to_string());
    entry_b.set_parents(vec![id_a.clone()]);
    let id_b = entry_b.id();

    let mut entry_c = Entry::new(root_id.clone(), "{}".to_string());
    entry_c.set_parents(vec![id_b.clone()]);

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
    let entry1 = Entry::new_top_level("{\"key\":\"value1\"}".to_string());
    let entry2 = Entry::new_top_level("{\"key\":\"value2\"}".to_string());

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
    let root1 = Entry::new_top_level("root1 data".to_string());
    let root1_id = root1.id();
    backend.put(root1).unwrap();

    let root2 = Entry::new_top_level("root2 data".to_string());
    let root2_id = root2.id();
    backend.put(root2).unwrap();

    // Test with two roots
    let roots = backend.all_roots().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));

    // Add a child under root1
    let mut child = Entry::new(root1_id.clone(), "child data".to_string());
    child.set_parents(vec![root1_id.clone()]);
    backend.put(child).unwrap();

    // Should still have only the two roots
    let roots = backend.all_roots().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));
}

#[test]
fn test_put_get_entry() {
    let mut backend = InMemoryBackend::new();

    // Test putting and getting an entry
    let entry = Entry::new_top_level("test data".to_string());
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
fn test_get_tips() {
    let mut backend = InMemoryBackend::new();

    // Create a simple tree structure:
    // Root -> A -> B
    //    \-> C

    let root = Entry::new_top_level("root".to_string());
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // Initially, root is the only tip
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], root_id);

    // Add child A
    let mut entry_a = Entry::new(root_id.clone(), "A".to_string());
    entry_a.set_parents(vec![root_id.clone()]);
    let id_a = entry_a.id();
    backend.put(entry_a.clone()).unwrap();

    // Now A should be the only tip
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_a);

    // Add child B from A
    let mut entry_b = Entry::new(root_id.clone(), "B".to_string());
    entry_b.set_parents(vec![id_a.clone()]);
    let id_b = entry_b.id();
    backend.put(entry_b.clone()).unwrap();

    // Now B should be the only tip from that branch
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_b);

    // Add child C directly from Root (creates a branch)
    let mut entry_c = Entry::new(root_id.clone(), "C".to_string());
    entry_c.set_parents(vec![root_id.clone()]);
    let id_c = entry_c.id();
    backend.put(entry_c.clone()).unwrap();

    // Now should have 2 tips: B and C
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 2);
    assert!(tips.contains(&id_b));
    assert!(tips.contains(&id_c));
}

#[test]
fn test_get_subtree_tips() {
    let mut backend = InMemoryBackend::new();

    // Create a tree with subtrees
    let root = Entry::new_top_level("root data".to_string());
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // Add entry A with subtree "sub1"
    let mut entry_a = Entry::new(root_id.clone(), "A".to_string());
    entry_a.set_parents(vec![root_id.clone()]);
    entry_a
        .set_subtree_data(String::from("sub1"), "A sub1 data".to_string())
        .unwrap();
    let id_a = entry_a.id();
    backend.put(entry_a).unwrap();

    // Initially, A is the only tip in subtree "sub1"
    let sub1_tips = backend.get_subtree_tips(&root_id, "sub1").unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_a);

    // Add entry B with subtree "sub1" as child of A
    let mut entry_b = Entry::new(root_id.clone(), "B".to_string());
    entry_b.set_parents(vec![id_a.clone()]);
    entry_b
        .set_subtree_data(String::from("sub1"), "B sub1 data".to_string())
        .unwrap();
    entry_b.set_subtree_parents("sub1", vec![id_a.clone()]);
    let id_b = entry_b.id();
    backend.put(entry_b).unwrap();

    // Now B is the only tip in subtree "sub1"
    let sub1_tips = backend.get_subtree_tips(&root_id, "sub1").unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Add entry C with subtree "sub2" (different subtree)
    let mut entry_c = Entry::new(root_id.clone(), "C".to_string());
    entry_c.set_parents(vec![root_id.clone()]);
    entry_c
        .set_subtree_data(String::from("sub2"), "C sub2 data".to_string())
        .unwrap();
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
    let mut entry_d = Entry::new(root_id.clone(), "D".to_string());
    entry_d.set_parents(vec![id_b.clone(), id_c.clone()]);
    entry_d
        .set_subtree_data(String::from("sub1"), "D sub1 data".to_string())
        .unwrap();
    entry_d.set_subtree_parents("sub1", vec![id_b.clone()]);
    entry_d
        .set_subtree_data(String::from("sub2"), "D sub2 data".to_string())
        .unwrap();
    entry_d.set_subtree_parents("sub2", vec![id_c.clone()]);
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
    let root = Entry::new_top_level("root".to_string());
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    let mut child = Entry::new(root_id.clone(), "child".to_string());
    child.set_parents(vec![root_id.clone()]);
    let child_id = child.id();
    backend.put(child.clone()).unwrap();

    let mut grandchild = Entry::new(root_id.clone(), "grandchild".to_string());
    grandchild.set_parents(vec![child_id.clone()]);
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
    let root = Entry::new_top_level("root".to_string());
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // Create children with and without subtree data

    // Child 1 - with subtree
    let mut child1 = Entry::new(root_id.clone(), "child1".to_string());
    child1.set_parents(vec![root_id.clone()]);
    child1
        .set_subtree_data(String::from("subtree1"), "child1 data".to_string())
        .unwrap();
    let child1_id = child1.id();
    backend.put(child1.clone()).unwrap();

    // Child 2 - without subtree
    let mut child2 = Entry::new(root_id.clone(), "child2".to_string());
    child2.set_parents(vec![root_id.clone()]);
    let child2_id = child2.id();
    backend.put(child2.clone()).unwrap();

    // Grandchild 1 - with subtree, child of child1
    let mut grandchild1 = Entry::new(root_id.clone(), "grandchild1".to_string());
    grandchild1.set_parents(vec![child1_id.clone()]);
    grandchild1
        .set_subtree_data(String::from("subtree1"), "grandchild1 data".to_string())
        .unwrap();
    grandchild1.set_subtree_parents("subtree1", vec![child1_id.clone()]);
    let gc1_id = grandchild1.id();
    backend.put(grandchild1.clone()).unwrap();

    // Grandchild 2 - with subtree, but from different parent (child2)
    let mut grandchild2 = Entry::new(root_id.clone(), "grandchild2".to_string());
    grandchild2.set_parents(vec![child2_id.clone()]);
    grandchild2
        .set_subtree_data(String::from("subtree1"), "grandchild2 data".to_string())
        .unwrap();
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
    assert!(entry_ids.contains(&grandchild2.id()));

    // Child2 shouldn't be in the subtree (no subtree data)
    assert!(!entry_ids.contains(&child2_id));
}

#[test]
fn test_calculate_subtree_height() {
    let mut backend = InMemoryBackend::new();

    // Create a tree with a subtree that has a different structure
    let root = Entry::new_top_level("root".to_string());
    let root_id = root.id();
    backend.put(root.clone()).unwrap();

    // A
    let mut entry_a = Entry::new(root_id.clone(), "A".to_string());
    entry_a.set_parents(vec![root_id.clone()]);
    entry_a
        .set_subtree_data(String::from("sub1"), "A_sub1".to_string())
        .unwrap();
    let id_a = entry_a.id();
    backend.put(entry_a.clone()).unwrap();

    // B (after A in main tree)
    let mut entry_b = Entry::new(root_id.clone(), "B".to_string());
    entry_b.set_parents(vec![id_a.clone()]);
    entry_b
        .set_subtree_data(String::from("sub1"), "B_sub1".to_string())
        .unwrap();
    // B is directly under root in subtree (not under A)
    // So we don't set subtree parents
    let id_b = entry_b.id();
    backend.put(entry_b.clone()).unwrap();

    // C (after B in main tree)
    let mut entry_c = Entry::new(root_id.clone(), "C".to_string());
    entry_c.set_parents(vec![id_b.clone()]);
    entry_c
        .set_subtree_data(String::from("sub1"), "C_sub1".to_string())
        .unwrap();
    // In subtree, C is a child of both A and B
    entry_c.set_subtree_parents("sub1", vec![id_a.clone(), id_b.clone()]);
    let id_c = entry_c.id();
    backend.put(entry_c.clone()).unwrap();

    // Calculate heights for main tree
    let main_heights = backend.calculate_heights(&root_id, None).unwrap();

    // Main tree: root -> A -> B -> C
    assert_eq!(main_heights.get(&root_id).unwrap(), &0);
    assert_eq!(main_heights.get(&id_a).unwrap(), &1);
    assert_eq!(main_heights.get(&id_b).unwrap(), &2);
    assert_eq!(main_heights.get(&id_c).unwrap(), &3);

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
