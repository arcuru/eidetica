//! Permission checking for authentication operations
//!
//! This module provides utilities for checking if resolved authentication
//! has sufficient permissions for specific operations.

use crate::{
    Result,
    auth::types::{Operation, ResolvedAuth},
};

/// Check if a resolved authentication has sufficient permissions for an operation
pub fn check_permissions(resolved: &ResolvedAuth, operation: &Operation) -> Result<bool> {
    match operation {
        Operation::WriteData => {
            Ok(resolved.effective_permission.can_write()
                || resolved.effective_permission.can_admin())
        }
        Operation::WriteSettings => Ok(resolved.effective_permission.can_admin()),
    }
}
