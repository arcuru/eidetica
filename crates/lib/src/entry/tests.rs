//! Tests for Entry and EntryBuilder

use super::*;
use crate::auth::types::{DelegationStep, KeyHint, SigInfo, SigKey};

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
        .add_parent(ID::default()) // Empty parent ID
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
        .set_subtree_data("messages", b"test_data")
        .set_subtree_parents("messages", vec![ID::default()]) // Empty subtree parent ID
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
        .set_subtree_data("messages", b"test_data")
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
        .set_subtree_data("messages", b"test_data")
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
        .set_subtree_data("_settings", b"auth_config")
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
        .set_subtree_data("_settings", b"auth_config")
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
        .set_subtree_data("messages", b"msg_data")
        .set_subtree_parents("messages", vec![ID::from_bytes("msg_parent")]) // Valid parent
        .set_subtree_data("users", b"user_data")
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
        .set_subtree_data("other_subtree", b"data")
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
fn test_subtreenode_serde_roundtrip() {
    // Verify SubTreeNode round-trips through DAG-CBOR with the opaque-bytes payload.

    // Missing data field deserializes as None.
    let no_data = r#"{
        "name": "test_subtree",
        "parents": []
    }"#;
    let node: SubTreeNode =
        serde_json::from_str(no_data).expect("Should deserialize with missing data");
    assert_eq!(node.name, "test_subtree");
    assert_eq!(node.data, None);

    // Explicit null data deserializes as None.
    let null_data = r#"{
        "name": "test_subtree",
        "parents": [],
        "data": null
    }"#;
    let node: SubTreeNode = serde_json::from_str(null_data).expect("Should deserialize null data");
    assert_eq!(node.data, None);

    // None data is skipped in serialization.
    let node_with_none = SubTreeNode {
        name: "test".to_string(),
        parents: vec![],
        data: None,
        height: None,
    };
    let serialized = serde_json::to_string(&node_with_none).unwrap();
    assert!(
        !serialized.contains("\"data\""),
        "None data should be skipped in serialization: {serialized}"
    );

    // Bytes round-trip through DAG-CBOR (which is the on-wire format).
    let node_with_bytes = SubTreeNode {
        name: "test".to_string(),
        parents: vec![],
        data: Some(b"content".to_vec()),
        height: None,
    };
    let cbor = serde_ipld_dagcbor::to_vec(&node_with_bytes).unwrap();
    let decoded: SubTreeNode = serde_ipld_dagcbor::from_slice(&cbor).unwrap();
    assert_eq!(decoded, node_with_bytes);

    // Height None is omitted; Some(n) serializes as `h: n`.
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

/// Verify that subtree payloads and metadata are encoded as CBOR byte strings
/// (major type 2), not as CBOR arrays of u8. This is required for IPLD /
/// DAG-CBOR interop — other IPLD implementations expect major type 2 for byte
/// data, and the IPLD model has no separate "array of small ints" path that
/// other languages would round-trip as bytes.
///
/// Decodes via `ipld_core::Ipld` and structurally inspects the value rather
/// than scanning raw bytes, so length-prefix encoding (inline / 1-byte /
/// 2-byte / 4-byte / 8-byte length) doesn't matter — payloads of any size
/// must land in the `Ipld::Bytes` variant.
#[test]
fn test_subtree_data_is_cbor_byte_string() {
    use ipld_core::ipld::Ipld;

    // Cover the empty case (zero-length CBOR byte string, header `0x40`), the
    // inline (≤23), 1-byte length (≤255), and 2-byte length (≤65535) length-prefix
    // forms so we don't only validate one header shape. The empty case also
    // exercises the `Some(vec![])` "explicit empty data" state that the design
    // doc distinguishes from `None` (subtree participates with no data change).
    let cases: &[(&str, Vec<u8>)] = &[
        ("empty", vec![]),
        ("inline_5", b"hello".to_vec()),
        ("len1_24", vec![0xab; 24]),
        ("len2_300", vec![0xcd; 300]),
    ];

    for (name, payload) in cases {
        let metadata = vec![0xee; payload.len()];
        let entry = Entry::root_builder()
            .set_subtree_data(*name, payload.clone())
            .set_metadata(metadata.clone())
            .build()
            .expect("Root entry should build successfully");

        let cbor = entry.to_dagcbor().expect("DAG-CBOR serialization");
        let value: Ipld =
            serde_ipld_dagcbor::from_slice(&cbor).expect("decode entry as Ipld value");

        // Walk { tree: { metadata: <Bytes> }, subtrees: [ { name, data: <Bytes> } ] }.
        let root = match &value {
            Ipld::Map(m) => m,
            other => panic!("entry root is not a map: {other:?}"),
        };

        let tree = match root.get("tree") {
            Some(Ipld::Map(m)) => m,
            other => panic!("tree field is not a map: {other:?}"),
        };
        match tree.get("metadata") {
            Some(Ipld::Bytes(b)) => assert_eq!(b, &metadata, "[{name}] metadata bytes mismatch"),
            other => {
                panic!("[{name}] metadata must be Ipld::Bytes (CBOR major type 2), got: {other:?}")
            }
        }

        let subtrees = match root.get("subtrees") {
            Some(Ipld::List(l)) => l,
            other => panic!("subtrees field is not a list: {other:?}"),
        };
        let subtree = subtrees
            .iter()
            .find_map(|st| match st {
                Ipld::Map(m) => match m.get("name") {
                    Some(Ipld::String(s)) if s == name => Some(m),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("[{name}] subtree not found in {subtrees:?}"));
        match subtree.get("data") {
            Some(Ipld::Bytes(b)) => assert_eq!(b, payload, "[{name}] subtree bytes mismatch"),
            other => panic!(
                "[{name}] subtree data must be Ipld::Bytes (CBOR major type 2), got: {other:?}"
            ),
        }

        // And the entry round-trips through DAG-CBOR.
        let decoded: Entry = serde_ipld_dagcbor::from_slice(&cbor).unwrap();
        assert_eq!(entry, decoded, "[{name}] round-trip mismatch");
    }
}

#[test]
fn test_entry_dagcbor_roundtrip_direct_sigkey() {
    // Test DAG-CBOR roundtrip with a Direct SigKey (default, unsigned)
    let entry = Entry::root_builder()
        .set_subtree_data("users", br#"{"user1":"data"}"#)
        .build()
        .expect("Root entry should build successfully");

    let bytes = serde_ipld_dagcbor::to_vec(&entry).unwrap();
    let decoded: Entry = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
    assert_eq!(entry, decoded);

    // Verify the ID can be computed and contains a valid CID
    let id = entry.id();
    let cid = id.as_cid().expect("ID should contain a CID");
    assert_eq!(cid.version(), cid::Version::V1);
    assert_eq!(cid.codec(), 0x71); // dag-cbor codec
}

#[test]
fn test_entry_dagcbor_roundtrip_delegation_sigkey() {
    // Test DAG-CBOR roundtrip with a Delegation SigKey (uses untagged enum + flatten)
    let sig = SigInfo {
        sig: Some("dGVzdF9zaWduYXR1cmU=".to_string()),
        key: SigKey::Delegation {
            path: vec![DelegationStep {
                tree: ID::from_bytes("delegated_tree_id"),
                tips: vec![ID::from_bytes("tip1"), ID::from_bytes("tip2")],
            }],
            hint: KeyHint::from_name("alice"),
        },
    };

    let entry = Entry::builder(ID::from_bytes("tree_root"))
        .add_parent(ID::from_bytes("parent_entry"))
        .set_subtree_data("data_store", br#"{"key":"value"}"#)
        .set_sig(sig)
        .build()
        .expect("Entry with delegation should build successfully");

    let bytes = serde_ipld_dagcbor::to_vec(&entry).unwrap();
    let decoded: Entry = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
    assert_eq!(entry, decoded);
}

#[test]
fn test_entry_dagcbor_roundtrip_with_pubkey_sigkey() {
    use crate::auth::crypto::PrivateKey;

    // Test with a Direct SigKey using pubkey hint
    let private_key = PrivateKey::generate();
    let sig = SigInfo::from_pubkey(&private_key.public_key());

    let entry = Entry::builder(ID::from_bytes("tree_root"))
        .add_parent(ID::from_bytes("parent_entry"))
        .set_subtree_data("store", b"data")
        .set_sig(sig)
        .build()
        .expect("Entry with pubkey sig should build successfully");

    let bytes = serde_ipld_dagcbor::to_vec(&entry).unwrap();
    let decoded: Entry = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
    assert_eq!(entry, decoded);
}

#[test]
fn test_entry_to_dagcbor_method() {
    let entry = Entry::root_builder()
        .set_subtree_data("test", b"value")
        .build()
        .expect("Root entry should build successfully");

    // to_dagcbor() should produce valid CBOR
    let bytes = entry
        .to_dagcbor()
        .expect("DAG-CBOR serialization should succeed");
    let decoded: Entry = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
    assert_eq!(entry, decoded);

    // id() should be deterministic
    let id1 = entry.id();
    let id2 = entry.id();
    assert_eq!(id1, id2);
}
