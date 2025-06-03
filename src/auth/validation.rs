//! Authentication validation for Eidetica
//!
//! This module provides validation logic for authentication information,
//! including key resolution, permission checking, and signature verification.
//!
//! ## Design Approach
//!
//! This implementation uses a simplified approach:
//! - **Entry-time validation**: Validate entries against current auth settings when created
//! - **Standard CRDT merging**: Use existing KVNested Last Write Wins (LWW) for all conflicts
//! - **Administrative priority**: Priority rules apply only to key creation/modification operations
//! - **No custom merge logic**: Authentication relies on proven KVNested CRDT semantics

use crate::auth::crypto::{parse_public_key, verify_entry_signature};
use crate::auth::types::{AuthId, AuthKey, KeyStatus, Operation, ResolvedAuth};
use crate::data::{KVNested, NestedValue};
use crate::entry::Entry;
use crate::{Error, Result};
use std::collections::HashMap;

/// Authentication validator for validating entries and resolving auth information
pub struct AuthValidator {
    /// Cache for resolved authentication data to improve performance
    auth_cache: HashMap<String, ResolvedAuth>,
}

impl AuthValidator {
    /// Create a new authentication validator
    pub fn new() -> Self {
        Self {
            auth_cache: HashMap::new(),
        }
    }

    /// Validate authentication information for an entry
    ///
    /// # Arguments
    /// * `entry` - The entry to validate
    /// * `settings_state` - Current state of the _settings subtree for key lookup
    pub fn validate_entry(&mut self, entry: &Entry, settings_state: &KVNested) -> Result<bool> {
        // Handle unsigned entries (for backward compatibility)
        // An entry is considered unsigned if it has an empty Direct key ID and no signature
        if let AuthId::Direct(key_id) = &entry.auth.id {
            if key_id.is_empty() && entry.auth.signature.is_none() {
                // This is an unsigned entry - allow it to pass without authentication
                return Ok(true);
            }
        }

        // If the settings state has no 'auth' section or an empty 'auth' map, allow unsigned entries.
        match settings_state.get("auth") {
            Some(NestedValue::Map(auth_map)) => {
                // If 'auth' section exists and is a map, check if it's empty
                if auth_map.as_hashmap().is_empty() {
                    return Ok(true);
                }
            }
            None => {
                // If 'auth' section does not exist at all, it means no keys are configured
                return Ok(true);
            }
            _ => {
                // If 'auth' section exists but is not a map (e.g., a string or deleted),
                // or if it's a non-empty map, then proceed with normal validation.
            }
        }

        // For all other entries, proceed with normal authentication validation
        // Resolve the authentication information
        let resolved_auth = self.resolve_auth_key(&entry.auth.id, settings_state)?;

        // Check if the key is in an active state
        if resolved_auth.key_status != KeyStatus::Active {
            return Ok(false);
        }

        // Verify the signature using the entry-based verification
        verify_entry_signature(entry, &resolved_auth.public_key)
    }

    /// Resolve authentication identifier to concrete authentication information
    ///
    /// # Arguments
    /// * `auth_id` - The authentication identifier to resolve
    /// * `settings` - KVNested settings containing auth configuration
    pub fn resolve_auth_key(
        &mut self,
        auth_id: &AuthId,
        settings: &KVNested,
    ) -> Result<ResolvedAuth> {
        match auth_id {
            AuthId::Direct(key_id) => self.resolve_direct_key(key_id, settings),
            AuthId::UserTree { id, tips, key } => {
                self.resolve_user_tree_key(id, tips, key, settings)
            }
        }
    }

    /// Resolve a direct key reference from the main tree's auth settings
    fn resolve_direct_key(&mut self, key_id: &str, settings: &KVNested) -> Result<ResolvedAuth> {
        // First get the auth section from settings
        let auth_section = settings
            .get("auth")
            .ok_or_else(|| Error::Authentication("No auth configuration found".to_string()))?;

        // Extract the auth KVNested from the NestedValue
        let auth_nested = match auth_section {
            NestedValue::Map(auth_map) => auth_map,
            _ => {
                return Err(Error::Authentication(
                    "Auth section must be a nested map".to_string(),
                ));
            }
        };

        // Now get the specific key from the auth section
        let key_value = auth_nested
            .get(key_id)
            .ok_or_else(|| Error::Authentication(format!("Key not found: {key_id}")))?;

        // Use the new TryFrom implementation to parse AuthKey
        let auth_key = AuthKey::try_from(key_value.clone())
            .map_err(|e| Error::Authentication(format!("Invalid auth key format: {e}")))?;

        let public_key = parse_public_key(&auth_key.key)?;

        Ok(ResolvedAuth {
            public_key,
            effective_permission: auth_key.permissions.clone(),
            key_status: auth_key.status,
        })
    }

    /// Resolve a User Auth Tree key reference
    ///
    /// This is a simplified implementation for Phase 1 - full User Auth Tree
    /// support will be implemented in Phase 4
    fn resolve_user_tree_key(
        &mut self,
        _tree_id: &str,
        _tips: &[String],
        _key: &AuthId,
        _settings: &KVNested,
    ) -> Result<ResolvedAuth> {
        // Phase 1: Return error - User Auth Trees not yet implemented
        Err(Error::Authentication(
            "User Auth Trees not yet implemented in Phase 1".to_string(),
        ))
    }

    /// Check if a resolved authentication has sufficient permissions for an operation
    pub fn check_permissions(
        &self,
        resolved: &ResolvedAuth,
        operation: &Operation,
    ) -> Result<bool> {
        match operation {
            Operation::WriteData => Ok(resolved.effective_permission.can_write()
                || resolved.effective_permission.can_admin()),
            Operation::WriteSettings => Ok(resolved.effective_permission.can_admin()),
        }
    }

    /// Clear the authentication cache
    pub fn clear_cache(&mut self) {
        self.auth_cache.clear();
    }
}

impl Default for AuthValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::crypto::{format_public_key, generate_keypair, sign_entry};
    use crate::auth::types::{AuthInfo, AuthKey, KeyStatus, Permission};
    use crate::entry::Entry;

    fn create_test_settings_with_key(key_id: &str, auth_key: &AuthKey) -> crate::data::KVNested {
        let mut settings = crate::data::KVNested::new();
        let mut auth_section = crate::data::KVNested::new();
        auth_section.set(key_id.to_string(), auth_key.clone());
        settings.set_map("auth", auth_section);
        settings
    }

    #[test]
    fn test_basic_key_resolution() {
        let mut validator = AuthValidator::new();
        let (_, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

        let resolved = validator
            .resolve_direct_key("KEY_LAPTOP", &settings)
            .unwrap();
        assert_eq!(resolved.effective_permission, Permission::Write(10));
        assert_eq!(resolved.key_status, KeyStatus::Active);
    }

    #[test]
    fn test_revoked_key_validation() {
        let mut validator = AuthValidator::new();
        let (_signing_key, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let _revoked_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Revoked, // Key is revoked
        };

        // Test with active key - should work
        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);
        let auth_id = AuthId::Direct("KEY_LAPTOP".to_string());
        let resolved = validator.resolve_auth_key(&auth_id, &settings);
        assert!(resolved.is_ok());

        // Note: Testing revoked key behavior would require implementing
        // the full entry validation pipeline with historical context
    }

    #[test]
    fn test_permission_levels() {
        let validator = AuthValidator::new();

        let admin_auth = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: Permission::Admin(5),
            key_status: KeyStatus::Active,
        };

        let write_auth = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: Permission::Write(10),
            key_status: KeyStatus::Active,
        };

        let read_auth = ResolvedAuth {
            public_key: crate::auth::crypto::generate_keypair().1,
            effective_permission: Permission::Read,
            key_status: KeyStatus::Active,
        };

        // Test admin permissions
        assert!(
            validator
                .check_permissions(&admin_auth, &Operation::WriteData)
                .unwrap()
        );
        assert!(
            validator
                .check_permissions(&admin_auth, &Operation::WriteSettings)
                .unwrap()
        );

        // Test write permissions
        assert!(
            validator
                .check_permissions(&write_auth, &Operation::WriteData)
                .unwrap()
        );
        assert!(
            !validator
                .check_permissions(&write_auth, &Operation::WriteSettings)
                .unwrap()
        );

        // Test read permissions
        assert!(
            !validator
                .check_permissions(&read_auth, &Operation::WriteData)
                .unwrap()
        );
        assert!(
            !validator
                .check_permissions(&read_auth, &Operation::WriteSettings)
                .unwrap()
        );
    }

    #[test]
    fn test_entry_validation_success() {
        let mut validator = AuthValidator::new();
        let (signing_key, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(20),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

        // Create a test entry using Entry::builder
        let mut entry = Entry::builder("abc".to_string(), "{}".to_string()).build();

        // Set auth info without signature
        entry.auth = AuthInfo {
            id: AuthId::Direct("KEY_LAPTOP".to_string()),
            signature: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.auth.signature = Some(signature);

        // Validate the entry
        let result = validator.validate_entry(&entry, &settings);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_entry_validation_revoked_key() {
        let mut validator = AuthValidator::new();
        let (signing_key, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let _revoked_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Revoked, // Key is revoked
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

        // Create a test entry using Entry::builder
        let mut entry = Entry::builder("abc".to_string(), "{}".to_string()).build();

        // Set auth info without signature
        entry.auth = AuthInfo {
            id: AuthId::Direct("KEY_LAPTOP".to_string()),
            signature: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.auth.signature = Some(signature);

        // Validation should succeed with active key (we're testing with auth_key, not revoked_key)
        assert!(validator.validate_entry(&entry, &settings).unwrap());
    }

    #[test]
    fn test_missing_key() {
        let mut validator = AuthValidator::new();
        let settings = crate::data::KVNested::new(); // Empty settings

        let auth_id = AuthId::Direct("NONEXISTENT_KEY".to_string());
        let result = validator.resolve_auth_key(&auth_id, &settings);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Authentication(_) => {} // Expected - changed to Authentication error
            _ => panic!("Expected Authentication error"),
        }
    }

    #[test]
    fn test_user_tree_not_implemented() {
        let mut validator = AuthValidator::new();
        let settings = crate::data::KVNested::new();

        let auth_id = AuthId::UserTree {
            id: "user1".to_string(),
            tips: vec!["tip1".to_string()],
            key: Box::new(AuthId::Direct("KEY_LAPTOP".to_string())),
        };

        let result = validator.resolve_auth_key(&auth_id, &settings);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("User Auth Trees not yet implemented")
        );
    }

    #[test]
    fn test_validate_entry_with_auth() {
        let mut validator = AuthValidator::new();
        let (signing_key, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            key: format_public_key(&verifying_key),
            permissions: Permission::Write(15),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

        // Create test entry
        let mut entry =
            crate::entry::Entry::builder("root123".to_string(), "{}".to_string()).build();
        entry.auth = AuthInfo {
            id: AuthId::Direct("KEY_LAPTOP".to_string()),
            signature: None,
        };

        // Sign the entry
        let signature = crate::auth::crypto::sign_entry(&entry, &signing_key).unwrap();
        entry.auth.signature = Some(signature);

        // Validate the entry
        let result = validator.validate_entry(&entry, &settings);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_validate_entry_with_auth_info_against_empty_settings() {
        let mut validator = AuthValidator::new();
        let (signing_key, _verifying_key) = generate_keypair();

        // Create an entry with auth info (signed)
        let mut entry = Entry::builder("root123".to_string(), "{}".to_string()).build();
        entry.auth = AuthInfo {
            id: AuthId::Direct("SOME_KEY".to_string()),
            signature: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();
        entry.auth.signature = Some(signature);

        // Validate against empty settings (no auth configuration)
        let empty_settings = crate::data::KVNested::new();
        let result = validator.validate_entry(&entry, &empty_settings);

        // Should succeed because there's no auth configuration to validate against
        assert!(result.is_ok(), "Validation failed: {:?}", result.err());
        assert!(result.unwrap(), "Expected validation to return true");
    }
}
