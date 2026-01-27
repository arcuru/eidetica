//! Tests for Entry and EntryBuilder

use super::*;

#[test]
fn test_validate_root_entry_without_parents_succeeds() {
    // Root entries (with "_root" subtree) should be valid without parents
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    // Should pass validation
    assert!(
        entry.validate().is_ok(),
        "Root entry should be valid without parents"
    );
    assert!(entry.is_root(), "Entry should be identified as root");
}

#[test]
fn test_validate_root_entry_with_parents_fails() {
    // Root entries with parents should fail at build time
    let result = Entry::root_builder()
        .add_parent(ID::from_bytes("some_parent"))
        .build();
    assert!(
        result.is_err(),
        "Root entry with parents should fail to build"
    );
}

#[test]
fn test_validate_non_root_entry_without_parents_fails() {
    // Non-root entries MUST have parents to be valid
    let result = Entry::builder(ID::from_bytes("tree")).build();
    assert!(
        result.is_err(),
        "Non-root entry without parents should fail to build"
    );

    // Check that it's the correct error type
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(
        error_msg.contains("EntryValidationFailed"),
        "Should be EntryValidationFailed error, got: {error_msg}"
    );
    assert!(
        error_msg.contains("empty main tree parents"),
        "Error should mention empty parent requirement, got: {error_msg}"
    );
}

#[test]
fn test_validate_non_root_entry_with_parents_succeeds() {
    // Non-root entries with parents should be valid
    let entry = Entry::builder(ID::from_bytes("tree"))
        .add_parent(ID::from_bytes("parent"))
        .build()
        .expect("Entry with parent should build successfully");

    // Should pass validation
    assert!(
        entry.validate().is_ok(),
        "Non-root entry with parents should be valid"
    );
    assert!(!entry.is_root(), "Entry should not be identified as root");
}

#[test]
fn test_validate_empty_parent_id_fails() {
    // Empty parent IDs should be rejected
    let result = Entry::builder(ID::from_bytes("tree"))
        .add_parent("") // Empty parent ID
        .build();
    assert!(
        result.is_err(),
        "Entry with empty parent ID should fail to build"
    );

    // Check that it's the correct error type
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(
        error_msg.contains("EntryValidationFailed"),
        "Should be EntryValidationFailed error, got: {error_msg}"
    );
    assert!(
        error_msg.contains("empty parent ID"),
        "Error should mention empty parent ID, got: {error_msg}"
    );
}

#[test]
fn test_validate_subtree_with_empty_parent_id_fails() {
    // Subtrees with empty parent IDs should be rejected
    let result = Entry::root_builder()
        .set_subtree_data("messages", "test_data")
        .set_subtree_parents("messages", vec!["".into()]) // Empty subtree parent ID
        .build();
    assert!(
        result.is_err(),
        "Entry with empty subtree parent ID should fail to build"
    );

    // Check that it's the correct error type
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(
        error_msg.contains("EntryValidationFailed"),
        "Should be EntryValidationFailed error, got: {error_msg}"
    );
    assert!(
        error_msg.contains("empty parent ID"),
        "Error should mention empty parent ID, got: {error_msg}"
    );
}

#[test]
fn test_validate_non_root_with_empty_subtree_parents_logs_but_passes() {
    // Non-root entries with empty subtree parents should log but pass validation
    // This is deferred to transaction layer for deeper validation
    let entry = Entry::builder(ID::from_bytes("tree"))
        .add_parent(ID::from_bytes("main_parent"))
        .set_subtree_data("messages", "test_data")
        .set_subtree_parents("messages", vec![]) // Empty subtree parents
        .build()
        .expect("Entry with main parent should build successfully");

    // Should pass validation (deeper validation happens in transaction layer)
    assert!(
        entry.validate().is_ok(),
        "Non-root entry with empty subtree parents should pass entry-level validation"
    );
    assert!(!entry.is_root(), "Entry should not be identified as root");

    // Verify it has main tree parents but empty subtree parents
    assert!(
        !entry.parents().unwrap().is_empty(),
        "Should have main tree parents"
    );
    assert!(
        entry.subtree_parents("messages").unwrap().is_empty(),
        "Should have empty subtree parents"
    );
}

#[test]
fn test_validate_root_entry_with_empty_subtree_parents_succeeds() {
    // Root entries can have empty subtree parents (they establish subtree roots)
    let entry = Entry::root_builder()
        .set_subtree_data("messages", "test_data")
        .set_subtree_parents("messages", vec![]) // Empty subtree parents - valid for root
        .build()
        .expect("Root entry should build successfully");

    // Should pass validation
    assert!(
        entry.validate().is_ok(),
        "Root entry with empty subtree parents should be valid"
    );
    assert!(entry.is_root(), "Entry should be identified as root");
}

#[test]
fn test_validate_settings_subtree_follows_standard_rules() {
    // Settings subtree follows the same validation rules as other subtrees

    // Root entry with empty settings subtree parents should be valid
    let root_entry = Entry::root_builder()
        .set_subtree_data("_settings", "auth_config")
        .set_subtree_parents("_settings", vec![]) // Empty - valid for root
        .build()
        .expect("Root entry should build successfully");

    assert!(
        root_entry.validate().is_ok(),
        "Root entry with empty settings subtree parents should be valid"
    );

    // Non-root entry with empty settings subtree parents should pass entry validation
    // (deeper validation deferred to transaction layer)
    let non_root_entry = Entry::builder(ID::from_bytes("tree"))
        .add_parent(ID::from_bytes("main_parent"))
        .set_subtree_data("_settings", "auth_config")
        .set_subtree_parents("_settings", vec![]) // Empty - logs but passes entry validation
        .build()
        .expect("Entry with main parent should build successfully");

    assert!(
        non_root_entry.validate().is_ok(),
        "Non-root entry with empty settings subtree parents should pass entry validation"
    );
}

#[test]
fn test_validate_multiple_subtrees_with_mixed_parent_scenarios() {
    // Test entry with multiple subtrees having different parent scenarios
    let entry = Entry::builder(ID::from_bytes("tree"))
        .add_parent(ID::from_bytes("main_parent"))
        .set_subtree_data("messages", "msg_data")
        .set_subtree_parents("messages", vec![ID::from_bytes("msg_parent")]) // Valid parent
        .set_subtree_data("users", "user_data")
        .set_subtree_parents("users", vec![]) // Empty parents - deferred validation
        .build()
        .expect("Entry with main parent should build successfully");

    // Should pass entry-level validation
    assert!(
        entry.validate().is_ok(),
        "Entry with mixed subtree parent scenarios should pass entry validation"
    );

    // Verify the structure
    assert!(
        !entry.parents().unwrap().is_empty(),
        "Should have main tree parents"
    );
    assert!(
        !entry.subtree_parents("messages").unwrap().is_empty(),
        "Messages should have parents"
    );
    assert!(
        entry.subtree_parents("users").unwrap().is_empty(),
        "Users should have empty parents"
    );
}

#[test]
fn test_validate_root_subtree_marker_skipped() {
    // The "_root" marker subtree should be skipped during validation
    let entry = Entry::root_builder()
        .set_subtree_data("other_subtree", "data")
        .build()
        .expect("Root entry should build successfully");

    // Should pass validation - _root subtree validation is skipped
    assert!(
        entry.validate().is_ok(),
        "Entry with _root marker subtree should pass validation"
    );

    // Verify it has the _root marker
    assert!(
        entry.subtrees().contains(&"_root".to_string()),
        "Root entry should contain _root marker"
    );
}

#[test]
fn test_subtreenode_serde_backwards_compatibility() {
    // Test that SubTreeNode can deserialize old format (data as direct string)
    // and new format (data as None/Some)

    // Old format: data field present with direct string value
    let old_format_with_data = r#"{
        "name": "test_subtree",
        "parents": [],
        "data": "some data"
    }"#;
    let node: SubTreeNode =
        serde_json::from_str(old_format_with_data).expect("Should deserialize old format");
    assert_eq!(node.name, "test_subtree");
    assert_eq!(node.data, Some("some data".to_string()));

    // Old format: data field missing entirely
    let old_format_no_data = r#"{
        "name": "test_subtree",
        "parents": []
    }"#;
    let node: SubTreeNode =
        serde_json::from_str(old_format_no_data).expect("Should deserialize with missing data");
    assert_eq!(node.name, "test_subtree");
    assert_eq!(node.data, None);

    // New format: data explicitly null
    let new_format_null_data = r#"{
        "name": "test_subtree",
        "parents": [],
        "data": null
    }"#;
    let node: SubTreeNode =
        serde_json::from_str(new_format_null_data).expect("Should deserialize null data");
    assert_eq!(node.name, "test_subtree");
    assert_eq!(node.data, None);

    // Verify serialization: None data should not produce "data" field
    let node_with_none = SubTreeNode {
        name: "test".to_string(),
        parents: vec![],
        data: None,
        height: None,
    };
    let serialized = serde_json::to_string(&node_with_none).unwrap();
    assert!(
        !serialized.contains("data"),
        "None data should be skipped in serialization: {serialized}"
    );

    // Verify serialization: Some data should produce "data" field
    let node_with_data = SubTreeNode {
        name: "test".to_string(),
        parents: vec![],
        data: Some("content".to_string()),
        height: None,
    };
    let serialized = serde_json::to_string(&node_with_data).unwrap();
    assert!(
        serialized.contains(r#""data":"content""#),
        "Some data should be serialized: {serialized}"
    );

    // Verify height serialization: None should be omitted, Some(n) should serialize as just n
    let node_no_height = SubTreeNode {
        name: "test".to_string(),
        parents: vec![],
        data: None,
        height: None,
    };
    let serialized = serde_json::to_string(&node_no_height).unwrap();
    assert!(
        !serialized.contains("\"h\""),
        "None height should be omitted: {serialized}"
    );

    let node_with_height = SubTreeNode {
        name: "test".to_string(),
        parents: vec![],
        data: None,
        height: Some(42),
    };
    let serialized = serde_json::to_string(&node_with_height).unwrap();
    assert!(
        serialized.contains(r#""h":42"#),
        "Some(42) should serialize as h:42, not Some(42): {serialized}"
    );
}
