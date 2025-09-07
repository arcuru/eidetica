//! Delegation system types for authentication
//!
//! This module defines types related to tree delegation and reference management.

use serde::{Deserialize, Serialize};

use super::permissions::PermissionBounds;
use crate::entry::ID;

/// Reference to a Merkle-DAG tree (for delegated trees)
/// TODO: May standardize on this format across the codebase instead of just for delegation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TreeReference {
    /// Root entry ID of the referenced tree
    pub root: ID,
    /// Current tip entry IDs of the referenced tree
    pub tips: Vec<ID>,
}

/// Delegated tree reference stored in main tree's _settings.auth
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DelegatedTreeRef {
    /// Permission bounds for keys from this delegated tree
    #[serde(rename = "permission-bounds")]
    pub permission_bounds: PermissionBounds,
    /// Reference to the delegated tree
    pub tree: TreeReference,
}
