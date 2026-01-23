//! Tests for Entry version validation during deserialization.

#[test]
fn entry_deserialize_wrong_version_fails() {
    // Construct a JSON entry with an unsupported version
    // SigKey is now a struct with explicit hint fields
    let json = r#"{
        "_v": 99,
        "tree": { "root": "", "parents": [] },
        "subtrees": [],
        "sig": {"key": {}}
    }"#;

    let result: Result<eidetica::Entry, _> = serde_json::from_str(json);
    assert!(result.is_err(), "Should fail to deserialize wrong version");
}

#[test]
fn entry_deserialize_missing_version_defaults_to_v0() {
    // Entry without version field should default to v0 and succeed
    // SigKey is now a struct with explicit hint fields (untagged enum)
    let json = r#"{
        "tree": { "root": "", "parents": [] },
        "subtrees": [],
        "sig": {"key": {}}
    }"#;

    let result: Result<eidetica::Entry, _> = serde_json::from_str(json);
    assert!(result.is_ok(), "Missing version should default to v0");
}

#[test]
fn entry_roundtrip() {
    use eidetica::Entry;

    let entry = Entry::root_builder().build().unwrap();
    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: Entry = serde_json::from_str(&json).unwrap();

    // Both should be equivalent (v0)
    assert_eq!(entry, deserialized);
}
