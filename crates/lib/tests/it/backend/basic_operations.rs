use eidetica::entry::{Entry, ID};

use super::helpers::{create_test_backend_with_root, test_backend};

#[tokio::test]
async fn test_backend_basic_operations() {
    let (backend, root_id) = create_test_backend_with_root().await;

    // Get the entry back
    let get_result = backend.get(&root_id).await;
    assert!(get_result.is_ok());
    let retrieved_entry = get_result.unwrap();
    assert_eq!(retrieved_entry.id(), root_id);

    // Check all_roots contains our root
    let roots = backend.all_roots().await.unwrap();
    assert!(
        roots.contains(&root_id),
        "all_roots should contain the root we added"
    );
}

#[tokio::test]
async fn test_backend_error_handling() {
    let backend = test_backend().await;

    // Test retrieving a non-existent entry
    let non_existent_id: ID = ID::from_bytes("non_existent_id");
    let get_result = backend.get(&non_existent_id).await;
    assert!(get_result.is_err());

    // get_tips for non-existent tree returns an empty vector
    let tips_result = backend.get_tips(&non_existent_id).await;
    assert!(
        tips_result.is_ok(),
        "get_tips should succeed for non-existent tree"
    );
    assert!(
        tips_result.unwrap().is_empty(),
        "get_tips should return empty vector for non-existent tree"
    );

    // get_store for non-existent tree returns an empty vector
    let subtree_result = backend
        .get_store(&non_existent_id, "non_existent_subtree")
        .await;
    assert!(
        subtree_result.is_ok(),
        "get_store should succeed for non-existent tree"
    );
    assert!(
        subtree_result.unwrap().is_empty(),
        "get_store should return empty vector for non-existent tree"
    );

    // get_store_tips for non-existent tree returns an empty vector
    let subtree_tips_result = backend
        .get_store_tips(&non_existent_id, "non_existent_subtree")
        .await;
    assert!(
        subtree_tips_result.is_ok(),
        "get_store_tips should succeed for non-existent tree"
    );
    assert!(
        subtree_tips_result.unwrap().is_empty(),
        "get_store_tips should return empty vector for non-existent tree"
    );
}

#[tokio::test]
async fn test_all_roots() {
    let backend = test_backend().await;

    // Record initial root count (may be non-zero for backends with pre-seeded data)
    let initial_roots = backend.all_roots().await.unwrap();
    let initial_count = initial_roots.len();

    // Add a simple top-level entry (a root)
    let root1 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root1_id = root1.id();
    backend.put_verified(root1).await.unwrap();

    let root2 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root2_id = root2.id();
    backend.put_verified(root2).await.unwrap();

    // Should have two more roots than initially
    let roots = backend.all_roots().await.unwrap();
    assert_eq!(roots.len(), initial_count + 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));

    // Add a child under root1
    let child = Entry::builder(root1_id.clone())
        .add_parent(root1_id.clone())
        .build()
        .expect("Child entry should build successfully");
    backend.put_verified(child).await.unwrap();

    // Adding a child should not change the root count
    let roots = backend.all_roots().await.unwrap();
    assert_eq!(roots.len(), initial_count + 2);
    assert!(roots.contains(&root1_id));
    assert!(roots.contains(&root2_id));
}
