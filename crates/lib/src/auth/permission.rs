//! Permission clamping logic for delegated trees
//!
//! This module provides functions for enforcing permission boundaries when
//! keys are delegated through other trees. The clamping ensures that delegated
//! keys cannot exceed their allowed permission levels.

use crate::auth::types::{Permission, PermissionBounds};

/// Clamp a delegated permission to fit within the specified bounds
///
/// This function enforces permission boundaries by:
/// 1. Applying the maximum bound (required) - reduces permission if it exceeds max
/// 2. Applying the minimum bound (optional) - increases permission if it's below min
///
/// # Arguments
/// * `delegated_permission` - The permission level from the delegated tree
/// * `bounds` - The permission bounds configured for this delegation
///
/// # Returns
/// The effective permission after applying bounds
///
/// # Examples
/// ```
/// use eidetica::auth::permission::clamp_permission;
/// use eidetica::auth::types::{Permission, PermissionBounds};
///
/// let bounds = PermissionBounds {
///     max: Permission::Write(10),
///     min: Some(Permission::Read),
/// };
///
/// // Admin permission gets clamped down to Write(10)
/// let clamped = clamp_permission(Permission::Admin(5), &bounds);
/// assert_eq!(clamped, Permission::Write(10));
///
/// // Read permission stays as Read (meets minimum)
/// let clamped = clamp_permission(Permission::Read, &bounds);
/// assert_eq!(clamped, Permission::Read);
/// ```
pub fn clamp_permission(delegated_permission: Permission, bounds: &PermissionBounds) -> Permission {
    // Validate bounds first
    if !validate_permission_bounds(bounds) {
        tracing::warn!(
            "Invalid permission bounds detected (min > max). Applying only max bound as fallback."
        );
        // For invalid bounds, apply only the max bound (safer fallback)
        if delegated_permission > bounds.max {
            return bounds.max;
        }
        return delegated_permission;
    }

    // Apply maximum bound (always enforced) - if permission exceeds max, clamp to max
    if delegated_permission > bounds.max {
        return bounds.max;
    }

    // Apply minimum bound if specified - only if permission is naturally below minimum
    if let Some(min) = &bounds.min
        && delegated_permission < *min
    {
        return *min;
    }

    delegated_permission
}

/// Validate that permission bounds are correctly configured
///
/// Ensures that:
/// - If minimum is specified, it's not greater than maximum
///
/// # Arguments
/// * `bounds` - The permission bounds to validate
///
/// # Returns
/// `true` if bounds are valid, `false` otherwise
pub fn validate_permission_bounds(bounds: &PermissionBounds) -> bool {
    // If minimum is specified, it must not exceed maximum
    if let Some(min) = &bounds.min {
        min <= &bounds.max
    } else {
        true // No minimum means bounds are valid
    }
}

/// Check if a delegating key has sufficient permission to set the given bounds
///
/// A key can only delegate permissions that are at or below its own level.
///
/// # Arguments
/// * `delegating_permission` - Permission level of the key setting up delegation
/// * `bounds` - The permission bounds being configured
///
/// # Returns
/// `true` if the delegating key can set these bounds, `false` otherwise
pub fn can_delegate_with_bounds(
    delegating_permission: &Permission,
    bounds: &PermissionBounds,
) -> bool {
    // Maximum bound cannot exceed delegating key's permission
    if bounds.max > *delegating_permission {
        return false;
    }
    validate_permission_bounds(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_permission_max_bound() {
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: None,
        };

        // Admin should be clamped down to Write(10)
        let result = clamp_permission(Permission::Admin(5), &bounds);
        assert_eq!(result, Permission::Write(10));

        // Write(20) should remain Write(20) (lower privilege than max)
        let result = clamp_permission(Permission::Write(20), &bounds);
        assert_eq!(result, Permission::Write(20));

        // Write(5) should be clamped to Write(10) (higher privilege than max)
        let result = clamp_permission(Permission::Write(5), &bounds);
        assert_eq!(result, Permission::Write(10));

        // Read should remain Read
        let result = clamp_permission(Permission::Read, &bounds);
        assert_eq!(result, Permission::Read);
    }

    #[test]
    fn test_clamp_permission_min_bound() {
        let bounds = PermissionBounds {
            max: Permission::Admin(5),
            min: Some(Permission::Write(10)),
        };

        // Admin(5) should remain Admin(5) (above minimum)
        let result = clamp_permission(Permission::Admin(5), &bounds);
        assert_eq!(result, Permission::Admin(5));

        // Write(15) should be raised to Write(10) (below minimum privilege)
        let result = clamp_permission(Permission::Write(15), &bounds);
        assert_eq!(result, Permission::Write(10));

        // Write(5) should remain Write(5) (above minimum, below max)
        let result = clamp_permission(Permission::Write(5), &bounds);
        assert_eq!(result, Permission::Write(5));

        // Read should be raised to Write(10) (minimum)
        let result = clamp_permission(Permission::Read, &bounds);
        assert_eq!(result, Permission::Write(10));
    }

    #[test]
    fn test_clamp_permission_both_bounds() {
        let bounds = PermissionBounds {
            max: Permission::Write(5),        // Higher privilege (lower number)
            min: Some(Permission::Write(15)), // Lower privilege (higher number)
        };

        // Admin should be clamped down to Write(5) (max allowed privilege)
        let result = clamp_permission(Permission::Admin(1), &bounds);
        assert_eq!(result, Permission::Write(5));

        // Write(20) should be raised to Write(15) (below minimum privilege)
        let result = clamp_permission(Permission::Write(20), &bounds);
        assert_eq!(result, Permission::Write(15));

        // Write(12) should remain Write(12) (within bounds)
        let result = clamp_permission(Permission::Write(12), &bounds);
        assert_eq!(result, Permission::Write(12));

        // Write(3) should be clamped to Write(5) (exceeds max, gets clamped to max)
        let result = clamp_permission(Permission::Write(3), &bounds);
        assert_eq!(result, Permission::Write(5));

        // Read should be raised to Write(15) (minimum)
        let result = clamp_permission(Permission::Read, &bounds);
        assert_eq!(result, Permission::Write(15));
    }

    #[test]
    fn test_validate_permission_bounds_valid() {
        // No minimum is always valid
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: None,
        };
        assert!(validate_permission_bounds(&bounds));

        // Minimum equal to maximum is valid
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Write(10)),
        };
        assert!(validate_permission_bounds(&bounds));

        // Minimum less than maximum is valid
        let bounds = PermissionBounds {
            max: Permission::Admin(5),
            min: Some(Permission::Write(10)),
        };
        assert!(validate_permission_bounds(&bounds));
    }

    #[test]
    fn test_validate_permission_bounds_invalid() {
        // Minimum greater than maximum is invalid
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Admin(5)),
        };
        assert!(!validate_permission_bounds(&bounds));

        // Read minimum with Write max is invalid (Read > Write in permission hierarchy)
        let bounds = PermissionBounds {
            max: Permission::Read,
            min: Some(Permission::Write(10)),
        };
        assert!(!validate_permission_bounds(&bounds));
    }

    #[test]
    fn test_can_delegate_with_bounds_valid() {
        let delegating_permission = Permission::Admin(5);

        // Can delegate any bounds within admin permission
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Read),
        };
        assert!(can_delegate_with_bounds(&delegating_permission, &bounds));

        // Can delegate up to admin level
        let bounds = PermissionBounds {
            max: Permission::Admin(5),
            min: None,
        };
        assert!(can_delegate_with_bounds(&delegating_permission, &bounds));
    }

    #[test]
    fn test_can_delegate_with_bounds_invalid() {
        let delegating_permission = Permission::Write(10);

        // Cannot delegate admin permission
        let bounds = PermissionBounds {
            max: Permission::Admin(5),
            min: None,
        };
        assert!(!can_delegate_with_bounds(&delegating_permission, &bounds));

        // Cannot set minimum above own permission
        let bounds = PermissionBounds {
            max: Permission::Write(20),
            min: Some(Permission::Admin(1)),
        };
        assert!(!can_delegate_with_bounds(&delegating_permission, &bounds));

        // Cannot set maximum above own permission
        let bounds = PermissionBounds {
            max: Permission::Write(5), // Write(5) > Write(10), so this should fail
            min: Some(Permission::Read),
        };
        assert!(!can_delegate_with_bounds(&delegating_permission, &bounds));
    }

    #[test]
    fn test_permission_clamping_edge_cases() {
        // Test with identical permissions
        let bounds = PermissionBounds {
            max: Permission::Write(10),
            min: Some(Permission::Write(10)),
        };

        let result = clamp_permission(Permission::Write(10), &bounds);
        assert_eq!(result, Permission::Write(10));

        let result = clamp_permission(Permission::Admin(5), &bounds);
        assert_eq!(result, Permission::Write(10));

        let result = clamp_permission(Permission::Read, &bounds);
        assert_eq!(result, Permission::Write(10));
    }

    #[test]
    fn test_permission_priority_clamping() {
        // Test that priority values are preserved when clamping within same permission level
        let bounds = PermissionBounds {
            max: Permission::Write(5),
            min: Some(Permission::Write(15)),
        };

        // Priority should be preserved when within bounds
        let result = clamp_permission(Permission::Write(12), &bounds);
        assert_eq!(result, Permission::Write(12));

        // Priority should be adjusted to min when below minimum
        let result = clamp_permission(Permission::Write(20), &bounds);
        assert_eq!(result, Permission::Write(15));

        // Priority should be adjusted to max when exceeding max
        let result = clamp_permission(Permission::Write(3), &bounds);
        assert_eq!(result, Permission::Write(5));
    }
}
