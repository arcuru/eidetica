//! Edge case tests for SigKey and flat delegation structure
//!
//! These tests cover edge cases, error conditions, and boundary scenarios
//! for the flat delegation structure implementation.

use eidetica::{
    Instance, Result,
    auth::{
        AuthSettings,
        crypto::format_public_key,
        types::{AuthKey, DelegationStep, Permission, SigInfo, SigKey},
        validation::AuthValidator,
    },
    backend::database::InMemory,
    crdt::Doc,
    entry::ID,
    instance::LegacyInstanceOps,
};

/// Test SigKey with empty delegation path
#[test]
fn test_empty_delegation_path() -> Result<()> {
    let empty_delegation = SigKey::DelegationPath(vec![]);

    // Empty delegation path should be considered invalid
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();
    let db = Instance::open(Box::new(InMemory::new())).expect("Failed to create test instance");

    let result = validator.resolve_sig_key(&empty_delegation, &auth_settings, Some(&db));
    assert!(result.is_err());

    Ok(())
}

/// Test SigKey::Direct with empty key ID
#[test]
fn test_direct_key_empty_id() -> Result<()> {
    let db = Instance::open(Box::new(InMemory::new())).expect("Failed to create test instance");

    // Add private key with empty ID to storage
    let admin_key = db.add_private_key("")?;

    // Create tree with empty key ID
    let mut auth = Doc::new();
    auth.set_json(
        "", // Empty key ID
        AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
    )
    .unwrap();

    let mut settings = Doc::new();
    settings.set("auth", auth);

    // This should work - empty key is technically valid
    let tree = db.new_database(settings, "")?;

    // Test resolving empty key ID
    let empty_key = SigKey::Direct("".to_string());
    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings()?.get_auth_settings()?;

    let result = validator.resolve_sig_key(&empty_key, &auth_settings, Some(&db));
    assert!(
        result.is_ok(),
        "Failed to resolve empty key: {:?}",
        result.err()
    );

    Ok(())
}

/// Test delegation path with null tips in intermediate step
#[test]
fn test_delegation_with_null_tips_intermediate() -> Result<()> {
    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "intermediate".to_string(),
            tips: None, // Should have tips for intermediate step
        },
        DelegationStep {
            key: "final_key".to_string(),
            tips: None,
        },
    ]);

    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();
    let db = Instance::open(Box::new(InMemory::new())).expect("Failed to create test instance");

    let result = validator.resolve_sig_key(&delegation_path, &auth_settings, Some(&db));
    // Should error because intermediate steps need tips
    assert!(result.is_err());

    Ok(())
}

/// Test delegation path with duplicate tips
#[test]
fn test_delegation_with_duplicate_tips() -> Result<()> {
    let duplicate_tips = vec![
        ID::from("tip1"),
        ID::from("tip2"),
        ID::from("tip1"), // Duplicate
        ID::from("tip3"),
    ];

    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "delegate_tree".to_string(),
            tips: Some(duplicate_tips),
        },
        DelegationStep {
            key: "final_key".to_string(),
            tips: None,
        },
    ]);

    // Serialization should work even with duplicates
    let serialized = serde_json::to_string(&delegation_path)?;
    let deserialized: SigKey = serde_json::from_str(&serialized)?;

    // Should be equal despite duplicates
    assert_eq!(delegation_path, deserialized);

    Ok(())
}

/// Test delegation path with extremely long key names
#[test]
fn test_delegation_with_long_key_names() -> Result<()> {
    let long_key = "a".repeat(10000); // Very long key name

    let delegation_path = SigKey::DelegationPath(vec![
        DelegationStep {
            key: long_key.clone(),
            tips: Some(vec![ID::from("tip1")]),
        },
        DelegationStep {
            key: "final_key".to_string(),
            tips: None,
        },
    ]);

    // Should serialize/deserialize correctly
    let serialized = serde_json::to_string(&delegation_path)?;
    let deserialized: SigKey = serde_json::from_str(&serialized)?;
    assert_eq!(delegation_path, deserialized);

    Ok(())
}

/// Test delegation path with unicode characters
#[test]
fn test_delegation_with_unicode_keys() -> Result<()> {
    let unicode_keys = vec!["🔑_key", "キー", "مفتاح", "ключ", "कुंजी", "🚀💻🔐"];

    for unicode_key in unicode_keys {
        let delegation_path = SigKey::DelegationPath(vec![
            DelegationStep {
                key: unicode_key.to_string(),
                tips: Some(vec![ID::from("tip1")]),
            },
            DelegationStep {
                key: "final_key".to_string(),
                tips: None,
            },
        ]);

        // Should serialize/deserialize correctly
        let serialized = serde_json::to_string(&delegation_path)?;
        let deserialized: SigKey = serde_json::from_str(&serialized)?;
        assert_eq!(delegation_path, deserialized);
    }

    Ok(())
}

/// Test SigInfo with signature but missing key
#[test]
fn test_sig_info_with_signature_no_key() {
    let sig_info = SigInfo::builder()
        .key(SigKey::Direct("".to_string())) // Empty key
        .sig("fake_signature")
        .build();

    // Should serialize/deserialize correctly
    let serialized = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&serialized).unwrap();
    assert_eq!(sig_info, deserialized);
}

/// Test SigInfo with key but no signature
#[test]
fn test_sig_info_with_key_no_signature() {
    let sig_info = SigInfo::builder()
        .key(SigKey::Direct("valid_key".to_string()))
        .build(); // No signature

    // Should serialize/deserialize correctly
    let serialized = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&serialized).unwrap();
    assert_eq!(sig_info, deserialized);
}

/// Test very deep delegation path (not exceeding limit but close)
#[test]
fn test_deep_delegation_path_performance() -> Result<()> {
    // Create a delegation path with 9 levels (just under the limit of 10)
    let mut delegation_steps = Vec::new();

    for i in 0..9 {
        delegation_steps.push(DelegationStep {
            key: format!("delegate_level_{i}"),
            tips: Some(vec![ID::from(format!("tip_{i}"))]),
        });
    }

    delegation_steps.push(DelegationStep {
        key: "final_key".to_string(),
        tips: None,
    });

    let delegation_path = SigKey::DelegationPath(delegation_steps);

    // Should serialize/deserialize without issues
    let start = std::time::Instant::now();
    let serialized = serde_json::to_string(&delegation_path)?;
    let _deserialized: SigKey = serde_json::from_str(&serialized)?;
    let duration = start.elapsed();

    // Should complete reasonably quickly (less than 1 second)
    assert!(duration.as_secs() < 1);

    Ok(())
}

/// Test delegation path with invalid JSON structure
#[test]
fn test_delegation_path_invalid_json() {
    let invalid_json_cases = vec![
        r#"{"DelegationPath": "not_an_array"}"#,
        r#"{"DelegationPath": [{"key": "test"}]}"#, // Missing tips field structure
        r#"{"DelegationPath": [{"tips": ["tip1"]}]}"#, // Missing key field
        r#"{"DelegationPath": [{"key": 123, "tips": null}]}"#, // Wrong type for key
        r#"{"DelegationPath": [{"key": "test", "tips": "not_array"}]}"#, // Wrong type for tips
    ];

    for invalid_json in invalid_json_cases {
        let result: std::result::Result<SigKey, _> = serde_json::from_str(invalid_json);
        assert!(result.is_err(), "Should fail to parse: {invalid_json}");
    }
}

/// Test circular delegation detection (simplified version)
#[test]
fn test_circular_delegation_simple() -> Result<()> {
    let db = Instance::open(Box::new(InMemory::new())).expect("Failed to create test instance");

    // Add private key to storage
    let admin_key = db.add_private_key("admin")?;

    // Create a tree that delegates to itself
    let mut auth = Doc::new();
    auth.set_json(
        "admin",
        AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
    )
    .unwrap();

    let mut settings = Doc::new();
    settings.set("auth", auth);
    let tree = db.new_database(settings, "admin")?;
    let tree_tips = tree.get_tips()?;

    // Create delegation path that references the same tree
    let circular_delegation = SigKey::DelegationPath(vec![
        DelegationStep {
            key: "self_reference".to_string(),
            tips: Some(tree_tips),
        },
        DelegationStep {
            key: "admin".to_string(),
            tips: None,
        },
    ]);

    // Add self-referencing delegation to the tree
    let op = tree.new_transaction()?.with_auth("admin");
    let _dict = op.get_store::<eidetica::store::DocStore>("_settings")?;

    // This should be detectable as a potential circular reference
    // For now, we just test that it doesn't crash
    let auth_settings = tree.get_settings()?.get_auth_settings()?;
    let mut validator = AuthValidator::new();
    let result = validator.resolve_sig_key(&circular_delegation, &auth_settings, Some(&db));

    // Should either work or fail gracefully (not crash)
    match result {
        Ok(_) => println!("Circular delegation resolved successfully"),
        Err(e) => println!("Circular delegation detected: {e}"),
    }

    Ok(())
}

/// Test delegation step serialization edge cases
#[test]
fn test_delegation_step_serialization_edge_cases() -> Result<()> {
    let edge_cases = vec![
        // Normal case
        DelegationStep {
            key: "normal_key".to_string(),
            tips: Some(vec![ID::from("tip1")]),
        },
        // Empty tips array
        DelegationStep {
            key: "empty_tips".to_string(),
            tips: Some(vec![]),
        },
        // Null tips
        DelegationStep {
            key: "null_tips".to_string(),
            tips: None,
        },
        // Many tips
        DelegationStep {
            key: "many_tips".to_string(),
            tips: Some((0..100).map(|i| ID::from(format!("tip_{i}"))).collect()),
        },
    ];

    for step in edge_cases {
        let serialized = serde_json::to_string(&step)?;
        let deserialized: DelegationStep = serde_json::from_str(&serialized)?;
        assert_eq!(step, deserialized);
    }

    Ok(())
}
