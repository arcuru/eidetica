//! Edge case tests for SigKey and flat delegation structure
//!
//! These tests cover edge cases, error conditions, and boundary scenarios
//! for the flat delegation structure implementation.

use eidetica::{
    Result,
    auth::{
        AuthSettings,
        types::{AuthKey, DelegationStep, KeyHint, Permission, SigInfo, SigKey},
        validation::AuthValidator,
    },
    crdt::Doc,
    entry::ID,
    store::DocStore,
};

use crate::helpers::{add_auth_key, test_instance, test_instance_with_user_and_key};

/// Test SigKey with empty delegation path
#[tokio::test]
async fn test_empty_delegation_path() -> Result<()> {
    let empty_delegation = SigKey::Delegation {
        path: vec![],
        hint: KeyHint::from_name("final"),
    };

    // Empty delegation path should be considered invalid
    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();
    let db = test_instance().await;

    let result = validator
        .resolve_sig_key(&empty_delegation, &auth_settings, Some(&db))
        .await;
    assert!(result.is_err());

    Ok(())
}

/// Test SigKey::Direct with name hint (not pubkey)
#[tokio::test]
async fn test_direct_key_name_hint() -> Result<()> {
    let (instance, mut user, key_id) =
        test_instance_with_user_and_key("test_user", Some("test_name")).await;

    // Create tree (signing key becomes Admin(0) with no name)
    let tree = user.create_database(Doc::new(), &key_id).await?;

    // Overwrite the bootstrapped key entry (same pubkey) to add the display
    // name "test_name" — set_auth_key replaces existing entries keyed by pubkey.
    // FIXME: consider adding a set_name method to AuthSettings/SettingsStore
    add_auth_key(
        &tree,
        &key_id,
        AuthKey::active(Some("test_name"), Permission::Admin(0)),
    )
    .await;

    // Test resolving by name hint - should find key with matching name
    let name_key = SigKey::from_name("test_name");
    let mut validator = AuthValidator::new();
    let auth_settings = tree.get_settings().await?.get_auth_settings().await?;

    let result = validator
        .resolve_sig_key(&name_key, &auth_settings, Some(&instance))
        .await;
    assert!(
        result.is_ok(),
        "Failed to resolve key by name: {:?}",
        result.err()
    );

    Ok(())
}

/// Test delegation path with empty tips in intermediate step
#[tokio::test]
async fn test_delegation_with_empty_tips_intermediate() -> Result<()> {
    let delegation_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "intermediate".to_string(),
            tips: vec![], // Empty tips for intermediate step
        }],
        hint: KeyHint::from_name("final_key"),
    };

    let mut validator = AuthValidator::new();
    let auth_settings = AuthSettings::new();
    let db = test_instance().await;

    let result = validator
        .resolve_sig_key(&delegation_path, &auth_settings, Some(&db))
        .await;
    // Should error because we can't resolve delegation without proper tips
    assert!(result.is_err());

    Ok(())
}

/// Test delegation path with duplicate tips
#[tokio::test]
async fn test_delegation_with_duplicate_tips() -> Result<()> {
    let duplicate_tips = vec![
        ID::from("tip1"),
        ID::from("tip2"),
        ID::from("tip1"), // Duplicate
        ID::from("tip3"),
    ];

    let delegation_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "delegate_tree".to_string(),
            tips: duplicate_tips,
        }],
        hint: KeyHint::from_name("final_key"),
    };

    // Serialization should work even with duplicates
    let serialized = serde_json::to_string(&delegation_path)?;
    let deserialized: SigKey = serde_json::from_str(&serialized)?;

    // Should be equal despite duplicates
    assert_eq!(delegation_path, deserialized);

    Ok(())
}

/// Test delegation path with extremely long key names
#[tokio::test]
async fn test_delegation_with_long_key_names() -> Result<()> {
    let long_key = "a".repeat(10000); // Very long key name

    let delegation_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: long_key.clone(),
            tips: vec![ID::from("tip1")],
        }],
        hint: KeyHint::from_name("final_key"),
    };

    // Should serialize/deserialize correctly
    let serialized = serde_json::to_string(&delegation_path)?;
    let deserialized: SigKey = serde_json::from_str(&serialized)?;
    assert_eq!(delegation_path, deserialized);

    Ok(())
}

/// Test delegation path with unicode characters
#[tokio::test]
async fn test_delegation_with_unicode_keys() -> Result<()> {
    let unicode_keys = vec!["🔑_key", "キー", "مفتاح", "ключ", "कुंजी", "🚀💻🔐"];

    for unicode_key in unicode_keys {
        let delegation_path = SigKey::Delegation {
            path: vec![DelegationStep {
                tree: unicode_key.to_string(),
                tips: vec![ID::from("tip1")],
            }],
            hint: KeyHint::from_name("final_key"),
        };

        // Should serialize/deserialize correctly
        let serialized = serde_json::to_string(&delegation_path)?;
        let deserialized: SigKey = serde_json::from_str(&serialized)?;
        assert_eq!(delegation_path, deserialized);
    }

    Ok(())
}

/// Test SigInfo with signature but missing key
#[tokio::test]
async fn test_sig_info_with_signature_no_key() {
    let sig_info = SigInfo::builder()
        .key(SigKey::from_pubkey("")) // Empty key
        .sig("fake_signature")
        .build();

    // Should serialize/deserialize correctly
    let serialized = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&serialized).unwrap();
    assert_eq!(sig_info, deserialized);
}

/// Test SigInfo with key but no signature
#[tokio::test]
async fn test_sig_info_with_key_no_signature() {
    let sig_info = SigInfo::builder()
        .key(SigKey::from_pubkey("valid_key"))
        .build(); // No signature

    // Should serialize/deserialize correctly
    let serialized = serde_json::to_string(&sig_info).unwrap();
    let deserialized: SigInfo = serde_json::from_str(&serialized).unwrap();
    assert_eq!(sig_info, deserialized);
}

/// Test very deep delegation path (not exceeding limit but close)
#[tokio::test]
#[cfg_attr(miri, ignore)] // Uses Instant::now() which Miri blocks
async fn test_deep_delegation_path_performance() -> Result<()> {
    // Create a delegation path with 9 levels (just under the limit of 10)
    let mut delegation_steps = Vec::new();

    for i in 0..9 {
        delegation_steps.push(DelegationStep {
            tree: format!("delegate_level_{i}"),
            tips: vec![ID::from(format!("tip_{i}"))],
        });
    }

    let delegation_path = SigKey::Delegation {
        path: delegation_steps,
        hint: KeyHint::from_name("final_key"),
    };

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
#[tokio::test]
async fn test_delegation_path_invalid_json() {
    // With untagged serde, SigKey tries Delegation first, then Direct(KeyHint).
    // KeyHint has all optional fields, so most invalid JSON for Delegation
    // will succeed as Direct(KeyHint) with fields ignored.
    //
    // Only truly invalid cases are those where field types don't match ANY variant:
    // - Wrong type for pubkey (not string) - fails both Delegation and Direct
    // - Wrong type for name (not string) - fails both variants
    let invalid_json_cases = vec![
        // Wrong type for pubkey (should be string or null)
        r#"{"pubkey": 123}"#,
        // Wrong type for name (should be string or null)
        r#"{"name": true}"#,
        // Not an object at all
        r#""just_a_string""#,
        // Null is not a valid SigKey
        r#"null"#,
    ];

    for invalid_json in invalid_json_cases {
        let result: std::result::Result<SigKey, _> = serde_json::from_str(invalid_json);
        assert!(result.is_err(), "Should fail to parse: {invalid_json}");
    }
}

/// Test circular delegation detection (simplified version)
#[tokio::test]
async fn test_circular_delegation_simple() -> Result<()> {
    let (instance, mut user, key_id) =
        test_instance_with_user_and_key("test_user", Some("admin")).await;

    // Create a tree (signing key becomes Admin(0))
    let tree = user.create_database(Doc::new(), &key_id).await?;
    let tree_tips = tree.get_tips().await?;

    // Create delegation path that references the same tree
    let circular_delegation = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: "self_reference".to_string(),
            tips: tree_tips,
        }],
        hint: KeyHint::from_pubkey(&key_id),
    };

    // Add self-referencing delegation to the tree
    let txn = tree.new_transaction().await?;
    let _dict = txn.get_store::<DocStore>("_settings").await?;

    // This should be detectable as a potential circular reference
    // For now, we just test that it doesn't crash
    let auth_settings = tree.get_settings().await?.get_auth_settings().await?;
    let mut validator = AuthValidator::new();
    let result = validator
        .resolve_sig_key(&circular_delegation, &auth_settings, Some(&instance))
        .await;

    // Should either work or fail gracefully (not crash)
    match result {
        Ok(_) => println!("Circular delegation resolved successfully"),
        Err(e) => println!("Circular delegation detected: {e}"),
    }

    Ok(())
}

/// Test delegation step serialization edge cases
#[tokio::test]
async fn test_delegation_step_serialization_edge_cases() -> Result<()> {
    let edge_cases = vec![
        // Normal case
        DelegationStep {
            tree: "normal_tree".to_string(),
            tips: vec![ID::from("tip1")],
        },
        // Empty tips array
        DelegationStep {
            tree: "empty_tips".to_string(),
            tips: vec![],
        },
        // Many tips
        DelegationStep {
            tree: "many_tips".to_string(),
            tips: (0..100).map(|i| ID::from(format!("tip_{i}"))).collect(),
        },
    ];

    for step in edge_cases {
        let serialized = serde_json::to_string(&step)?;
        let deserialized: DelegationStep = serde_json::from_str(&serialized)?;
        assert_eq!(step, deserialized);
    }

    Ok(())
}
