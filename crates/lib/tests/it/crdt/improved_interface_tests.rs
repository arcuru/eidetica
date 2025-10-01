//! Tests for the improved CRDT interface with better type inference
//!
//! This module demonstrates and tests the new ergonomic interface additions:
//! - TryFrom implementations for Value
//! - Generic get_as<T>() methods
//! - Convenience methods for common patterns (extract, try_extract, etc.)

use eidetica::{
    Result,
    crdt::{
        Doc,
        doc::{Value, path},
    },
};

#[test]
fn test_tryfrom_value_implementations() {
    let text_val = Value::Text("hello".to_string());
    let int_val = Value::Int(42);
    let bool_val = Value::Bool(true);
    let deleted_val = Value::Deleted;

    // Test successful conversions
    let text_result: Result<String> = (&text_val).try_into().map_err(Into::into);
    assert_eq!(text_result.unwrap(), "hello");

    let int_result: Result<i64> = (&int_val).try_into().map_err(Into::into);
    assert_eq!(int_result.unwrap(), 42);

    let bool_result: Result<bool> = (&bool_val).try_into().map_err(Into::into);
    assert!(bool_result.unwrap());

    // Test failed conversions
    let wrong_type: Result<i64> = (&text_val).try_into().map_err(Into::into);
    assert!(wrong_type.is_err());
    // Note: After conversion to eidetica::Error, we can't use is_type_error() directly

    let from_deleted: Result<String> = (&deleted_val).try_into().map_err(Into::into);
    assert!(from_deleted.is_err());
    // Note: After conversion to eidetica::Error, we can't use is_type_error() directly
}

#[test]
fn test_doc_get_as_method() {
    let mut doc = Doc::new();
    doc.set("name", "Alice");
    doc.set("age", 30);
    doc.set("active", true);
    doc.set("score", 98.5f64 as i64); // Convert to i64 for CRDT

    // Test successful type inference
    let name: Option<String> = doc.get_as("name");
    assert_eq!(name.unwrap(), "Alice");

    let age: Option<i64> = doc.get_as("age");
    assert_eq!(age.unwrap(), 30);

    let active: Option<bool> = doc.get_as("active");
    assert!(active.unwrap());

    // Test missing key
    let missing: Option<String> = doc.get_as("missing");
    assert!(missing.is_none());

    // Test type mismatch
    let wrong_type: Option<i64> = doc.get_as("name");
    assert!(wrong_type.is_none());
}

#[test]
fn test_node_get_as_method() {
    let mut node = Doc::new();
    node.set("name", "Bob");
    node.set("count", 100);
    node.set("enabled", false);

    // Test successful type inference
    let name = node.get_as::<String>("name");
    assert_eq!(name.unwrap(), "Bob");

    let count = node.get_as::<i64>("count");
    assert_eq!(count.unwrap(), 100);

    let enabled = node.get_as::<bool>("enabled");
    assert!(!enabled.unwrap());
}

#[test]
fn test_mixed_path_and_direct_access() {
    let mut doc = Doc::new();

    // Mix path and direct setting
    doc.set("top_level", "root_value");
    doc.set_path(path!("user.profile.name"), "Charlie").unwrap();
    doc.set_path(path!("user.profile.age"), 25).unwrap();
    doc.set("user_count", 42); // Direct set at root level
    doc.set_path(path!("user.settings.notifications"), true)
        .unwrap();

    // Access top-level values with both methods
    let root_direct: Option<String> = doc.get_as("top_level");
    assert_eq!(root_direct.unwrap(), "root_value");

    let root_via_path: Option<String> = doc.get_as(path!("top_level"));
    assert_eq!(root_via_path.unwrap(), "root_value");

    // Access nested values set via path using direct access to intermediate nodes
    let user_node: Option<Doc> = doc.get_as("user");
    let user = user_node.unwrap();
    let profile_node = user.get_as::<Doc>("profile");
    let profile = profile_node.unwrap();
    let profile_name = profile.get_as::<String>("name");
    assert_eq!(profile_name.unwrap(), "Charlie");

    // Access nested values using path methods
    let name_via_path: Option<String> = doc.get_as(path!("user.profile.name"));
    assert_eq!(name_via_path.unwrap(), "Charlie");

    let age_via_path: Option<i64> = doc.get_as(path!("user.profile.age"));
    assert_eq!(age_via_path.unwrap(), 25);

    // Mix modification methods - modify via path, then access directly
    doc.modify::<i64, _>(path!("user.profile.age"), |age| {
        *age += 1;
    })
    .unwrap();

    // Verify change via both access methods
    assert_eq!(doc.get_as::<i64>(path!("user.profile.age")).unwrap(), 26);
    let user_again: Option<Doc> = doc.get_as("user");
    let profile_age = user_again.unwrap().get_as::<i64>(path!("profile.age"));
    assert_eq!(profile_age.unwrap(), 26);

    // Set nested value directly on retrieved node, then access via path
    let user_mut = doc.get_mut("user").unwrap().as_doc_mut().unwrap();
    user_mut
        .set_path(path!("profile.email"), "charlie@example.com")
        .unwrap();

    // Access the newly set value via root path
    let email: Option<String> = doc.get_as(path!("user.profile.email"));
    assert_eq!(email.unwrap(), "charlie@example.com");

    // Test missing path
    let missing: Option<String> = doc.get_as(path!("user.missing.field"));
    assert!(missing.is_none());
}

#[test]
fn test_convenience_methods() {
    let mut doc = Doc::new();
    doc.set("name", "Dave");
    doc.set("level", 5);
    doc.set("premium", true);
    doc.set_path(path!("config.theme"), "dark").unwrap();

    // Test get_as with type inference (clean and safe)
    let name: String = doc.get_as("name").unwrap();
    assert_eq!(name, "Dave");

    let level: i64 = doc.get_as("level").unwrap();
    assert_eq!(level, 5);

    let premium: bool = doc.get_as("premium").unwrap();
    assert!(premium);

    // Test get_path_as method
    let theme: String = doc.get_as(path!("config.theme")).unwrap();
    assert_eq!(theme, "dark");

    // Test get_as for safe access (returns Option)
    let name_opt: Option<String> = doc.get_as("name");
    assert_eq!(name_opt, Some("Dave".to_string()));

    let missing_opt: Option<String> = doc.get_as("missing");
    assert_eq!(missing_opt, None);

    let wrong_type_opt: Option<i64> = doc.get_as("name"); // String as i64
    assert_eq!(wrong_type_opt, None);
}

#[test]
fn test_complex_nested_structures() -> eidetica::Result<()> {
    let mut doc = Doc::new();

    // Create nested structure
    doc.set_path(path!("app.users.123.name"), "Test User")
        .unwrap();
    doc.set_path(path!("app.users.123.permissions.read"), true)
        .unwrap();
    doc.set_path(path!("app.users.123.permissions.write"), false)
        .unwrap();
    doc.set_path(path!("app.config.max_users"), 1000).unwrap();

    // Test deep path access with type inference
    let username: String = doc.get_as(path!("app.users.123.name")).ok_or_else(|| {
        eidetica::Error::CRDT(eidetica::crdt::errors::CRDTError::ElementNotFound {
            key: "app.users.123.name".to_string(),
        })
    })?;
    assert_eq!(username, "Test User");

    let can_read: bool = doc
        .get_as(path!("app.users.123.permissions.read"))
        .ok_or_else(|| {
            eidetica::Error::CRDT(eidetica::crdt::errors::CRDTError::ElementNotFound {
                key: "app.users.123.permissions.read".to_string(),
            })
        })?;
    assert!(can_read);

    let can_write: bool = doc
        .get_as(path!("app.users.123.permissions.write"))
        .ok_or_else(|| {
            eidetica::Error::CRDT(eidetica::crdt::errors::CRDTError::ElementNotFound {
                key: "app.users.123.permissions.write".to_string(),
            })
        })?;
    assert!(!can_write);

    let max_users: i64 = doc.get_as(path!("app.config.max_users")).ok_or_else(|| {
        eidetica::Error::CRDT(eidetica::crdt::errors::CRDTError::ElementNotFound {
            key: "app.config.max_users".to_string(),
        })
    })?;
    assert_eq!(max_users, 1000);

    Ok(())
}

#[test]
fn test_interface_comparison() -> eidetica::Result<()> {
    let mut doc = Doc::new();
    doc.set("message", "Hello World");
    doc.set("count", 42);

    // Old verbose way
    let old_way = doc.get("message").and_then(|v| v.as_text());
    assert_eq!(old_way, Some("Hello World"));

    // Old specific getter way
    let old_specific = doc.get_as::<String>("message");
    assert_eq!(old_specific, Some("Hello World".to_string()));

    // New generic way
    let new_way: Option<String> = doc.get_as("message");
    let new_way_str = new_way.unwrap();
    assert_eq!(new_way_str, "Hello World");

    // New method way (most ergonomic) - use ok_or_else to convert Option to Result for ?
    let method_way: String = doc.get_as("message").ok_or_else(|| {
        eidetica::Error::CRDT(eidetica::crdt::errors::CRDTError::ElementNotFound {
            key: "message".to_string(),
        })
    })?;
    assert_eq!(method_way, "Hello World");

    // All should be equivalent but new ways are more ergonomic
    assert_eq!(old_way.unwrap(), new_way_str);
    assert_eq!(old_specific.unwrap(), method_way);

    Ok(())
}

#[test]
fn test_backwards_compatibility() {
    let mut doc = Doc::new();
    doc.set("text", "test");
    doc.set("number", 123);
    doc.set("flag", true);

    // All old methods should still work
    assert_eq!(doc.get_as::<String>("text"), Some("test".to_string()));
    assert_eq!(doc.get_as::<i64>("number"), Some(123));
    assert_eq!(doc.get_as::<bool>("flag"), Some(true));

    // New methods should work alongside old ones
    let text: String = doc.get_as("text").unwrap();
    let number: i64 = doc.get_as("number").unwrap();
    let flag: bool = doc.get_as("flag").unwrap();

    assert_eq!(text, "test");
    assert_eq!(number, 123);
    assert!(flag);

    // Old and new should give same results
    assert_eq!(doc.get_as::<String>("text").unwrap(), text);
    assert_eq!(doc.get_as::<i64>("number").unwrap(), number);
    assert_eq!(doc.get_as::<bool>("flag").unwrap(), flag);
}

#[test]
fn test_mutable_access_methods_mixed() {
    let mut doc = Doc::new();

    // Set up mixed structure - some direct, some path-based
    doc.set("counter", 0);
    doc.set_path(path!("stats.views"), 100).unwrap();
    doc.set_path(path!("stats.downloads"), 50).unwrap();
    doc.set("user_name", "alice");

    // Test get_or_insert on direct key
    let value1 = doc.get_or_insert("counter", 999);
    assert_eq!(*value1, Value::Int(0)); // Should keep existing value

    // Test get_or_insert on new key
    let value2 = doc.get_or_insert("new_field", "default");
    assert_eq!(*value2, Value::Text("default".to_string()));

    // Modify direct key, verify with both access methods
    doc.modify::<i64, _>("counter", |count| {
        *count += 5;
    })
    .unwrap();
    assert_eq!(doc.get_as::<i64>("counter").unwrap(), 5);
    assert_eq!(doc.get_as::<i64>(path!("counter")).unwrap(), 5);

    // Modify nested value via path, verify with direct node access
    doc.modify::<i64, _>(path!("stats.views"), |views| {
        *views *= 2;
    })
    .unwrap();

    // Verify via path access
    assert_eq!(doc.get_as::<i64>(path!("stats.views")).unwrap(), 200);

    // Verify via direct node access
    let stats_node: Option<Doc> = doc.get_as("stats");
    let views_direct = stats_node.unwrap().get_as::<i64>("views");
    assert_eq!(views_direct.unwrap(), 200);

    // Modify direct key, then use path to access it
    doc.modify::<String, _>("user_name", |name| {
        name.push_str("_modified");
    })
    .unwrap();
    assert_eq!(
        doc.get_as::<String>(path!("user_name")).unwrap(),
        "alice_modified"
    );

    // Set via direct access to retrieved node, verify via root path
    {
        let stats_mut = doc.get_mut("stats").unwrap().as_doc_mut().unwrap();
        stats_mut.set("likes", 75);
        stats_mut.set("rating", 95);
    }
    assert_eq!(doc.get_as::<i64>(path!("stats.likes")).unwrap(), 75);
    assert_eq!(doc.get_as::<i64>(path!("stats.rating")).unwrap(), 95);

    // Try to modify non-existent paths/keys
    let result1 = doc.modify::<i64, _>("missing", |_| {});
    assert!(result1.is_err());

    let result2 = doc.modify::<i64, _>(path!("stats.missing"), |_| {});
    assert!(result2.is_err());
}
