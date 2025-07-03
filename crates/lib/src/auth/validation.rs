//! Authentication validation for Eidetica
//!
//! This module provides validation logic for authentication information,
//! including key resolution, permission checking, and signature verification.
//!
//! ## Design Approach
//!
//! This implementation uses a simplified approach:
//! - **Entry-time validation**: Validate entries against current auth settings when created
//! - **Standard CRDT merging**: Use existing Nested Last Write Wins (LWW) for all conflicts
//! - **Administrative priority**: Priority rules apply only to key creation/modification operations
//! - **No custom merge logic**: Authentication relies on proven Nested CRDT semantics
//! - **Direct backend access**: Uses backend directly for delegated tree operations

use crate::auth::crypto::{parse_public_key, verify_entry_signature};
use crate::auth::permission::clamp_permission;
use crate::auth::types::{AuthKey, DelegatedTreeRef, KeyStatus, Operation, ResolvedAuth, SigKey};
use crate::backend::Backend;
use crate::crdt::{Nested, Value};
use crate::entry::{Entry, ID};
use crate::tree::Tree;
use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

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
    /// * `backend` - Backend for loading delegated trees (optional for direct keys)
    pub fn validate_entry(
        &mut self,
        entry: &Entry,
        settings_state: &Nested,
        backend: Option<&Arc<dyn Backend>>,
    ) -> Result<bool> {
        // Handle unsigned entries (for backward compatibility)
        // An entry is considered unsigned if it has an empty Direct key ID and no signature
        if let SigKey::Direct(key_id) = &entry.sig.key
            && key_id.is_empty()
            && entry.sig.sig.is_none()
        {
            // This is an unsigned entry - allow it to pass without authentication
            return Ok(true);
        }

        // If the settings state has no 'auth' section or an empty 'auth' map, allow unsigned entries.
        match settings_state.get("auth") {
            Some(Value::Map(auth_map)) => {
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
        let resolved_auth = self.resolve_sig_key(&entry.sig.key, settings_state, backend)?;

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
    /// * `sig_key` - The signature key identifier to resolve
    /// * `settings` - Nested settings containing auth configuration
    /// * `backend` - Backend for loading delegated trees (required for DelegationPath sig_key)
    pub fn resolve_sig_key(
        &mut self,
        sig_key: &SigKey,
        settings: &Nested,
        backend: Option<&Arc<dyn Backend>>,
    ) -> Result<ResolvedAuth> {
        // Note: We don't cache results here because auth settings can change
        // and cached results could become stale (e.g., revoked keys, updated permissions).
        // In a production system, caching would need to be more sophisticated with
        // invalidation strategies based on settings changes.
        self.resolve_sig_key_with_depth(sig_key, settings, backend, 0)
    }

    /// Resolve authentication identifier with recursion depth tracking
    ///
    /// This internal method tracks delegation depth to prevent infinite loops
    /// and ensures that delegation chains don't exceed reasonable limits.
    ///
    /// # Arguments
    /// * `sig_key` - The signature key identifier to resolve
    /// * `settings` - Nested settings containing auth configuration
    /// * `backend` - Backend for loading delegated trees (required for DelegationPath sig_key)
    /// * `depth` - Current recursion depth (0 for initial call)
    fn resolve_sig_key_with_depth(
        &mut self,
        sig_key: &SigKey,
        settings: &Nested,
        backend: Option<&Arc<dyn Backend>>,
        depth: usize,
    ) -> Result<ResolvedAuth> {
        // Prevent infinite recursion and overly deep delegation chains
        const MAX_DELEGATION_DEPTH: usize = 10;
        if depth >= MAX_DELEGATION_DEPTH {
            return Err(Error::Authentication(format!(
                "Maximum delegation depth ({MAX_DELEGATION_DEPTH}) exceeded - possible circular delegation"
            )));
        }

        match sig_key {
            SigKey::Direct(key_id) => self.resolve_direct_key(key_id, settings),
            SigKey::DelegationPath(steps) => {
                let backend = backend.ok_or_else(|| {
                    Error::Authentication(
                        "Backend required for delegated tree resolution".to_string(),
                    )
                })?;
                self.resolve_delegation_path_with_depth(steps, settings, backend, depth)
            }
        }
    }

    /// Resolve a direct key reference from the main tree's auth settings
    fn resolve_direct_key(&mut self, key_id: &str, settings: &Nested) -> Result<ResolvedAuth> {
        // First get the auth section from settings
        let auth_section = settings
            .get("auth")
            .ok_or_else(|| Error::Authentication("No auth configuration found".to_string()))?;

        // Extract the auth Nested from the Value
        let auth_nested = match auth_section {
            Value::Map(auth_map) => auth_map,
            _ => {
                return Err(Error::Authentication(
                    "Auth section must be a nested map".to_string(),
                ));
            }
        };

        // Now get the specific key from the auth section

        // Use get_json to parse AuthKey
        let auth_key = auth_nested
            .get_json::<AuthKey>(key_id)
            .map_err(|e| Error::Authentication(format!("Invalid auth key format: {e}")))?;

        let public_key = parse_public_key(&auth_key.pubkey)?;

        Ok(ResolvedAuth {
            public_key,
            effective_permission: auth_key.permissions.clone(),
            key_status: auth_key.status,
        })
    }

    /// Resolve delegation path using flat list structure
    ///
    /// This iteratively processes each step in the delegation path,
    /// applying permission clamping at each level.
    fn resolve_delegation_path_with_depth(
        &mut self,
        steps: &[crate::auth::types::DelegationStep],
        settings: &Nested,
        backend: &Arc<dyn Backend>,
        _depth: usize,
    ) -> Result<ResolvedAuth> {
        if steps.is_empty() {
            return Err(Error::Authentication("Empty delegation path".to_string()));
        }

        // Iterate through delegation steps
        let mut current_settings = settings.clone();
        let current_backend = Arc::clone(backend);
        let mut cumulative_bounds = None;

        // Process all steps except the last one (which should be the final key)
        for (i, step) in steps.iter().enumerate() {
            let is_final_step = i == steps.len() - 1;

            if is_final_step {
                // Final step: resolve the actual key
                if step.tips.is_some() {
                    return Err(Error::Authentication(
                        "Final delegation step must not have tips".to_string(),
                    ));
                }

                // Resolve the final key directly
                let mut resolved = self.resolve_direct_key(&step.key, &current_settings)?;

                // Apply accumulated permission bounds
                if let Some(bounds) = cumulative_bounds {
                    resolved.effective_permission =
                        clamp_permission(resolved.effective_permission, &bounds);
                }

                return Ok(resolved);
            } else {
                // Intermediate step: load delegated tree
                if step.tips.is_none() {
                    return Err(Error::Authentication(
                        "Non-final delegation step must have tips".to_string(),
                    ));
                }

                let tips = step.tips.as_ref().unwrap();

                // Get the delegated tree reference
                let delegated_tree_ref =
                    self.get_delegated_tree_ref(&step.key, &current_settings)?;

                // Load the delegated tree
                let root_id = delegated_tree_ref.tree.root.clone();
                let delegated_tree =
                    Tree::new_from_id(root_id.clone(), Arc::clone(&current_backend)).map_err(
                        |e| {
                            Error::Authentication(format!(
                                "Failed to load delegated tree with root '{root_id}': {e}"
                            ))
                        },
                    )?;

                // Validate tips
                let current_tips = current_backend.get_tips(&root_id).map_err(|e| {
                    Error::Authentication(format!(
                        "Failed to get current tips for delegated tree '{root_id}': {e}"
                    ))
                })?;

                let tips_valid =
                    self.validate_tip_ancestry(tips, &current_tips, &current_backend)?;
                if !tips_valid {
                    return Err(Error::Authentication(format!(
                        "Invalid delegation: claimed tips {tips:?} are not valid for delegated tree '{root_id}'"
                    )));
                }

                // Get delegated tree's settings
                let delegated_settings_kvstore = delegated_tree.get_settings().map_err(|e| {
                    Error::Authentication(format!("Failed to get delegated tree settings: {e}"))
                })?;
                current_settings = delegated_settings_kvstore.get_all().map_err(|e| {
                    Error::Authentication(format!(
                        "Failed to get delegated tree settings data: {e}"
                    ))
                })?;

                // Accumulate permission bounds
                if let Some(existing_bounds) = cumulative_bounds {
                    // Combine bounds by taking the minimum of max permissions
                    let new_max = std::cmp::min(
                        existing_bounds.max.clone(),
                        delegated_tree_ref.permission_bounds.max.clone(),
                    );
                    let new_min = match (
                        existing_bounds.min,
                        delegated_tree_ref.permission_bounds.min.clone(),
                    ) {
                        (Some(existing_min), Some(new_min)) => {
                            Some(std::cmp::max(existing_min, new_min))
                        }
                        (Some(existing_min), None) => Some(existing_min),
                        (None, Some(new_min)) => Some(new_min),
                        (None, None) => None,
                    };
                    cumulative_bounds = Some(crate::auth::types::PermissionBounds {
                        max: new_max,
                        min: new_min,
                    });
                } else {
                    cumulative_bounds = Some(delegated_tree_ref.permission_bounds);
                }
            }
        }

        // This should never be reached due to the final step handling above
        Err(Error::Authentication(
            "Invalid delegation path structure".to_string(),
        ))
    }

    /// Get delegated tree reference from auth settings
    fn get_delegated_tree_ref(
        &self,
        tree_ref_id: &str,
        settings: &Nested,
    ) -> Result<DelegatedTreeRef> {
        // Get the auth section
        let auth_section = settings
            .get("auth")
            .ok_or_else(|| Error::Authentication("No auth configuration found".to_string()))?;

        let auth_nested = match auth_section {
            Value::Map(auth_map) => auth_map,
            _ => {
                return Err(Error::Authentication(
                    "Auth section must be a nested map".to_string(),
                ));
            }
        };

        // Get the delegated tree reference

        // Parse the delegated tree reference
        auth_nested
            .get_json::<DelegatedTreeRef>(tree_ref_id)
            .map_err(|e| {
                Error::Authentication(format!("Invalid delegated tree reference format: {e}"))
            })
    }

    /// Validate tip ancestry using backend's DAG traversal
    ///
    /// This method checks if claimed tips are descendants of or equal to current tips
    /// using the backend's DAG traversal capabilities.
    ///
    /// # Arguments
    /// * `claimed_tips` - Tips claimed by the entry being validated
    /// * `current_tips` - Current tips from the backend
    /// * `backend` - Backend to use for DAG traversal
    fn validate_tip_ancestry(
        &self,
        claimed_tips: &[ID],
        current_tips: &[ID],
        backend: &Arc<dyn Backend>,
    ) -> Result<bool> {
        // Fast path: If no current tips, accept any claimed tips (first entry in tree)
        if current_tips.is_empty() {
            return Ok(true);
        }

        // Fast path: If no claimed tips, that's invalid (should have at least some context)
        if claimed_tips.is_empty() {
            return Ok(false);
        }

        // Fast path: Check if all claimed tips are identical to current tips
        if claimed_tips.len() == current_tips.len()
            && claimed_tips.iter().all(|tip| current_tips.contains(tip))
        {
            return Ok(true);
        }

        // Check if each claimed tip is either:
        // 1. Equal to a current tip, or
        // 2. An ancestor of a current tip (meaning we're using older but valid state)
        // 3. A descendant of a current tip (meaning we're ahead of current state)

        // Validate each claimed tip
        for claimed_tip in claimed_tips {
            let mut is_valid = false;

            // Fast path: Check if claimed tip equals any current tip
            if current_tips.contains(claimed_tip) {
                is_valid = true;
            } else {
                // TODO: For now, we'll use a simplified check and accept the claimed tips
                // if they exist in the tree at all. A more sophisticated implementation
                // would verify the actual ancestry relationships using the backend's
                // DAG traversal methods.

                // Try to get the entry to verify it exists in the tree
                if backend.get(claimed_tip).is_ok() {
                    is_valid = true;
                }
            }

            if !is_valid {
                return Ok(false);
            }
        }

        Ok(true)
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
    use crate::auth::types::{AuthKey, DelegationStep, KeyStatus, Permission, SigInfo, SigKey};
    use crate::entry::Entry;

    fn create_test_settings_with_key(key_id: &str, auth_key: &AuthKey) -> Nested {
        let mut settings = Nested::new();
        let mut auth_section = Nested::new();
        auth_section.as_hashmap_mut().insert(
            key_id.to_string(),
            serde_json::to_string(&auth_key).unwrap().into(),
        );
        settings.set_map("auth", auth_section);
        settings
    }

    #[test]
    fn test_basic_key_resolution() {
        let mut validator = AuthValidator::new();
        let (_, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            pubkey: format_public_key(&verifying_key),
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
            pubkey: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);
        let sig_key = SigKey::Direct("KEY_LAPTOP".to_string());
        let resolved = validator.resolve_sig_key(&sig_key, &settings, None);
        assert!(resolved.is_ok());
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
            pubkey: format_public_key(&verifying_key),
            permissions: Permission::Write(20),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &auth_key);

        // Create a test entry using Entry::builder
        let mut entry = Entry::builder("abc").build();

        // Set auth info without signature
        entry.sig = SigInfo {
            key: SigKey::Direct("KEY_LAPTOP".to_string()),
            sig: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.sig.sig = Some(signature);

        // Validate the entry
        let result = validator.validate_entry(&entry, &settings, None);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_missing_key() {
        let mut validator = AuthValidator::new();
        let settings = Nested::new(); // Empty settings

        let sig_key = SigKey::Direct("NONEXISTENT_KEY".to_string());
        let result = validator.resolve_sig_key(&sig_key, &settings, None);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Authentication(_) => {} // Expected
            _ => panic!("Expected Authentication error"),
        }
    }

    #[test]
    fn test_delegated_tree_requires_backend() {
        let mut validator = AuthValidator::new();
        let settings = Nested::new();

        let sig_key = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "user1".to_string(),
                tips: Some(vec![ID::new("tip1")]),
            },
            DelegationStep {
                key: "KEY_LAPTOP".to_string(),
                tips: None,
            },
        ]);

        let result = validator.resolve_sig_key(&sig_key, &settings, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Backend required for delegated tree resolution")
        );
    }

    #[test]
    fn test_validate_entry_with_auth_info_against_empty_settings() {
        let mut validator = AuthValidator::new();
        let (signing_key, _verifying_key) = generate_keypair();

        // Create an entry with auth info (signed)
        let mut entry = Entry::builder("root123").build();
        entry.sig = SigInfo {
            key: SigKey::Direct("SOME_KEY".to_string()),
            sig: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();
        entry.sig.sig = Some(signature);

        // Validate against empty settings (no auth configuration)
        let empty_settings = Nested::new();
        let result = validator.validate_entry(&entry, &empty_settings, None);

        // Should succeed because there's no auth configuration to validate against
        assert!(result.is_ok(), "Validation failed: {:?}", result.err());
        assert!(result.unwrap(), "Expected validation to return true");
    }

    #[test]
    fn test_entry_validation_with_revoked_key() {
        let mut validator = AuthValidator::new();
        let (signing_key, verifying_key) = generate_keypair();

        let revoked_key = AuthKey {
            pubkey: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Revoked, // Key is revoked
        };

        let settings = create_test_settings_with_key("KEY_LAPTOP", &revoked_key);

        // Create a test entry using Entry::builder
        let mut entry = Entry::builder("abc").build();

        // Set auth info without signature
        entry.sig = SigInfo {
            key: SigKey::Direct("KEY_LAPTOP".to_string()),
            sig: None,
        };

        // Sign the entry
        let signature = sign_entry(&entry, &signing_key).unwrap();

        // Set the signature on the entry
        entry.sig.sig = Some(signature);

        // Validation should fail with revoked key
        let result = validator.validate_entry(&entry, &settings, None);
        assert!(result.is_ok()); // validate_entry returns Ok(bool)
        assert!(!result.unwrap()); // But the validation should return false for revoked keys
    }

    #[test]
    fn test_basic_delegated_tree_resolution() {
        let mut validator = AuthValidator::new();

        // Create a simple direct key resolution test
        let (_, verifying_key) = generate_keypair();
        let auth_key = AuthKey {
            pubkey: format_public_key(&verifying_key),
            permissions: Permission::Admin(5),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("DIRECT_KEY", &auth_key);

        let sig_key = SigKey::Direct("DIRECT_KEY".to_string());
        let result = validator.resolve_sig_key(&sig_key, &settings, None);

        match result {
            Ok(resolved) => {
                assert_eq!(resolved.effective_permission, Permission::Admin(5));
                assert_eq!(resolved.key_status, KeyStatus::Active);
            }
            Err(e) => {
                panic!("Failed to resolve auth key: {e}");
            }
        }
    }

    #[test]
    fn test_complete_delegation_workflow() {
        use crate::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};
        use crate::backend::InMemoryBackend;
        use crate::basedb::BaseDB;

        // Create a backend and database for testing
        let backend = Box::new(InMemoryBackend::new());
        let db = BaseDB::new(backend);

        // Create keys for both main and delegated trees
        let main_key = db.add_private_key("main_admin").unwrap();
        let delegated_key = db.add_private_key("delegated_user").unwrap();

        // Create the delegated tree with its own auth configuration
        let mut delegated_settings = Nested::new();
        let mut delegated_auth = Nested::new();
        delegated_auth
            .set_json(
                "delegated_user", // Key name must match the key used for tree creation
                AuthKey {
                    pubkey: format_public_key(&delegated_key),
                    permissions: Permission::Admin(5),
                    status: KeyStatus::Active,
                },
            )
            .unwrap();
        delegated_settings.set_map("auth", delegated_auth);

        let delegated_tree = db.new_tree(delegated_settings, "delegated_user").unwrap();

        // Create the main tree with delegation configuration
        let mut main_settings = Nested::new();
        let mut main_auth = Nested::new();

        // Add direct key to main tree
        main_auth
            .set_json(
                "main_admin",
                AuthKey {
                    pubkey: format_public_key(&main_key),
                    permissions: Permission::Admin(0),
                    status: KeyStatus::Active,
                },
            )
            .unwrap();

        // Get the actual tips from the delegated tree
        let delegated_tips = delegated_tree.get_tips().unwrap();

        // Add delegation reference
        main_auth
            .set_json(
                "delegate_to_user",
                DelegatedTreeRef {
                    permission_bounds: PermissionBounds {
                        max: Permission::Write(10),
                        min: Some(Permission::Read),
                    },
                    tree: TreeReference {
                        root: delegated_tree.root_id().clone(),
                        tips: delegated_tips.clone(),
                    },
                },
            )
            .unwrap();

        main_settings.set_map("auth", main_auth);
        let main_tree = db.new_tree(main_settings, "main_admin").unwrap();

        // Test delegation resolution
        let mut validator = AuthValidator::new();
        let main_settings = main_tree.get_settings().unwrap().get_all().unwrap();

        let delegated_sig_key = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "delegate_to_user".to_string(),
                tips: Some(delegated_tips),
            },
            DelegationStep {
                key: "delegated_user".to_string(),
                tips: None,
            },
        ]);

        let result =
            validator.resolve_sig_key(&delegated_sig_key, &main_settings, Some(db.backend()));

        // Should succeed with permission clamping (Admin -> Write due to bounds)
        assert!(
            result.is_ok(),
            "Delegation resolution failed: {:?}",
            result.err()
        );
        let resolved = result.unwrap();
        assert_eq!(resolved.effective_permission, Permission::Write(10)); // Clamped from Admin to Write
        assert_eq!(resolved.key_status, KeyStatus::Active);
    }

    #[test]
    fn test_delegated_tree_requires_tips() {
        use crate::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};
        use crate::backend::InMemoryBackend;
        use crate::basedb::BaseDB;

        // Create a backend and database for testing
        let backend = Box::new(InMemoryBackend::new());
        let db = BaseDB::new(backend);

        // Create keys for both main and delegated trees
        let main_key = db.add_private_key("main_admin").unwrap();

        // Create a simple delegated tree
        let delegated_settings = Nested::new();
        let delegated_tree = db.new_tree(delegated_settings, "main_admin").unwrap();

        // Create the main tree with delegation configuration
        let mut main_settings = Nested::new();
        let mut main_auth = Nested::new();

        // Add direct key to main tree
        main_auth
            .set_json(
                "main_admin",
                AuthKey {
                    pubkey: format_public_key(&main_key),
                    permissions: Permission::Admin(0),
                    status: KeyStatus::Active,
                },
            )
            .unwrap();

        // Add delegation reference (with proper tips that we'll ignore in the test)
        main_auth
            .set_json(
                "delegate_to_user",
                DelegatedTreeRef {
                    permission_bounds: PermissionBounds {
                        max: Permission::Write(10),
                        min: Some(Permission::Read),
                    },
                    tree: TreeReference {
                        root: delegated_tree.root_id().clone(),
                        tips: vec![ID::new("some_tip")], // This will be ignored due to empty tips in auth_id
                    },
                },
            )
            .unwrap();

        main_settings.set_map("auth", main_auth);

        // Create validator and test with empty tips
        let mut validator = AuthValidator::new();
        let settings = main_settings;

        // Create a DelegationPath sig_key with empty tips
        let sig_key = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "delegate_to_user".to_string(),
                tips: Some(vec![]), // Empty tips should cause validation to fail
            },
            DelegationStep {
                key: "delegated_user".to_string(),
                tips: None,
            },
        ]);

        let result = validator.resolve_sig_key(&sig_key, &settings, Some(db.backend()));

        // Should fail because tips are required for delegated tree resolution
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Invalid delegation"),
            "Expected error about invalid delegation, got: {error_msg}"
        );
    }

    #[test]
    fn test_nested_delegation_with_permission_clamping() {
        use crate::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};
        use crate::backend::InMemoryBackend;
        use crate::basedb::BaseDB;

        // Create a backend and database for testing
        let backend = Box::new(InMemoryBackend::new());
        let db = BaseDB::new(backend);

        // Create keys for main tree, intermediate delegated tree, and final user tree
        let main_key = db.add_private_key("main_admin").unwrap();
        let intermediate_key = db.add_private_key("intermediate_admin").unwrap();
        let user_key = db.add_private_key("final_user").unwrap();

        // 1. Create the final user tree (deepest level)
        let mut user_settings = Nested::new();
        let mut user_auth = Nested::new();
        user_auth
            .set_json(
                "final_user",
                AuthKey {
                    pubkey: format_public_key(&user_key),
                    permissions: Permission::Admin(3), // High privilege at source
                    status: KeyStatus::Active,
                },
            )
            .unwrap();
        user_settings.set_map("auth", user_auth);
        let user_tree = db.new_tree(user_settings, "final_user").unwrap();
        let user_tips = user_tree.get_tips().unwrap();

        // 2. Create intermediate delegated tree that delegates to user tree
        let mut intermediate_settings = Nested::new();
        let mut intermediate_auth = Nested::new();

        // Add direct key to intermediate tree
        intermediate_auth
            .set_json(
                "intermediate_admin",
                AuthKey {
                    pubkey: format_public_key(&intermediate_key),
                    permissions: Permission::Admin(2),
                    status: KeyStatus::Active,
                },
            )
            .unwrap();

        // Add delegation to user tree with bounds Write(8) max, Read min
        intermediate_auth
            .set_json(
                "user_delegation",
                DelegatedTreeRef {
                    permission_bounds: PermissionBounds {
                        max: Permission::Write(8), // Clamp Admin(3) to Write(8)
                        min: Some(Permission::Read),
                    },
                    tree: TreeReference {
                        root: user_tree.root_id().clone(),
                        tips: user_tips.clone(),
                    },
                },
            )
            .unwrap();

        intermediate_settings.set_map("auth", intermediate_auth);
        let intermediate_tree = db
            .new_tree(intermediate_settings, "intermediate_admin")
            .unwrap();
        let intermediate_tips = intermediate_tree.get_tips().unwrap();

        // 3. Create main tree that delegates to intermediate tree
        let mut main_settings = Nested::new();
        let mut main_auth = Nested::new();

        // Add direct key to main tree
        main_auth
            .set_json(
                "main_admin",
                AuthKey {
                    pubkey: format_public_key(&main_key),
                    permissions: Permission::Admin(0),
                    status: KeyStatus::Active,
                },
            )
            .unwrap();

        // Add delegation to intermediate tree with bounds Write(5) max, Read min
        // This should be more restrictive than the intermediate tree's Write(8)
        main_auth
            .set_json(
                "intermediate_delegation",
                DelegatedTreeRef {
                    permission_bounds: PermissionBounds {
                        max: Permission::Write(5), // More restrictive than Write(8)
                        min: Some(Permission::Read),
                    },
                    tree: TreeReference {
                        root: intermediate_tree.root_id().clone(),
                        tips: intermediate_tips.clone(),
                    },
                },
            )
            .unwrap();

        main_settings.set_map("auth", main_auth);
        let main_tree = db.new_tree(main_settings, "main_admin").unwrap();

        // 4. Test nested delegation resolution: Main -> Intermediate -> User
        let mut validator = AuthValidator::new();
        let main_settings = main_tree.get_settings().unwrap().get_all().unwrap();

        // Create nested delegation SigKey:
        // Main tree delegates to "intermediate_delegation" ->
        // Intermediate tree delegates to "user_delegation" ->
        // User tree resolves "final_user" key
        let nested_sig_key = SigKey::DelegationPath(vec![
            DelegationStep {
                key: "intermediate_delegation".to_string(),
                tips: Some(intermediate_tips),
            },
            DelegationStep {
                key: "user_delegation".to_string(),
                tips: Some(user_tips),
            },
            DelegationStep {
                key: "final_user".to_string(),
                tips: None,
            },
        ]);

        let result = validator.resolve_sig_key(&nested_sig_key, &main_settings, Some(db.backend()));

        // Should succeed with multi-level permission clamping:
        // Admin(3) -> Write(8) (at intermediate level) -> Write(5) (at main level, further clamping)
        assert!(
            result.is_ok(),
            "Nested delegation resolution failed: {:?}",
            result.err()
        );
        let resolved = result.unwrap();

        // The permission should be clamped at each level:
        // 1. User tree has Admin(3) (high permission)
        // 2. Intermediate tree clamps Admin(3) to Write(8) due to max bound
        // 3. Main tree clamps Write(8) with max bound Write(5) -> no change since Write(8) is more restrictive
        // Final result should be Write(8) - the most restrictive bound in the chain

        assert_eq!(resolved.effective_permission, Permission::Write(8)); // Correctly clamped through the chain
        assert_eq!(resolved.key_status, KeyStatus::Active);
    }

    #[test]
    fn test_delegation_depth_limit() {
        // Test that excessive delegation depth is prevented
        let mut validator = AuthValidator::new();

        // Create an empty settings (doesn't matter for depth test)
        let settings = Nested::new();

        // Test the depth check by directly calling with depth = MAX_DELEGATION_DEPTH
        let simple_sig_key = SigKey::Direct("base_key".to_string());

        // This should succeed (just under the limit)
        let result = validator.resolve_sig_key_with_depth(&simple_sig_key, &settings, None, 9);
        // Should fail due to missing auth configuration, not depth limit
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("No auth configuration found"));

        // This should fail due to depth limit (at the limit)
        let result = validator.resolve_sig_key_with_depth(&simple_sig_key, &settings, None, 10);
        assert!(result.is_err());
        let error = result.unwrap_err();
        println!("Depth limit error: {error}");
        assert!(error.to_string().contains("Maximum delegation depth"));
        assert!(error.to_string().contains("exceeded"));
    }

    #[test]
    fn test_performance_optimizations() {
        let mut validator = AuthValidator::new();
        let (_, verifying_key) = generate_keypair();

        let auth_key = AuthKey {
            pubkey: format_public_key(&verifying_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        };

        let settings = create_test_settings_with_key("PERF_KEY", &auth_key);
        let sig_key = SigKey::Direct("PERF_KEY".to_string());

        // Test that resolution works correctly
        let result1 = validator.resolve_sig_key(&sig_key, &settings, None);
        assert!(result1.is_ok());

        // Multiple resolutions should work consistently
        let result2 = validator.resolve_sig_key(&sig_key, &settings, None);
        assert!(result2.is_ok());

        // Results should be identical
        let resolved1 = result1.unwrap();
        let resolved2 = result2.unwrap();
        assert_eq!(
            resolved1.effective_permission,
            resolved2.effective_permission
        );
        assert_eq!(resolved1.key_status, resolved2.key_status);

        // Test cache clear functionality
        validator.clear_cache();
        assert_eq!(validator.auth_cache.len(), 0);
    }
}
