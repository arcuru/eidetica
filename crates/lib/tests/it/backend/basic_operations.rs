use super::helpers::*;
use eidetica::backend::{BackendDB, database::InMemory};
use eidetica::entry::{Entry, ID};

#[test]
fn test_in_memory_backend_basic_operations() {
    let (backend, root_id) = create_test_backend_with_root();

    // Get the entry back
    let get_result = backend.get(&root_id);
    assert!(get_result.is_ok());
    let retrieved_entry = get_result.unwrap();
    assert_eq!(retrieved_entry.id(), root_id);

    // Check all_roots
    let roots_result = backend.all_roots();
    assert!(roots_result.is_ok());
    let roots = roots_result.unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], root_id);
}

#[test]
fn test_in_memory_backend_error_handling() {
    let backend = create_test_backend();

    // Test retrieving a non-existent entry
    let non_existent_id: ID = "non_existent_id".into();
    let get_result = backend.get(&non_existent_id);
    assert!(get_result.is_err());

    // For some database implementations like InMemory, get_tips might return
    // an empty vector instead of an error when the tree doesn't exist
    // Let's verify it returns either an error or an empty vector
    // FIXME: Code smell, databases should be consistent. Update this test once the API is defined.
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
fn test_all_roots() {
    let backend = InMemory::new();

    // Initially, there should be no roots
    assert!(backend.all_roots().unwrap().is_empty());

    // Add a simple top-level entry (a root)
    let root1 = Entry::root_builder().build();
    let root1_id = root1.id();
    backend.put_verified(root1).unwrap();

    let root2 = Entry::root_builder().build();
    let root2_id = root2.id();
    backend.put_verified(root2).unwrap();

    // Test with two roots
    let roots = backend.all_roots().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));

    // Add a child under root1
    let child = Entry::builder(root1_id.clone())
        .add_parent(root1_id.clone())
        .build();
    backend.put_verified(child).unwrap();

    // Should still have only the two roots
    let roots = backend.all_roots().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));
}
