//! DocStore path access, mutation, and interoperability tests.

use eidetica::{
    crdt::{
        Doc,
        doc::{Value, path},
    },
    store::DocStore,
};

use crate::helpers::TestContext;

#[tokio::test]
async fn test_docstore_path_based_access() {
    let ctx = TestContext::new().with_database().await;

    // Create operation and set up nested data structure
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("path_test")
        .await
        .expect("Failed to get DocStore");

    // Set up mixed structure - some direct, some that would be path-accessible
    dict.set("top_level", "root_value")
        .await
        .expect("Failed to set top_level");
    dict.set("counter", 42)
        .await
        .expect("Failed to set counter");

    // Create nested structure by setting a Doc with nested data
    let mut user_doc = Doc::new();
    user_doc.set("name", "Alice");
    user_doc.set("age", 30);

    let mut profile_doc = Doc::new();
    profile_doc.set("email", "alice@example.com");
    profile_doc.set("verified", true);
    user_doc.set("profile", Value::Doc(profile_doc));

    dict.set("user", Value::Doc(user_doc))
        .await
        .expect("Failed to set user");

    // Test get_path() for various path levels

    // Top-level path access (equivalent to direct access)
    let top_value = dict
        .get_path(path!("top_level"))
        .await
        .expect("Failed to get top_level path");
    assert_eq!(top_value, Value::Text("root_value".to_string()));

    let counter_value = dict
        .get_path(path!("counter"))
        .await
        .expect("Failed to get counter path");
    assert_eq!(counter_value, Value::Int(42));

    // Nested path access
    let user_name = dict
        .get_path(path!("user.name"))
        .await
        .expect("Failed to get user.name path");
    assert_eq!(user_name, Value::Text("Alice".to_string()));

    let user_age = dict
        .get_path(path!("user.age"))
        .await
        .expect("Failed to get user.age path");
    assert_eq!(user_age, Value::Int(30));

    // Deep nested path access
    let user_email = dict
        .get_path(path!("user.profile.email"))
        .await
        .expect("Failed to get user.profile.email path");
    assert_eq!(user_email, Value::Text("alice@example.com".to_string()));

    let user_verified = dict
        .get_path(path!("user.profile.verified"))
        .await
        .expect("Failed to get user.profile.verified path");
    assert_eq!(user_verified, Value::Bool(true));

    // Test get_path_as() with type conversion

    // Direct type conversion
    let top_typed: String = dict
        .get_path_as(path!("top_level"))
        .await
        .expect("Failed to get typed top_level");
    assert_eq!(top_typed, "root_value");

    let counter_typed: i64 = dict
        .get_path_as(path!("counter"))
        .await
        .expect("Failed to get typed counter");
    assert_eq!(counter_typed, 42);

    // Nested type conversion
    let name_typed: String = dict
        .get_path_as(path!("user.name"))
        .await
        .expect("Failed to get typed user.name");
    assert_eq!(name_typed, "Alice");

    let age_typed: i64 = dict
        .get_path_as(path!("user.age"))
        .await
        .expect("Failed to get typed user.age");
    assert_eq!(age_typed, 30);

    // Deep nested type conversion
    let email_typed: String = dict
        .get_path_as(path!("user.profile.email"))
        .await
        .expect("Failed to get typed user.profile.email");
    assert_eq!(email_typed, "alice@example.com");

    let verified_typed: bool = dict
        .get_path_as(path!("user.profile.verified"))
        .await
        .expect("Failed to get typed user.profile.verified");
    assert!(verified_typed);

    // Test error cases

    // Non-existent top-level path
    let missing_result = dict.get_path(path!("missing_key")).await;
    assert!(missing_result.unwrap_err().is_not_found());

    // Non-existent nested path
    let missing_nested = dict.get_path(path!("user.missing")).await;
    assert!(missing_nested.unwrap_err().is_not_found());

    // Non-existent deep path
    let missing_deep = dict.get_path(path!("user.profile.missing")).await;
    assert!(missing_deep.unwrap_err().is_not_found());

    // Type mismatch with get_path_as
    let type_mismatch: Result<i64, _> = dict.get_path_as(path!("user.name")).await; // String as i64
    assert!(type_mismatch.unwrap_err().is_crdt_type_error());

    // Commit and verify persistence
    txn.commit().await.expect("Failed to commit transaction");

    // Test via viewer (read-only access)
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("path_test")
        .await
        .expect("Failed to get viewer");

    // Verify all path access still works after commit
    assert_eq!(
        viewer.get_path(path!("top_level")).await.unwrap(),
        Value::Text("root_value".to_string())
    );
    assert_eq!(
        viewer.get_path(path!("user.name")).await.unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        viewer.get_path(path!("user.profile.email")).await.unwrap(),
        Value::Text("alice@example.com".to_string())
    );

    // Verify typed access still works
    let persisted_name: String = viewer
        .get_path_as(path!("user.name"))
        .await
        .expect("Failed to get persisted name");
    assert_eq!(persisted_name, "Alice");
}

#[tokio::test]
async fn test_docstore_path_mixed_with_staging() {
    let ctx = TestContext::new().with_database().await;

    // Create initial committed data
    {
        let txn = ctx
            .database()
            .new_transaction()
            .await
            .expect("Failed to start transaction");
        let dict = txn
            .get_store::<DocStore>("staging_test")
            .await
            .expect("Failed to get DocStore");

        // Set some initial data
        let mut config_doc = Doc::new();
        config_doc.set("version", "1.0");
        config_doc.set("debug", false);
        dict.set("config", Value::Doc(config_doc))
            .await
            .expect("Failed to set config");

        txn.commit().await.expect("Failed to commit initial data");
    }

    // Now test staging behavior with paths
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("staging_test")
        .await
        .expect("Failed to get DocStore");

    // Verify we can access committed data via path
    let initial_version: String = dict
        .get_path_as(path!("config.version"))
        .await
        .expect("Failed to get initial version");
    assert_eq!(initial_version, "1.0");

    let initial_debug: bool = dict
        .get_path_as(path!("config.debug"))
        .await
        .expect("Failed to get initial debug");
    assert!(!initial_debug);

    // Stage some changes (update existing and add new) by updating the nested structure
    let mut updated_config = Doc::new();
    updated_config.set("version", "2.0"); // Update version
    updated_config.set("debug", false); // Keep debug same
    updated_config.set("environment", "production"); // Add new field
    dict.set("config", Value::Doc(updated_config))
        .await
        .expect("Failed to stage config update");
    dict.set("new_key", "new_value")
        .await
        .expect("Failed to stage new key");

    // Test that staged changes are visible via path access
    let staged_version: String = dict
        .get_path_as(path!("config.version"))
        .await
        .expect("Failed to get staged version");
    assert_eq!(staged_version, "2.0"); // Should see staged version

    let staged_env: String = dict
        .get_path_as(path!("config.environment"))
        .await
        .expect("Failed to get staged environment");
    assert_eq!(staged_env, "production");

    let new_value: String = dict
        .get_path_as(path!("new_key"))
        .await
        .expect("Failed to get new key");
    assert_eq!(new_value, "new_value");

    // Verify that committed data that wasn't changed is still accessible
    let unchanged_debug: bool = dict
        .get_path_as(path!("config.debug"))
        .await
        .expect("Failed to get unchanged debug");
    assert!(!unchanged_debug); // Should still be false

    // Test that viewer still sees old committed data
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("staging_test")
        .await
        .expect("Failed to get viewer");

    let viewer_version: String = viewer
        .get_path_as(path!("config.version"))
        .await
        .expect("Failed to get viewer version");
    assert_eq!(viewer_version, "1.0"); // Should see committed version

    // New staged data shouldn't be visible to viewer
    let viewer_env = viewer.get_path(path!("config.environment")).await;
    assert!(viewer_env.unwrap_err().is_not_found()); // Should not exist in committed data

    let viewer_new = viewer.get_path(path!("new_key")).await;
    assert!(viewer_new.unwrap_err().is_not_found()); // Should not exist in committed data
}

#[tokio::test]
async fn test_docstore_set_path() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("set_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test setting simple path (single level)
    dict.set_path(path!("simple"), "value")
        .await
        .expect("Failed to set simple path");
    assert_eq!(
        dict.get_path(path!("simple")).await.unwrap(),
        Value::Text("value".to_string())
    );

    // Test setting nested paths (creates intermediate structure)
    dict.set_path(path!("user.name"), "Alice")
        .await
        .expect("Failed to set user.name");
    dict.set_path(path!("user.age"), 30)
        .await
        .expect("Failed to set user.age");
    dict.set_path(path!("user.profile.email"), "alice@example.com")
        .await
        .expect("Failed to set user.profile.email");
    dict.set_path(path!("user.profile.verified"), true)
        .await
        .expect("Failed to set user.profile.verified");

    // Test deep nesting
    dict.set_path(path!("config.database.host"), "localhost")
        .await
        .expect("Failed to set deep path");
    dict.set_path(path!("config.database.port"), 5432)
        .await
        .expect("Failed to set deep path port");

    // Verify all values are accessible
    assert_eq!(
        dict.get_path(path!("user.name")).await.unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        dict.get_path(path!("user.age")).await.unwrap(),
        Value::Int(30)
    );
    assert_eq!(
        dict.get_path(path!("user.profile.email")).await.unwrap(),
        Value::Text("alice@example.com".to_string())
    );
    assert_eq!(
        dict.get_path(path!("user.profile.verified")).await.unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        dict.get_path(path!("config.database.host")).await.unwrap(),
        Value::Text("localhost".to_string())
    );
    assert_eq!(
        dict.get_path(path!("config.database.port")).await.unwrap(),
        Value::Int(5432)
    );

    // Test overwriting existing values
    dict.set_path(path!("user.age"), 31)
        .await
        .expect("Failed to overwrite user.age");
    assert_eq!(
        dict.get_path(path!("user.age")).await.unwrap(),
        Value::Int(31)
    );

    // Test overwriting path segments (should work as expected)
    dict.set_path(path!("user.profile"), "simple_string")
        .await
        .expect("Failed to overwrite user.profile");
    assert_eq!(
        dict.get_path(path!("user.profile")).await.unwrap(),
        Value::Text("simple_string".to_string())
    );

    // Verify that previous nested values under user.profile are now inaccessible
    let email_result = dict.get_path(path!("user.profile.email")).await;
    assert!(email_result.unwrap_err().is_not_found());

    // Commit and verify persistence
    txn.commit().await.expect("Failed to commit transaction");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("set_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer.get_path(path!("user.name")).await.unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        viewer.get_path(path!("user.age")).await.unwrap(),
        Value::Int(31)
    );
    assert_eq!(
        viewer
            .get_path(path!("config.database.host"))
            .await
            .unwrap(),
        Value::Text("localhost".to_string())
    );
}

#[tokio::test]
async fn test_docstore_modify_path() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("modify_path_test")
        .await
        .expect("Failed to get DocStore");

    // Set up initial data
    dict.set_path(path!("stats.score"), 100)
        .await
        .expect("Failed to set initial score");
    dict.set_path(path!("config.retries"), 3)
        .await
        .expect("Failed to set initial retries");
    dict.set_path(path!("user.name"), "Alice")
        .await
        .expect("Failed to set initial name");

    // Test modifying integer values
    dict.modify_path::<i64, _>(path!("stats.score"), |score| {
        *score += 50;
    })
    .await
    .expect("Failed to modify score");
    assert_eq!(
        dict.get_path_as::<i64>(path!("stats.score")).await.unwrap(),
        150
    );

    dict.modify_path::<i64, _>(path!("config.retries"), |retries| {
        *retries *= 2;
    })
    .await
    .expect("Failed to modify retries");
    assert_eq!(
        dict.get_path_as::<i64>(path!("config.retries"))
            .await
            .unwrap(),
        6
    );

    // Test modifying string values
    dict.modify_path::<String, _>(path!("user.name"), |name| {
        name.push_str(" Smith");
    })
    .await
    .expect("Failed to modify name");
    assert_eq!(
        dict.get_path_as::<String>(path!("user.name"))
            .await
            .unwrap(),
        "Alice Smith"
    );

    // Test error case - path doesn't exist
    let result = dict
        .modify_path::<i64, _>(path!("nonexistent.path"), |_| {})
        .await;
    assert!(result.unwrap_err().is_not_found());

    // Test error case - type mismatch
    let result = dict
        .modify_path::<i64, _>(path!("user.name"), |_| {}) // name is string, not int
        .await;
    assert!(result.unwrap_err().is_crdt_type_error());

    // Commit and verify persistence
    txn.commit().await.expect("Failed to commit transaction");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("modify_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("stats.score"))
            .await
            .unwrap(),
        150
    );
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("config.retries"))
            .await
            .unwrap(),
        6
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("user.name"))
            .await
            .unwrap(),
        "Alice Smith"
    );
}

#[tokio::test]
async fn test_docstore_get_or_insert_path() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("get_or_insert_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test inserting when path doesn't exist (creates structure)
    let score1: i64 = dict
        .get_or_insert_path(path!("player.stats.score"), 0)
        .await
        .expect("Failed to get_or_insert_path score");
    assert_eq!(score1, 0);

    // Verify structure was created
    assert_eq!(
        dict.get_path_as::<i64>(path!("player.stats.score"))
            .await
            .unwrap(),
        0
    );

    // Test returning existing value when path exists
    dict.set_path(path!("player.stats.score"), 42)
        .await
        .expect("Failed to set score");
    let score2: i64 = dict
        .get_or_insert_path(path!("player.stats.score"), 100)
        .await
        .expect("Failed to get_or_insert_path existing score");
    assert_eq!(score2, 42); // Should return existing value, not default

    // Test with different data types
    let name: String = dict
        .get_or_insert_path(path!("player.info.name"), "DefaultName".to_string())
        .await
        .expect("Failed to get_or_insert_path name");
    assert_eq!(name, "DefaultName");

    let active: bool = dict
        .get_or_insert_path(path!("player.status.active"), true)
        .await
        .expect("Failed to get_or_insert_path active");
    assert!(active);

    // Test existing values are returned
    dict.set_path(path!("player.info.name"), "Alice")
        .await
        .expect("Failed to set name");
    let existing_name: String = dict
        .get_or_insert_path(path!("player.info.name"), "ShouldNotBeUsed".to_string())
        .await
        .expect("Failed to get existing name");
    assert_eq!(existing_name, "Alice");

    // Verify all paths exist
    assert_eq!(
        dict.get_path_as::<i64>(path!("player.stats.score"))
            .await
            .unwrap(),
        42
    );
    assert_eq!(
        dict.get_path_as::<String>(path!("player.info.name"))
            .await
            .unwrap(),
        "Alice"
    );
    assert!(
        dict.get_path_as::<bool>(path!("player.status.active"))
            .await
            .unwrap()
    );

    // Commit and verify persistence
    txn.commit().await.expect("Failed to commit transaction");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("get_or_insert_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("player.stats.score"))
            .await
            .unwrap(),
        42
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("player.info.name"))
            .await
            .unwrap(),
        "Alice"
    );
    assert!(
        viewer
            .get_path_as::<bool>(path!("player.status.active"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn test_docstore_modify_or_insert_path() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("modify_or_insert_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test inserting and modifying when path doesn't exist
    dict.modify_or_insert_path::<i64, _>(path!("metrics.requests"), 0, |count| {
        *count += 10;
    })
    .await
    .expect("Failed to modify_or_insert_path requests");
    assert_eq!(
        dict.get_path_as::<i64>(path!("metrics.requests"))
            .await
            .unwrap(),
        10
    );

    // Test modifying existing value
    dict.modify_or_insert_path::<i64, _>(path!("metrics.requests"), 100, |count| {
        *count *= 2;
    })
    .await
    .expect("Failed to modify existing requests");
    assert_eq!(
        dict.get_path_as::<i64>(path!("metrics.requests"))
            .await
            .unwrap(),
        20
    ); // 10 * 2, not 100 * 2

    // Test with string values
    dict.modify_or_insert_path::<String, _>(
        path!("config.environment"),
        "dev".to_string(),
        |env| {
            env.push_str(".local");
        },
    )
    .await
    .expect("Failed to modify_or_insert_path environment");
    assert_eq!(
        dict.get_path_as::<String>(path!("config.environment"))
            .await
            .unwrap(),
        "dev.local"
    );

    // Test modifying existing string
    dict.modify_or_insert_path::<String, _>(
        path!("config.environment"),
        "prod".to_string(),
        |env| {
            *env = format!("override-{env}");
        },
    )
    .await
    .expect("Failed to modify existing environment");
    assert_eq!(
        dict.get_path_as::<String>(path!("config.environment"))
            .await
            .unwrap(),
        "override-dev.local"
    );

    // Test complex nested path creation and modification
    dict.modify_or_insert_path::<i64, _>(path!("app.features.cache.ttl"), 300, |ttl| {
        *ttl += 60; // Add 1 minute
    })
    .await
    .expect("Failed to modify_or_insert_path ttl");
    assert_eq!(
        dict.get_path_as::<i64>(path!("app.features.cache.ttl"))
            .await
            .unwrap(),
        360
    );

    // Test multiple operations on the same nested structure
    dict.modify_or_insert_path::<i64, _>(path!("app.features.cache.size"), 1024, |size| {
        *size *= 2;
    })
    .await
    .expect("Failed to modify_or_insert_path size");
    assert_eq!(
        dict.get_path_as::<i64>(path!("app.features.cache.size"))
            .await
            .unwrap(),
        2048
    );

    // Verify that existing structure is preserved
    assert_eq!(
        dict.get_path_as::<i64>(path!("app.features.cache.ttl"))
            .await
            .unwrap(),
        360
    );

    // Commit and verify persistence
    txn.commit().await.expect("Failed to commit transaction");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("modify_or_insert_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("metrics.requests"))
            .await
            .unwrap(),
        20
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("config.environment"))
            .await
            .unwrap(),
        "override-dev.local"
    );
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("app.features.cache.ttl"))
            .await
            .unwrap(),
        360
    );
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("app.features.cache.size"))
            .await
            .unwrap(),
        2048
    );
}

#[tokio::test]
async fn test_docstore_path_mutation_interoperability() {
    let ctx = TestContext::new().with_database().await;

    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("interop_test")
        .await
        .expect("Failed to get DocStore");

    // Mix direct and path-based operations
    dict.set("level1", "direct")
        .await
        .expect("Failed to set direct");

    // Setting a nested path when level1 is not a map should replace level1 with a node
    let result = dict.set_path(path!("level1.nested"), "path_based").await;
    result.expect("Nested path should replace the scalar with a node");

    // Verify that level1 is now a node containing "nested" = "path_based"
    assert_eq!(
        dict.get_string("level1.nested")
            .await
            .expect("Failed to get level1.nested"),
        "path_based"
    );

    // However, we can set level1 to be a map structure directly
    dict.set_path(path!("level1_map.nested"), "path_based")
        .await
        .expect("Failed to set nested"); // This creates level1_map as a map

    // Verify that level1_map is now a map
    let level1_value = dict
        .get("level1_map")
        .await
        .expect("Failed to get level1_map");
    match level1_value {
        Value::Doc(_) => {} // Expected
        _ => panic!("Expected level1_map to be a Doc after path operation"),
    }

    // Verify nested value is accessible
    assert_eq!(
        dict.get_path_as::<String>(path!("level1_map.nested"))
            .await
            .unwrap(),
        "path_based"
    );

    // Mix get_as and get_path_as operations
    dict.set_path(path!("data.count"), 42)
        .await
        .expect("Failed to set count");
    let direct_count_result: Result<i64, _> = dict.get_as("data").await; // Should fail - data is a map
    assert!(direct_count_result.unwrap_err().is_crdt_type_error());
    let path_count: i64 = dict.get_path_as(path!("data.count")).await.unwrap();
    assert_eq!(path_count, 42);

    // Mix modify and modify_path operations
    dict.set("simple_count", 10)
        .await
        .expect("Failed to set simple_count");
    dict.modify::<i64, _>("simple_count", |count| *count += 5)
        .await
        .expect("Failed to modify simple_count");
    assert_eq!(dict.get_as::<i64>("simple_count").await.unwrap(), 15);

    dict.modify_path::<i64, _>(path!("data.count"), |count| *count *= 2)
        .await
        .expect("Failed to modify path count");
    assert_eq!(
        dict.get_path_as::<i64>(path!("data.count")).await.unwrap(),
        84
    );

    // Mix get_or_insert and get_or_insert_path
    let simple_new: String = dict
        .get_or_insert("new_simple", "simple".to_string())
        .await
        .expect("Failed to get_or_insert simple");
    assert_eq!(simple_new, "simple");

    let path_new: String = dict
        .get_or_insert_path(path!("new_path.deep.value"), "deep".to_string())
        .await
        .expect("Failed to get_or_insert_path deep");
    assert_eq!(path_new, "deep");

    // Verify both exist with different access methods
    assert_eq!(dict.get_as::<String>("new_simple").await.unwrap(), "simple");
    assert_eq!(
        dict.get_path_as::<String>(path!("new_path.deep.value"))
            .await
            .unwrap(),
        "deep"
    );

    // Commit and verify all operations persisted correctly
    txn.commit().await.expect("Failed to commit transaction");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("interop_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<String>(path!("level1_map.nested"))
            .await
            .unwrap(),
        "path_based"
    );
    assert_eq!(viewer.get_as::<i64>("simple_count").await.unwrap(), 15);
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("data.count"))
            .await
            .unwrap(),
        84
    );
    assert_eq!(
        viewer.get_as::<String>("new_simple").await.unwrap(),
        "simple"
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("new_path.deep.value"))
            .await
            .unwrap(),
        "deep"
    );
}
