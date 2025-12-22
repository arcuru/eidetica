use eidetica::entry::{Entry, ID};

use super::helpers::test_backend;

#[tokio::test]
async fn test_backend_subtree_operations() {
    let backend = test_backend().await;

    // Create a root entry with a subtree
    let root_entry = Entry::root_builder()
        .set_subtree_data("subtree1", "root_subtree1_data")
        .build()
        .expect("Entry should build successfully");
    let root_id = root_entry.id();
    backend.put_verified(root_entry).await.unwrap();

    // Create child entry with subtree
    let child_entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("subtree1", "child_subtree1_data")
        .add_subtree_parent("subtree1", root_id.clone())
        .build()
        .expect("Entry should build successfully");
    let child_id = child_entry.id();
    backend.put_verified(child_entry).await.unwrap();

    // Test get_store_tips
    let subtree_tips_result = backend.get_store_tips(&root_id, "subtree1").await;
    assert!(subtree_tips_result.is_ok());
    let subtree_tips = subtree_tips_result.unwrap();
    assert_eq!(subtree_tips.len(), 1);
    assert_eq!(subtree_tips[0], child_id);

    // Test get_subtree
    let subtree_result = backend.get_store(&root_id, "subtree1").await;
    assert!(subtree_result.is_ok());
    let subtree = subtree_result.unwrap();
    assert_eq!(subtree.len(), 2); // root + child
}

#[tokio::test]
async fn test_backend_get_store_from_tips() {
    let backend = test_backend().await;
    let subtree_name = "my_subtree";

    // Create entries: root -> e1 -> e2a, e2b
    // root: has subtree
    // e1: no subtree
    // e2a: has subtree
    // e2b: has subtree

    let entry_root = Entry::root_builder()
        .set_subtree_data(subtree_name, "root_sub_data")
        .build()
        .expect("Entry should build successfully");
    let root_entry_id = entry_root.id();
    backend.put_verified(entry_root).await.unwrap();

    let e1 = Entry::builder(root_entry_id.clone())
        .add_parent(root_entry_id.clone())
        .build()
        .expect("Entry should build successfully");
    let e1_id = e1.id();
    backend.put_verified(e1).await.unwrap();

    let e2a = Entry::builder(root_entry_id.clone())
        .add_parent(e1_id.clone())
        .set_subtree_data(subtree_name, "e2a_sub_data")
        .add_subtree_parent(subtree_name, root_entry_id.clone())
        .build()
        .expect("Entry should build successfully");
    let e2a_id = e2a.id();
    backend.put_verified(e2a).await.unwrap();

    let e2b = Entry::builder(root_entry_id.clone())
        .add_parent(e1_id.clone())
        .set_subtree_data(subtree_name, "e2b_sub_data")
        .add_subtree_parent(subtree_name, root_entry_id.clone())
        .build()
        .expect("Entry should build successfully");
    let e2b_id = e2b.id();
    backend.put_verified(e2b).await.unwrap();

    // --- Test with single tip e2a ---
    let subtree_e2a = backend
        .get_store_from_tips(&root_entry_id, subtree_name, std::slice::from_ref(&e2a_id))
        .await
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
        .get_store_from_tips(
            &root_entry_id,
            subtree_name,
            &[e2a_id.clone(), e2b_id.clone()],
        )
        .await
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
    // When given a tip that exists but doesn't have the specified store,
    // the result should be empty.
    let subtree_bad_name = backend
        .get_store_from_tips(&root_entry_id, "bad_name", std::slice::from_ref(&e2a_id))
        .await
        .expect("Getting subtree with bad name should succeed");
    assert!(
        subtree_bad_name.is_empty(),
        "Getting subtree with non-existent store name should return empty vector"
    );

    // --- Test with non-existent tip ---
    let subtree_bad_tip = backend
        .get_store_from_tips(&root_entry_id, subtree_name, &["bad_tip_id".into()])
        .await
        .expect("Failed to get subtree with non-existent tip");
    assert!(
        subtree_bad_tip.is_empty(),
        "Getting subtree from non-existent tip should return empty list"
    );

    // --- Test with non-existent tree root ---
    // When given a valid tip but an invalid root (tree_id doesn't match),
    // the result should be empty because the tip doesn't belong to the specified tree.
    let bad_root_id_2: ID = "bad_root".into();
    let subtree_bad_root = backend
        .get_store_from_tips(&bad_root_id_2, subtree_name, std::slice::from_ref(&e1_id))
        .await
        .expect("Failed to get subtree with non-existent root");
    assert!(
        subtree_bad_root.is_empty(),
        "Getting subtree from tip with mismatched root should return empty vector"
    );

    // --- Test get_subtree() convenience function ---
    // This function should get the full subtree from current tips
    let full_subtree = backend
        .get_store(&root_entry_id, subtree_name)
        .await
        .expect("Failed to get full subtree");
    assert_eq!(
        full_subtree.len(),
        3,
        "Full subtree should have root, e2a, e2b"
    );
    let full_subtree_ids: Vec<_> = full_subtree.iter().map(|e| e.id()).collect();
    assert!(full_subtree_ids.contains(&root_entry_id));
    assert!(!full_subtree_ids.contains(&e1_id)); // e1 doesn't have the subtree
    assert!(full_subtree_ids.contains(&e2a_id));
    assert!(full_subtree_ids.contains(&e2b_id));
}

#[tokio::test]
async fn test_get_store_tips() {
    let backend = test_backend().await;

    // Create a tree with subtrees
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            root.clone(),
        )
        .await
        .unwrap();

    // Add entry A with subtree "sub1"
    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub1", "A sub1 data")
        .build()
        .expect("Entry should build successfully");
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    // Initially, A is the only tip in subtree "sub1"
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_a);

    // Add entry B with subtree "sub1" as child of A
    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data("sub1", "B sub1 data")
        .add_subtree_parent("sub1", id_a.clone())
        .build()
        .expect("Entry should build successfully");
    let id_b = entry_b.id();
    backend.put_verified(entry_b).await.unwrap();

    // Now B is the only tip in subtree "sub1"
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Add entry C with subtree "sub2" (different subtree)
    let entry_c = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub2", "C sub2 data")
        .build()
        .expect("Entry should build successfully");
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // Check tips for subtree "sub1" (should still be just B)
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Check tips for subtree "sub2" (should be just C)
    let sub2_tips = backend.get_store_tips(&root_id, "sub2").await.unwrap();
    assert_eq!(sub2_tips.len(), 1);
    assert_eq!(sub2_tips[0], id_c);

    // Add entry D with both subtrees "sub1" and "sub2"
    let entry_d = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .add_parent(id_c.clone())
        .set_subtree_data("sub1", "D sub1 data")
        .add_subtree_parent("sub1", id_b.clone())
        .set_subtree_data("sub2", "D sub2 data")
        .add_subtree_parent("sub2", id_c.clone())
        .build()
        .expect("Entry should build successfully");
    let id_d = entry_d.id();
    backend.put_verified(entry_d).await.unwrap();

    // Now D should be the tip for both subtrees
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_d);

    let sub2_tips = backend.get_store_tips(&root_id, "sub2").await.unwrap();
    assert_eq!(sub2_tips.len(), 1);
    assert_eq!(sub2_tips[0], id_d);
}
