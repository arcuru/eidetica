//! Comprehensive tests for authentication types

use super::*;
use crate::{crdt::Doc, entry::ID};

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
    let key = AuthKey {
        pubkey: "ed25519:PExACKOW0L7bKAM9mK_mH3L5EDwszC437uRzTqAbxpk".to_string(),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };

    let serialized = serde_json::to_string(&key).unwrap();
    let deserialized: AuthKey = serde_json::from_str(&serialized).unwrap();

    assert_eq!(key.pubkey, deserialized.pubkey);
    assert_eq!(key.permissions, deserialized.permissions);
    assert_eq!(key.status, deserialized.status);
}

#[test]
fn test_sig_info_serialization() {
    let sig_info = SigInfo {
        key: SigKey::Direct("KEY_LAPTOP".to_string()),
        sig: Some("signature_base64_encoded_string_here".to_string()),
    };

    let json = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(
        serde_json::to_string(&sig_info.key).unwrap(),
        serde_json::to_string(&deserialized.key).unwrap()
    );
    assert_eq!(sig_info.sig, deserialized.sig);
}

#[test]
fn test_delegation_path_sig_key() {
    let sig_key = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "example@eidetica.dev".to_string(),
            tips: Some(vec![ID::new("abc123")]),
        },
        DelegationStep {
            key: "KEY_LAPTOP".to_string(),
            tips: None,
        },
    ]);

    let json = serde_json::to_string(&sig_key).unwrap();
    let deserialized: SigKey = serde_json::from_str(&json).unwrap();

    assert_eq!(
        serde_json::to_string(&sig_key).unwrap(),
        serde_json::to_string(&deserialized).unwrap()
    );
}

#[test]
fn test_auth_key_to_nested_value() {
    let key = AuthKey {
        pubkey: "ed25519:test_key".to_string(),
        permissions: Permission::Read,
        status: KeyStatus::Active,
    };

    let mut nested = Doc::new();
    nested.set_json("test_key", &key).unwrap();

    // Test that we can retrieve it back
    let retrieved: AuthKey = nested.get_json("test_key").unwrap();
    assert_eq!(retrieved.pubkey, key.pubkey);
    assert_eq!(retrieved.permissions, key.permissions);
    assert_eq!(retrieved.status, key.status);
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
    let original = SigKey::Direct("KEY_LAPTOP".to_string());
    let mut nested = Doc::new();
    nested.set_json("sig_key", &original).unwrap();
    let parsed: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_sig_key_direct_format() {
    let sig_key = SigKey::Direct("KEY_LAPTOP".to_string());
    let mut nested = Doc::new();
    nested.set_json("sig_key", &sig_key).unwrap();

    // Test that we can retrieve it back correctly
    let retrieved: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(retrieved, sig_key);
}

#[test]
fn test_sig_key_delegation_path_format() {
    let sig_key = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "user@example.com".to_string(),
            tips: Some(vec![ID::new("tip1"), ID::new("tip2")]),
        },
        DelegationStep {
            key: "KEY_LAPTOP".to_string(),
            tips: None,
        },
    ]);

    let mut nested = Doc::new();
    nested.set_json("sig_key", &sig_key).unwrap();

    // Test that we can retrieve it back correctly
    let retrieved: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(retrieved, sig_key);
}

#[test]
fn test_sig_key_delegation_path_roundtrip() {
    let original = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "user@example.com".to_string(),
            tips: Some(vec![ID::new("tip1"), ID::new("tip2")]),
        },
        DelegationStep {
            key: "KEY_LAPTOP".to_string(),
            tips: None,
        },
    ]);

    let mut nested = Doc::new();
    nested.set_json("sig_key", &original).unwrap();
    let parsed: SigKey = nested.get_json("sig_key").unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn test_sig_info_nested_value_roundtrip() {
    let original = SigInfo {
        key: SigKey::Direct("KEY_LAPTOP".to_string()),
        sig: Some("signature_here".to_string()),
    };
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
    let original = AuthKey {
        pubkey: "ed25519:test_key".to_string(),
        permissions: Permission::Write(42),
        status: KeyStatus::Revoked,
    };

    let mut nested = Doc::new();
    nested.set_json("auth_key", &original).unwrap();
    let parsed: AuthKey = nested.get_json("auth_key").unwrap();

    assert_eq!(original.pubkey, parsed.pubkey);
    assert_eq!(original.permissions, parsed.permissions);
    assert_eq!(original.status, parsed.status);
}

#[test]
fn test_sig_key_is_signed_by() {
    // Test direct key
    let direct_key = SigKey::Direct("KEY_LAPTOP".to_string());
    assert!(direct_key.is_signed_by("KEY_LAPTOP"));
    assert!(!direct_key.is_signed_by("KEY_DESKTOP"));

    // Test delegation path
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "user@example.com".to_string(),
            tips: Some(vec![ID::new("tip1")]),
        },
        DelegationStep {
            key: "KEY_LAPTOP".to_string(),
            tips: None,
        },
    ]);
    assert!(delegation_path.is_signed_by("KEY_LAPTOP"));
    assert!(!delegation_path.is_signed_by("user@example.com"));
    assert!(!delegation_path.is_signed_by("KEY_DESKTOP"));

    // Test empty delegation path
    let empty_path = SigKey::DelegationPath(vec![]);
    assert!(!empty_path.is_signed_by("KEY_LAPTOP"));
}

#[test]
fn test_delegation_step_serialization() {
    let step = DelegationStep {
        key: "user@example.com".to_string(),
        tips: Some(vec![ID::new("tip1"), ID::new("tip2")]),
    };

    let json = serde_json::to_string(&step).unwrap();
    let deserialized: DelegationStep = serde_json::from_str(&json).unwrap();

    assert_eq!(step.key, deserialized.key);
    assert_eq!(step.tips, deserialized.tips);

    // Test final step (no tips)
    let final_step = DelegationStep {
        key: "KEY_LAPTOP".to_string(),
        tips: None,
    };

    let json = serde_json::to_string(&final_step).unwrap();
    let deserialized: DelegationStep = serde_json::from_str(&json).unwrap();

    assert_eq!(final_step.key, deserialized.key);
    assert_eq!(final_step.tips, deserialized.tips);
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
