//! Comprehensive tests for authentication types

use super::*;
use crate::{
    auth::crypto::{format_public_key, generate_keypair},
    crdt::Doc,
    entry::ID,
};

fn generate_public_key() -> String {
    let (_, verifying_key) = generate_keypair();
    format_public_key(&verifying_key)
}

#[test]
fn test_permission_min_max() {
    use std::cmp::{max, min};

    // Test min/max with different permission levels
    assert_eq!(
        min(Permission::Admin(5), Permission::Write(10)),
        Permission::Write(10)
    );
    assert_eq!(
        max(Permission::Read, Permission::Write(1)),
        Permission::Write(1)
    );

    assert_eq!(
        min(Permission::Write(1), Permission::Write(5)),
        Permission::Write(5)
    );
    assert_eq!(
        max(Permission::Admin(1), Permission::Admin(5)),
        Permission::Admin(1)
    );
}

#[test]
fn test_auth_key_serialization() {
    let key = AuthKey::active(Some("my_device"), Permission::Write(10));

    let serialized = serde_json::to_string(&key).unwrap();
    let deserialized: AuthKey = serde_json::from_str(&serialized).unwrap();

    assert_eq!(key.name(), deserialized.name());
    assert_eq!(key.permissions(), deserialized.permissions());
    assert_eq!(key.status(), deserialized.status());
}

#[test]
fn test_sig_info_serialization() {
    let sig_info = SigInfo::builder()
        .key(SigKey::from_name("KEY_LAPTOP"))
        .sig("signature_base64_encoded_string_here")
        .build();

    let json = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(
        serde_json::to_string(&sig_info.key).unwrap(),
        serde_json::to_string(&deserialized.key).unwrap()
    );
    assert_eq!(sig_info.sig, deserialized.sig);
}

#[test]
fn test_delegation_sig_key() {
    let sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "example_tree_id".to_string(),
            tips: vec![ID::new("abc123")],
        }],
        hint: KeyHint::from_name("KEY_LAPTOP"),
    };

    let json = serde_json::to_string(&sig_key).unwrap();
    let deserialized: SigKey = serde_json::from_str(&json).unwrap();

    assert_eq!(
        serde_json::to_string(&sig_key).unwrap(),
        serde_json::to_string(&deserialized).unwrap()
    );
}

#[test]
fn test_auth_key_to_nested_value() {
    let key = AuthKey::active(Some("my_device"), Permission::Read);

    let mut nested = Doc::new();
    nested.set_json("test_key", &key).unwrap();

    // Test that we can retrieve it back
    let retrieved: AuthKey = nested.get_json("test_key").unwrap();
    assert_eq!(retrieved.name(), key.name());
    assert_eq!(retrieved.permissions(), key.permissions());
    assert_eq!(retrieved.status(), key.status());
}

#[test]
fn test_permission_nested_value_roundtrip() {
    let original = Permission::Write(42);
    let mut nested = Doc::new();
    nested.set_json("perm", &original).unwrap();
    let parsed: Permission = nested.get_json("perm").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_key_status_nested_value_roundtrip() {
    let original = KeyStatus::Revoked;
    let mut nested = Doc::new();
    nested.set_json("status", &original).unwrap();
    let parsed: KeyStatus = nested.get_json("status").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_vec_string_nested_value_roundtrip() {
    let original = vec!["tip1".to_string(), "tip2".to_string(), "tip3".to_string()];
    let mut nested = Doc::new();
    nested.set_json("vec", &original).unwrap();
    let parsed: Vec<String> = nested.get_json("vec").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_sig_key_nested_value_roundtrip() {
    let original = SigKey::from_name("KEY_LAPTOP");
    let mut nested = Doc::new();
    nested.set_json("sig_key", &original).unwrap();
    let parsed: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_sig_key_direct_format() {
    let sig_key = SigKey::from_name("KEY_LAPTOP");
    let mut nested = Doc::new();
    nested.set_json("sig_key", &sig_key).unwrap();

    // Test that we can retrieve it back correctly
    let retrieved: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(retrieved, sig_key);
}

#[test]
fn test_sig_key_delegation_format() {
    let sig_key = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "tree_id_123".to_string(),
            tips: vec![ID::new("tip1"), ID::new("tip2")],
        }],
        hint: KeyHint::from_name("KEY_LAPTOP"),
    };

    let mut nested = Doc::new();
    nested.set_json("sig_key", &sig_key).unwrap();

    // Test that we can retrieve it back correctly
    let retrieved: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(retrieved, sig_key);
}

#[test]
fn test_sig_key_delegation_roundtrip() {
    let original = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "tree_id_123".to_string(),
            tips: vec![ID::new("tip1"), ID::new("tip2")],
        }],
        hint: KeyHint::from_name("KEY_LAPTOP"),
    };

    let mut nested = Doc::new();
    nested.set_json("sig_key", &original).unwrap();
    let parsed: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_sig_info_nested_value_roundtrip() {
    let original = SigInfo::builder()
        .key(SigKey::from_name("KEY_LAPTOP"))
        .sig("signature_here")
        .build();
    let mut nested = Doc::new();
    nested.set_json("sig_info", &original).unwrap();
    let parsed: SigInfo = nested.get_json("sig_info").unwrap();
    assert_eq!(original.key, parsed.key);
    assert_eq!(original.sig, parsed.sig);
}

#[test]
fn test_tree_reference_nested_value_content() {
    let tree_ref = TreeReference {
        root: ID::new("root123"),
        tips: vec![ID::new("tip1"), ID::new("tip2")],
    };

    let mut nested = Doc::new();
    nested.set_json("tree_ref", &tree_ref).unwrap();

    // Test that we can retrieve it back correctly
    let retrieved: TreeReference = nested.get_json("tree_ref").unwrap();
    assert_eq!(retrieved.root, tree_ref.root);
    assert_eq!(retrieved.tips, tree_ref.tips);
}

#[test]
fn test_permission_bounds_clamping() {
    // Test permission clamping with bounds
    let bounds = PermissionBounds {
        max: Permission::Write(10),
        min: Some(Permission::Read),
    };

    // Test clamping admin to write (max bound)
    let admin_perm = Permission::Admin(5);
    assert_eq!(admin_perm.clamp_to_bounds(&bounds), Permission::Write(10));

    // Test clamping Write(5) to Write(10) (Write(5) exceeds max)
    let write_perm = Permission::Write(5);
    assert_eq!(write_perm.clamp_to_bounds(&bounds), Permission::Write(10));

    // Test minimum bound enforcement when no minimum specified
    let bounds_no_min = PermissionBounds {
        max: Permission::Admin(5),
        min: None,
    };

    let read_perm = Permission::Read;
    assert_eq!(read_perm.clamp_to_bounds(&bounds_no_min), Permission::Read);
}

#[test]
fn test_delegated_tree_ref_serialization() {
    let bounds = PermissionBounds {
        max: Permission::Write(10),
        min: Some(Permission::Read),
    };

    let tree_ref = DelegatedTreeRef {
        permission_bounds: bounds,
        tree: TreeReference {
            root: ID::new("root123"),
            tips: vec![ID::new("tip1")],
        },
    };

    let mut nested = Doc::new();
    nested.set_json("tree_ref", &tree_ref).unwrap();
    let parsed: DelegatedTreeRef = nested.get_json("tree_ref").unwrap();

    assert_eq!(tree_ref.permission_bounds.max, parsed.permission_bounds.max);
    assert_eq!(tree_ref.permission_bounds.min, parsed.permission_bounds.min);
    assert_eq!(tree_ref.tree.root, parsed.tree.root);
}

#[test]
fn test_option_permission_nested_value_roundtrip() {
    // Test Some(permission)
    let some_perm = Some(Permission::Write(42));
    let mut nested = Doc::new();
    nested.set_json("perm", &some_perm).unwrap();
    let parsed: Option<Permission> = nested.get_json("perm").unwrap();
    assert_eq!(some_perm, parsed);

    // Test None
    let none_perm: Option<Permission> = None;
    let mut nested2 = Doc::new();
    nested2.set_json("perm", &none_perm).unwrap();
    let parsed2: Option<Permission> = nested2.get_json("perm").unwrap();
    assert_eq!(none_perm, parsed2);
}

#[test]
fn test_option_u32_nested_value_roundtrip() {
    // Test Some(u32)
    let some_num = Some(42u32);
    let mut nested = Doc::new();
    nested.set_json("num", some_num).unwrap();
    let parsed: Option<u32> = nested.get_json("num").unwrap();
    assert_eq!(some_num, parsed);

    // Test None
    let none_num: Option<u32> = None;
    let mut nested2 = Doc::new();
    nested2.set_json("num", none_num).unwrap();
    let parsed2: Option<u32> = nested2.get_json("num").unwrap();
    assert_eq!(none_num, parsed2);
}

#[test]
fn test_permission_bounds_nested_value_roundtrip() {
    // Test with both min and max
    let bounds = PermissionBounds {
        max: Permission::Admin(5),
        min: Some(Permission::Read),
    };

    let mut nested = Doc::new();
    nested.set_json("bounds", &bounds).unwrap();
    let parsed: PermissionBounds = nested.get_json("bounds").unwrap();
    assert_eq!(bounds.max, parsed.max);
    assert_eq!(bounds.min, parsed.min);

    // Test with only max
    let bounds_no_min = PermissionBounds {
        max: Permission::Write(10),
        min: None,
    };

    let mut nested2 = Doc::new();
    nested2.set_json("bounds", &bounds_no_min).unwrap();
    let parsed2: PermissionBounds = nested2.get_json("bounds").unwrap();
    assert_eq!(bounds_no_min.max, parsed2.max);
    assert_eq!(bounds_no_min.min, parsed2.min);
}

#[test]
fn test_delegated_tree_ref_complete_roundtrip() {
    let tree_ref = DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Read),
        },
        tree: TreeReference {
            root: ID::new("root123"),
            tips: vec![ID::new("tip1"), ID::new("tip2")],
        },
    };

    let mut nested = Doc::new();
    nested.set_json("tree_ref", &tree_ref).unwrap();
    let parsed: DelegatedTreeRef = nested.get_json("tree_ref").unwrap();

    assert_eq!(tree_ref.permission_bounds.max, parsed.permission_bounds.max);
    assert_eq!(tree_ref.permission_bounds.min, parsed.permission_bounds.min);
    assert_eq!(tree_ref.tree.root, parsed.tree.root);
    assert_eq!(tree_ref.tree.tips, parsed.tree.tips);
}

#[test]
fn test_auth_key_nested_value_roundtrip() {
    let original = AuthKey::new(Some("my_key"), Permission::Write(42), KeyStatus::Revoked);

    let mut nested = Doc::new();
    nested.set_json("auth_key", &original).unwrap();
    let parsed: AuthKey = nested.get_json("auth_key").unwrap();

    assert_eq!(original.name(), parsed.name());
    assert_eq!(original.permissions(), parsed.permissions());
    assert_eq!(original.status(), parsed.status());
}

#[test]
fn test_auth_key_with_and_without_name() {
    // Test key with name
    let key_with_name = AuthKey::active(Some("my_device"), Permission::Write(10));
    assert_eq!(key_with_name.name(), Some("my_device"));
    assert_eq!(key_with_name.permissions(), &Permission::Write(10));
    assert_eq!(key_with_name.status(), &KeyStatus::Active);

    // Test key without name
    let key_without_name = AuthKey::active(None, Permission::Admin(5));
    assert_eq!(key_without_name.name(), None);
    assert_eq!(key_without_name.permissions(), &Permission::Admin(5));
    assert_eq!(key_without_name.status(), &KeyStatus::Active);
}

#[test]
fn test_auth_key_mutators() {
    let mut key = AuthKey::active(Some("test"), Permission::Write(10));

    // Test status modification
    key.set_status(KeyStatus::Revoked);
    assert_eq!(key.status(), &KeyStatus::Revoked);

    // Test permission modification
    key.set_permissions(Permission::Admin(5));
    assert_eq!(key.permissions(), &Permission::Admin(5));

    // Test name modification
    key.set_name(Some("new_name"));
    assert_eq!(key.name(), Some("new_name"));

    key.set_name(None);
    assert_eq!(key.name(), None);
}

#[test]
fn test_key_hint_types() {
    // Test pubkey hint
    let pubkey = generate_public_key();
    let hint = KeyHint::from_pubkey(&pubkey);
    assert_eq!(hint.pubkey, Some(pubkey.clone()));
    assert_eq!(hint.name, None);
    assert!(hint.is_set());
    assert_eq!(hint.hint_type(), "pubkey");

    // Test name hint
    let hint = KeyHint::from_name("my_key");
    assert_eq!(hint.pubkey, None);
    assert_eq!(hint.name, Some("my_key".to_string()));
    assert!(hint.is_set());
    assert_eq!(hint.hint_type(), "name");

    // Test global hint
    let hint = KeyHint::global(&pubkey);
    assert_eq!(hint.pubkey, Some(format!("*:{}", pubkey)));
    assert!(hint.is_global());
    assert_eq!(hint.global_actual_pubkey(), Some(pubkey.as_str()));

    // Test empty hint
    let hint = KeyHint::default();
    assert!(!hint.is_set());
    assert_eq!(hint.hint_type(), "none");
}

#[test]
fn test_sig_key_hint_access() {
    let pubkey = generate_public_key();

    // Test Direct SigKey
    let direct = SigKey::from_pubkey(&pubkey);
    assert!(direct.has_pubkey_hint(&pubkey));
    assert!(!direct.has_name_hint("test"));

    let direct_name = SigKey::from_name("test");
    assert!(direct_name.has_name_hint("test"));
    assert!(!direct_name.has_pubkey_hint(&pubkey));

    // Test Delegation SigKey
    let delegation = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "tree_id".to_string(),
            tips: vec![],
        }],
        hint: KeyHint::from_name("final_key"),
    };
    assert!(delegation.has_name_hint("final_key"));
    assert!(!delegation.has_pubkey_hint(&pubkey));
}

#[test]
fn test_delegation_step_serialization() {
    let step = DelegationStep {
        tree: "tree_123".to_string(),
        tips: vec![ID::new("tip1"), ID::new("tip2")],
    };

    let json = serde_json::to_string(&step).unwrap();
    let deserialized: DelegationStep = serde_json::from_str(&json).unwrap();

    assert_eq!(step.tree, deserialized.tree);
    assert_eq!(step.tips, deserialized.tips);
}

#[test]
fn test_permission_clamping() {
    assert_eq!(
        Permission::Admin(5).clamp_to(&Permission::Write(10)),
        Permission::Write(10)
    );
    assert_eq!(
        Permission::Admin(5).clamp_to(&Permission::Read),
        Permission::Read
    );
    assert_eq!(
        Permission::Write(5).clamp_to(&Permission::Read),
        Permission::Read
    );
    assert_eq!(
        Permission::Write(5).clamp_to(&Permission::Admin(10)),
        Permission::Write(5)
    );
    assert_eq!(
        Permission::Read.clamp_to(&Permission::Admin(10)),
        Permission::Read
    );
    assert_eq!(
        Permission::Read.clamp_to(&Permission::Read),
        Permission::Read
    );
    assert_eq!(
        Permission::Write(3).clamp_to(&Permission::Write(7)),
        Permission::Write(7)
    );
    assert_eq!(
        Permission::Admin(2).clamp_to(&Permission::Admin(1)),
        Permission::Admin(2)
    );
}

#[test]
fn test_permission_ordering() {
    // Test permission level ordering (Read < Write < Admin)
    assert!(Permission::Read < Permission::Write(1));
    assert!(Permission::Read < Permission::Admin(1));
    assert!(Permission::Write(1) < Permission::Admin(1));

    // Test priority ordering within same level
    assert!(Permission::Write(1) > Permission::Write(5));
    assert!(Permission::Admin(1) > Permission::Admin(5));

    // Test that permission level takes precedence over priority
    assert!(Permission::Write(100) < Permission::Admin(1));
    assert!(Permission::Read < Permission::Write(0));
    assert!(Permission::Read < Permission::Admin(0));

    // Test equality
    assert_eq!(Permission::Read, Permission::Read);
    assert_eq!(Permission::Write(5), Permission::Write(5));
    assert_eq!(Permission::Admin(10), Permission::Admin(10));

    // Test that different priorities make permissions different
    assert_ne!(Permission::Write(1), Permission::Write(2));
    assert_ne!(Permission::Admin(1), Permission::Admin(2));
}

#[test]
fn test_permission_methods() {
    // Test can_write
    assert!(!Permission::Read.can_write());
    assert!(Permission::Write(10).can_write());
    assert!(Permission::Admin(5).can_write());

    // Test can_admin
    assert!(!Permission::Read.can_admin());
    assert!(!Permission::Write(10).can_admin());
    assert!(Permission::Admin(5).can_admin());

    // Test priority
    assert_eq!(Permission::Read.priority(), None);
    assert_eq!(Permission::Write(10).priority(), Some(10));
    assert_eq!(Permission::Admin(5).priority(), Some(5));
}

#[test]
fn test_sig_info_with_global_serialization() {
    let pubkey = generate_public_key();
    let sig_info = SigInfo::builder()
        .key(SigKey::global(&pubkey))
        .sig("signature_base64_encoded_string_here")
        .build();

    let json = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(sig_info.key, deserialized.key);
    assert_eq!(sig_info.sig, deserialized.sig);
    assert!(sig_info.is_global());
}

#[test]
fn test_sig_info_builder_basic() {
    let sig_info = SigInfo::builder()
        .key(SigKey::from_name("KEY_LAPTOP"))
        .sig("test_signature")
        .build();

    assert!(sig_info.key.has_name_hint("KEY_LAPTOP"));
    assert_eq!(sig_info.sig, Some("test_signature".to_string()));
}

#[test]
fn test_sig_info_builder_with_pubkey_hint() {
    let pubkey = generate_public_key();
    let sig_info = SigInfo::builder()
        .pubkey_hint(&pubkey)
        .sig("test_signature")
        .build();

    assert!(sig_info.key.has_pubkey_hint(&pubkey));
    assert_eq!(sig_info.sig, Some("test_signature".to_string()));
}

#[test]
fn test_sig_info_builder_with_name_hint() {
    let sig_info = SigInfo::builder()
        .name_hint("my_key")
        .sig("test_signature")
        .build();

    assert!(sig_info.key.has_name_hint("my_key"));
    assert_eq!(sig_info.sig, Some("test_signature".to_string()));
}

#[test]
fn test_sig_info_builder_with_global_hint() {
    let pubkey = generate_public_key();
    let sig_info = SigInfo::builder()
        .global_hint(&pubkey)
        .sig("test_signature")
        .build();

    assert!(sig_info.is_global());
    assert_eq!(sig_info.sig, Some("test_signature".to_string()));
}

#[test]
fn test_sig_info_builder_minimal() {
    let sig_info = SigInfo::builder()
        .key(SigKey::from_name("KEY_LAPTOP"))
        .build();

    assert!(sig_info.key.has_name_hint("KEY_LAPTOP"));
    assert_eq!(sig_info.sig, None);
}

#[test]
#[should_panic(expected = "key is required for SigInfo")]
fn test_sig_info_builder_missing_key() {
    SigInfo::builder().sig("test_signature").build();
}

#[test]
fn test_sig_info_builder_delegation() {
    let delegation = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "tree_id".to_string(),
            tips: vec![ID::new("tip1")],
        }],
        hint: KeyHint::from_name("KEY_LAPTOP"),
    };

    let sig_info = SigInfo::builder()
        .key(delegation.clone())
        .sig("test_signature")
        .build();

    assert_eq!(sig_info.key, delegation);
    assert_eq!(sig_info.sig, Some("test_signature".to_string()));
}

#[test]
fn test_sig_info_default() {
    let default_sig_info = SigInfo::default();
    assert_eq!(default_sig_info.key, SigKey::default());
    assert_eq!(default_sig_info.sig, None);
}

#[test]
fn test_sig_info_is_unsigned() {
    // Default SigInfo is unsigned
    let default = SigInfo::default();
    assert!(default.is_unsigned());

    // With pubkey hint - not unsigned
    let with_hint = SigInfo::from_pubkey("ed25519:ABC");
    assert!(!with_hint.is_unsigned());

    // With name hint - not unsigned
    let with_name = SigInfo::from_name("my_key");
    assert!(!with_name.is_unsigned());

    // With signature - not unsigned
    let with_sig = SigInfo {
        sig: Some("signature".to_string()),
        ..Default::default()
    };
    assert!(!with_sig.is_unsigned());

    // Delegation is never unsigned (even with empty hint and no sig)
    let delegation = SigInfo {
        sig: None,
        key: SigKey::Delegation {
            path: vec![],
            hint: KeyHint::default(),
        },
    };
    assert!(!delegation.is_unsigned());
}

#[test]
fn test_sig_info_malformed_reason() {
    // Valid states: unsigned (default)
    let default = SigInfo::default();
    assert!(default.malformed_reason().is_none());

    // Valid states: properly signed with hint
    let signed = SigInfo {
        sig: Some("signature".to_string()),
        key: SigKey::from_pubkey("ed25519:ABC"),
    };
    assert!(signed.malformed_reason().is_none());

    // Valid states: properly signed delegation
    let signed_delegation = SigInfo {
        sig: Some("signature".to_string()),
        key: SigKey::Delegation {
            path: vec![],
            hint: KeyHint::from_name("key"),
        },
    };
    assert!(signed_delegation.malformed_reason().is_none());

    // Malformed: hint but no signature
    let hint_no_sig = SigInfo::from_pubkey("ed25519:ABC");
    assert_eq!(
        hint_no_sig.malformed_reason(),
        Some("entry has key hint but no signature")
    );

    // Malformed: signature but no hint (Direct with empty hint)
    let sig_no_hint = SigInfo {
        sig: Some("signature".to_string()),
        key: SigKey::Direct(KeyHint::default()),
    };
    assert_eq!(
        sig_no_hint.malformed_reason(),
        Some("entry has signature but no key hint")
    );

    // Malformed: delegation without signature
    let delegation_no_sig = SigInfo {
        sig: None,
        key: SigKey::Delegation {
            path: vec![],
            hint: KeyHint::from_name("key"),
        },
    };
    assert_eq!(
        delegation_no_sig.malformed_reason(),
        Some("delegation entry requires a signature")
    );
}

#[test]
fn test_sig_key_is_global() {
    let pubkey = generate_public_key();

    // Direct with global hint
    let global = SigKey::global(&pubkey);
    assert!(global.is_global());

    // Direct with regular pubkey
    let direct = SigKey::from_pubkey(&pubkey);
    assert!(!direct.is_global());

    // Direct with name
    let named = SigKey::from_name("test");
    assert!(!named.is_global());
}
