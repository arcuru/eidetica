//! Delegation path resolution for authentication
//!
//! This module handles the complex logic of resolving delegation paths,
//! including multi-tree traversal and permission clamping.

use crate::Result;
use crate::auth::crypto::parse_public_key;
use crate::auth::errors::AuthError;
use crate::auth::permission::clamp_permission;
use crate::auth::types::{
    AuthKey, DelegatedTreeRef, DelegationStep, PermissionBounds, ResolvedAuth,
};
use crate::backend::Database;
use crate::crdt::{Nested, NodeValue};
use crate::entry::ID;
use crate::tree::Tree;
use std::sync::Arc;

/// Delegation resolver for handling complex delegation paths
pub struct DelegationResolver;

impl DelegationResolver {
    /// Create a new delegation resolver
    pub fn new() -> Self {
        Self
    }

    /// Resolve delegation path using flat list structure
    ///
    /// This iteratively processes each step in the delegation path,
    /// applying permission clamping at each level.
    pub fn resolve_delegation_path_with_depth(
        &mut self,
        steps: &[DelegationStep],
        settings: &Nested,
        backend: &Arc<dyn Database>,
        _depth: usize,
    ) -> Result<ResolvedAuth> {
        if steps.is_empty() {
            return Err(AuthError::EmptyDelegationPath.into());
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
                    return Err(AuthError::InvalidDelegationStep {
                        reason: "Final delegation step must not have tips".to_string(),
                    }
                    .into());
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
                    return Err(AuthError::InvalidDelegationStep {
                        reason: "Non-final delegation step must have tips".to_string(),
                    }
                    .into());
                }

                let tips = step.tips.as_ref().unwrap();

                // Get the delegated tree reference
                let delegated_tree_ref =
                    self.get_delegated_tree_ref(&step.key, &current_settings)?;

                // Load the delegated tree
                let root_id = delegated_tree_ref.tree.root.clone();
                let delegated_tree =
                    Tree::new_from_id(root_id.clone(), Arc::clone(&current_backend)).map_err(
                        |e| AuthError::DelegatedTreeLoadFailed {
                            tree_id: root_id.to_string(),
                            source: Box::new(e),
                        },
                    )?;

                // Validate tips
                let current_tips = current_backend.get_tips(&root_id).map_err(|e| {
                    AuthError::InvalidAuthConfiguration {
                        reason: format!(
                            "Failed to get current tips for delegated tree '{root_id}': {e}"
                        ),
                    }
                })?;

                let tips_valid =
                    self.validate_tip_ancestry(tips, &current_tips, &current_backend)?;
                if !tips_valid {
                    return Err(AuthError::InvalidDelegationTips {
                        tree_id: root_id.to_string(),
                        claimed_tips: tips.clone(),
                    }
                    .into());
                }

                // Get delegated tree's settings
                let delegated_settings_kvstore = delegated_tree.get_settings().map_err(|e| {
                    AuthError::InvalidAuthConfiguration {
                        reason: format!("Failed to get delegated tree settings: {e}"),
                    }
                })?;
                current_settings = delegated_settings_kvstore.get_all().map_err(|e| {
                    AuthError::InvalidAuthConfiguration {
                        reason: format!("Failed to get delegated tree settings data: {e}"),
                    }
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
                    cumulative_bounds = Some(PermissionBounds {
                        max: new_max,
                        min: new_min,
                    });
                } else {
                    cumulative_bounds = Some(delegated_tree_ref.permission_bounds);
                }
            }
        }

        // This should never be reached due to the final step handling above
        Err(AuthError::InvalidDelegationStep {
            reason: "Invalid delegation path structure".to_string(),
        }
        .into())
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
            .ok_or_else(|| AuthError::NoAuthConfiguration)?;

        let auth_nested = match auth_section {
            NodeValue::Node(auth_map) => auth_map,
            _ => {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "Auth section must be a nested map".to_string(),
                }
                .into());
            }
        };

        // Parse the delegated tree reference
        Ok(auth_nested
            .get_json::<DelegatedTreeRef>(tree_ref_id)
            .map_err(|e| AuthError::InvalidAuthConfiguration {
                reason: format!("Invalid delegated tree reference format: {e}"),
            })?)
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
        backend: &Arc<dyn Database>,
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

    /// Resolve a direct key reference from the main tree's auth settings
    fn resolve_direct_key(&self, key_id: &str, settings: &Nested) -> Result<ResolvedAuth> {
        // First get the auth section from settings
        let auth_section = settings
            .get("auth")
            .ok_or_else(|| AuthError::NoAuthConfiguration)?;

        // Extract the auth Nested from the Value
        let auth_nested = match auth_section {
            NodeValue::Node(auth_map) => auth_map,
            _ => {
                return Err(AuthError::InvalidAuthConfiguration {
                    reason: "Auth section must be a nested map".to_string(),
                }
                .into());
            }
        };

        // Use get_json to parse AuthKey
        let auth_key = auth_nested.get_json::<AuthKey>(key_id).map_err(|e| {
            AuthError::InvalidAuthConfiguration {
                reason: format!("Invalid auth key format: {e}"),
            }
        })?;

        let public_key = parse_public_key(&auth_key.pubkey)?;

        Ok(ResolvedAuth {
            public_key,
            effective_permission: auth_key.permissions.clone(),
            key_status: auth_key.status,
        })
    }
}

impl Default for DelegationResolver {
    fn default() -> Self {
        Self::new()
    }
}
