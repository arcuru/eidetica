//! Delegation path resolution for authentication
//!
//! This module handles the complex logic of resolving delegation paths,
//! including multi-tree traversal and permission clamping.

use std::sync::Arc;

use crate::{
    Database, Instance, Result,
    auth::{
        errors::AuthError,
        permission::clamp_permission,
        settings::AuthSettings,
        types::{DelegationStep, KeyHint, PermissionBounds, ResolvedAuth},
    },
    backend::BackendImpl,
    entry::ID,
};

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
    /// applying permission clamping at each level. The final hint
    /// is resolved in the last delegated tree's auth settings.
    ///
    /// Returns all matching ResolvedAuth entries. For name hints that match
    /// multiple keys at the final step, all matches are returned with the
    /// same permission clamping applied to each.
    pub async fn resolve_delegation_path_with_depth(
        &mut self,
        steps: &[DelegationStep],
        final_hint: &KeyHint,
        auth_settings: &AuthSettings,
        instance: &Instance,
        _depth: usize,
    ) -> Result<Vec<ResolvedAuth>> {
        if steps.is_empty() {
            return Err(AuthError::EmptyDelegationPath.into());
        }

        // Validate no global hints in delegation (must resolve to concrete key)
        if final_hint.is_global() {
            return Err(AuthError::InvalidDelegationStep {
                reason: "Delegation paths cannot use global '*' hint".to_string(),
            }
            .into());
        }

        // Iterate through delegation steps
        let mut current_auth_settings = auth_settings.clone();
        let current_backend = Arc::clone(instance.backend().as_arc_backend_impl());
        let mut cumulative_bounds: Option<PermissionBounds> = None;

        // Process all delegation steps (tree traversal)
        for step in steps {
            // Load delegated tree (step.tree contains the root ID as a string)
            let delegated_tree_ref = current_auth_settings.get_delegated_tree_by_str(&step.tree)?;

            let root_id = delegated_tree_ref.tree.root.clone();
            let delegated_tree = Database::open_unauthenticated(root_id.clone(), instance)
                .map_err(|e| AuthError::DelegatedTreeLoadFailed {
                    tree_id: root_id.to_string(),
                    source: Box::new(e),
                })?;

            // Validate tips
            let current_tips = current_backend.get_tips(&root_id).await.map_err(|e| {
                AuthError::InvalidAuthConfiguration {
                    reason: format!(
                        "Failed to get current tips for delegated tree '{root_id}': {e}"
                    ),
                }
            })?;

            let tips_valid = self
                .validate_tip_ancestry(&step.tips, &current_tips, &current_backend)
                .await?;
            if !tips_valid {
                return Err(AuthError::InvalidDelegationTips {
                    tree_id: root_id.to_string(),
                    claimed_tips: step.tips.clone(),
                }
                .into());
            }

            // Get delegated tree's auth settings
            let delegated_settings = delegated_tree.get_settings().await.map_err(|e| {
                AuthError::InvalidAuthConfiguration {
                    reason: format!("Failed to get delegated tree settings: {e}"),
                }
            })?;
            current_auth_settings = delegated_settings.auth_snapshot().await.map_err(|e| {
                AuthError::InvalidAuthConfiguration {
                    reason: format!("Failed to get delegated tree auth settings: {e}"),
                }
            })?;

            // Accumulate permission bounds
            cumulative_bounds = Some(match cumulative_bounds {
                Some(existing_bounds) => {
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
                    PermissionBounds {
                        max: new_max,
                        min: new_min,
                    }
                }
                None => delegated_tree_ref.permission_bounds.clone(),
            });
        }

        // After traversing all steps, resolve the final hint in the last tree's auth settings
        let mut matches = current_auth_settings.resolve_hint(final_hint)?;
        if matches.is_empty() {
            return Err(AuthError::KeyNotFound {
                key_name: format!("hint({:?})", final_hint.hint_type()),
            }
            .into());
        }

        // Apply accumulated permission bounds to all matches
        if let Some(bounds) = cumulative_bounds {
            for resolved in &mut matches {
                resolved.effective_permission =
                    clamp_permission(resolved.effective_permission.clone(), &bounds);
            }
        }

        Ok(matches)
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
    async fn validate_tip_ancestry(
        &self,
        claimed_tips: &[ID],
        current_tips: &[ID],
        backend: &Arc<dyn BackendImpl>,
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
                if backend.get(claimed_tip).await.is_ok() {
                    is_valid = true;
                }
            }

            if !is_valid {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

impl Default for DelegationResolver {
    fn default() -> Self {
        Self::new()
    }
}
