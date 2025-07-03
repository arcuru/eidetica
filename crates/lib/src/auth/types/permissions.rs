//! Permission system for authentication
//!
//! This module defines the permission levels and operations for authentication.

use serde::{Deserialize, Serialize};

/// Permission levels for authenticated operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Permission {
    /// Full administrative access including settings and key management
    /// Priority may be used for conflict resolution, lower number = higher priority
    /// Admin keys always have priority over Write keys
    Admin(u32),
    /// Read and write access to data (excludes settings modifications)
    /// Priority may be used for conflict resolution, lower number = higher priority
    Write(u32),
    /// Read-only access to data
    Read,
}

impl PartialOrd for Permission {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Permission {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ordering_value().cmp(&other.ordering_value())
    }
}

impl Permission {
    /// Calculate ordering value for mathematical comparison
    /// Read = 0, Write(p) = 1 + u32::MAX - p, Admin(p) = 2 + (2 * u32::MAX) - p
    /// This ensures Admin permissions are always > Write permissions
    fn ordering_value(&self) -> u64 {
        match self {
            Permission::Read => 0,
            Permission::Write(p) => 1 + (u32::MAX as u64) - (*p as u64),
            Permission::Admin(p) => 2 + (2 * u32::MAX as u64) - (*p as u64),
        }
    }

    /// Get the priority level for permissions that have one
    pub fn priority(&self) -> Option<u32> {
        match self {
            Permission::Read => None,
            Permission::Write(priority) => Some(*priority),
            Permission::Admin(priority) => Some(*priority),
        }
    }

    /// Check if this permission allows writing data
    pub fn can_write(&self) -> bool {
        matches!(self, Permission::Write(_) | Permission::Admin(_))
    }

    /// Check if this permission allows administrative operations
    pub fn can_admin(&self) -> bool {
        matches!(self, Permission::Admin(_))
    }

    /// Clamp permissions to a maximum level
    ///
    /// Used for delegated tree delegation to ensure users cannot escalate
    /// their permissions beyond what was granted in the main tree.
    /// Returns the minimum of self and max_permission.
    pub fn clamp_to(&self, max_permission: &Permission) -> Permission {
        use std::cmp::min;
        min(self.clone(), max_permission.clone())
    }

    /// Clamp permissions within bounds (for delegated trees)
    ///
    /// Applies both minimum and maximum bounds from PermissionBounds.
    /// If min is specified and self is below min, raises to min.
    /// If self is above max, lowers to max.
    pub fn clamp_to_bounds(&self, bounds: &PermissionBounds) -> Permission {
        crate::auth::permission::clamp_permission(self.clone(), bounds)
    }
}

/// Status of an authentication key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyStatus {
    /// Key is active and can create new entries
    Active,
    /// Key is revoked - cannot create new entries, but historical entries are preserved
    /// Content of revoked entries is preserved during merges, but cannot be parents of new entries
    Revoked,
}

/// Permission bounds for delegated trees
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionBounds {
    /// Maximum permission level (required)
    pub max: Permission,
    /// Minimum permission level (optional)
    pub min: Option<Permission>,
}

impl Default for PermissionBounds {
    fn default() -> Self {
        Self {
            max: Permission::Read,
            min: None,
        }
    }
}

/// Operation types for permission checking
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Writing data to non-settings subtrees
    WriteData,
    /// Writing to _settings subtree (includes authentication modifications)
    WriteSettings,
}
