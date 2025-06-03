use eidetica::Error;
use eidetica::backend::Backend;
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
use eidetica::constants::SETTINGS;
use eidetica::subtree::KVStore;

#[test]
fn test_new_db_and_tree() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    let tree_result = db.new_tree_default();
    match tree_result {
        Ok(_) => println!("Tree creation succeeded"),
        Err(e) => {
            println!("Tree creation failed with error: {e:?}");
            panic!("Tree creation failed: {e:?}");
        }
    }
}

#[test]
fn test_load_tree() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    let tree = db.new_tree_default().expect("Failed to create tree");
    let root_id = tree.root_id().clone();

    // Drop the original tree instance
    drop(tree);

    // Create a new DB instance with the same backend (or reuse db)
    let loaded_tree_result = db.load_tree(&root_id);
    assert!(loaded_tree_result.is_ok());
    let loaded_tree = loaded_tree_result.unwrap();
    assert_eq!(loaded_tree.root_id(), &root_id);
}

#[test]
fn test_all_trees() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let tree1 = db.new_tree_default().expect("Failed to create tree 1");
    let root_id1 = tree1.root_id().clone();

    let tree2 = db.new_tree_default().expect("Failed to create tree 2");
    let root_id2 = tree2.root_id().clone();

    // Set the tree name through the Tree API
    let op = tree2.new_operation().expect("Failed to start operation");
    {
        let settings = op
            .get_subtree::<KVStore>(SETTINGS)
            .expect("Failed to get settings subtree");
        settings
            .set("name", "Tree2")
            .expect("Failed to set tree name");
    }
    op.commit().expect("Failed to commit");

    let trees: Vec<eidetica::Tree> = db.all_trees().expect("Failed to get all trees");
    assert_eq!(trees.len(), 2);

    let found_ids: Vec<String> = trees.iter().map(|t| t.root_id().clone()).collect();
    assert!(found_ids.contains(&root_id1));
    assert!(found_ids.contains(&root_id2));
}

#[test]
fn test_get_backend() {
    let backend: Box<dyn Backend> = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let retrieved_backend = db.backend();
    assert!(retrieved_backend.lock().unwrap().all_roots().is_ok());
}

#[test]
fn test_create_tree_with_initial_settings() {
    let backend: Box<dyn Backend> = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create an empty tree first
    let tree = db.new_tree_default().expect("Failed to create tree");

    // Then set the settings through operations
    let op = tree.new_operation().expect("Failed to start operation");
    {
        let settings = op
            .get_subtree::<KVStore>(SETTINGS)
            .expect("Failed to get settings subtree");

        settings
            .set("name", "My Settings Tree")
            .expect("Failed to set name setting");
        settings
            .set("version", "1.0")
            .expect("Failed to set version setting");
    }
    op.commit().expect("Failed to commit settings");

    let settings_viewer = tree
        .get_subtree_viewer::<KVStore>(SETTINGS)
        .expect("Failed to get settings viewer");

    assert_eq!(
        settings_viewer
            .get_string("name")
            .expect("Failed to get name setting"),
        "My Settings Tree"
    );
    assert_eq!(
        settings_viewer
            .get_string("version")
            .expect("Failed to get version setting"),
        "1.0"
    );

    assert_eq!(
        tree.get_name().expect("Failed to get tree name"),
        "My Settings Tree"
    );
}

#[test]
fn test_basic_subtree_modification() {
    let backend: Box<dyn Backend> = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    let tree = db.new_tree_default().expect("Failed to create tree");

    let op = tree.new_operation().expect("Failed to start operation");
    {
        let data_store = op
            .get_subtree::<KVStore>("user_data")
            .expect("Failed to get data subtree");

        data_store
            .set("user_id", "alice")
            .expect("Failed to set user_id");
        data_store
            .set("email", "alice@example.com")
            .expect("Failed to set email");
    }

    let commit_result = op.commit();
    assert!(
        commit_result.is_ok(),
        "Commit failed: {:?}",
        commit_result.err()
    );
    let new_tip_id = commit_result.unwrap();
    assert_ne!(
        new_tip_id,
        *tree.root_id(),
        "Commit should create a new tip"
    );

    let data_viewer = tree
        .get_subtree_viewer::<KVStore>("user_data")
        .expect("Failed to get data viewer after commit");

    assert_eq!(
        data_viewer
            .get_string("user_id")
            .expect("Failed to get user_id after commit"),
        "alice"
    );
    assert_eq!(
        data_viewer
            .get_string("email")
            .expect("Failed to get email after commit"),
        "alice@example.com"
    );

    match data_viewer.get("non_existent_key") {
        Err(eidetica::Error::NotFound) => (),
        other => panic!("Expected NotFound error, got {other:?}"),
    }
}

#[test]
fn test_find_tree() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Tree 1: No name
    db.new_tree_default().expect("Failed to create tree 1");

    // Tree 2: Name "Tree2"
    let tree2 = db.new_tree_default().expect("Failed to create tree 2");
    let op2 = tree2.new_operation().expect("Failed to start operation");
    {
        let settings = op2
            .get_subtree::<KVStore>(SETTINGS)
            .expect("Failed to get settings subtree");
        settings
            .set("name", "Tree2")
            .expect("Failed to set tree name");
    }
    op2.commit().expect("Failed to commit");

    // Tree 3: Name "Tree3"
    let tree3 = db.new_tree_default().expect("Failed to create tree 3");
    let op3 = tree3.new_operation().expect("Failed to start operation");
    {
        let settings = op3
            .get_subtree::<KVStore>(SETTINGS)
            .expect("Failed to get settings subtree");
        settings
            .set("name", "Tree3")
            .expect("Failed to set tree name");
    }
    op3.commit().expect("Failed to commit");

    // Tree 4: Name "Tree3" (duplicate name)
    let tree4 = db.new_tree_default().expect("Failed to create tree 4");
    let op4 = tree4.new_operation().expect("Failed to start operation");
    {
        let settings = op4
            .get_subtree::<KVStore>(SETTINGS)
            .expect("Failed to get settings subtree");
        settings
            .set("name", "Tree3")
            .expect("Failed to set tree name");
    }
    op4.commit().expect("Failed to commit");

    // Test: Find non-existent name
    let found_none_result = db.find_tree("NonExistent");
    assert!(matches!(found_none_result, Err(Error::NotFound)));

    // Test: Find unique name
    let found_tree2 = db.find_tree("Tree2").expect("find_tree failed");
    assert_eq!(found_tree2.len(), 1);
    assert_eq!(found_tree2[0].get_name().unwrap(), "Tree2");

    // Test: Find duplicate name
    let found_tree3 = db.find_tree("Tree3").expect("find_tree failed");
    assert_eq!(found_tree3.len(), 2);
    // Check if both found trees have the name "Tree3"
    assert!(found_tree3.iter().all(|t| t.get_name().unwrap() == "Tree3"));

    // Test: Find when no trees exist
    let empty_backend = Box::new(InMemoryBackend::new());
    let empty_db = BaseDB::new(empty_backend);
    let found_empty_result = empty_db.find_tree("AnyName");
    assert!(matches!(found_empty_result, Err(Error::NotFound)));
}
