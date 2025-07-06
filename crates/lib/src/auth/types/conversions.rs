//! String conversion implementations for authentication types
//!
//! This module provides conversions between authentication types and strings
//! for serialization and human-readable representation.

use super::permissions::{KeyStatus, Permission};
use crate::auth::errors::AuthError;

impl From<Permission> for String {
    fn from(permission: Permission) -> Self {
        match permission {
            Permission::Read => "read".to_string(),
            Permission::Write(priority) => format!("write:{priority}"),
            Permission::Admin(priority) => format!("admin:{priority}"),
        }
    }
}

impl TryFrom<String> for Permission {
    type Error = AuthError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let parts = s.split(':').collect::<Vec<&str>>();
        match parts[0] {
            "read" => Ok(Permission::Read),
            "write" => {
                if parts.len() != 2 {
                    return Err(AuthError::PermissionRequiresPriority {
                        permission_type: "Write".to_string(),
                    });
                }
                let priority =
                    parts[1]
                        .parse::<u32>()
                        .map_err(|_| AuthError::InvalidPriorityValue {
                            value: parts[1].to_string(),
                        })?;
                Ok(Permission::Write(priority))
            }
            "admin" => {
                if parts.len() != 2 {
                    return Err(AuthError::PermissionRequiresPriority {
                        permission_type: "Admin".to_string(),
                    });
                }
                let priority =
                    parts[1]
                        .parse::<u32>()
                        .map_err(|_| AuthError::InvalidPriorityValue {
                            value: parts[1].to_string(),
                        })?;
                Ok(Permission::Admin(priority))
            }
            _ => Err(AuthError::InvalidPermissionString { value: s }),
        }
    }
}

impl From<KeyStatus> for String {
    fn from(status: KeyStatus) -> Self {
        match status {
            KeyStatus::Active => "active".to_string(),
            KeyStatus::Revoked => "revoked".to_string(),
        }
    }
}

impl TryFrom<String> for KeyStatus {
    type Error = AuthError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.as_str() {
            "active" => Ok(KeyStatus::Active),
            "revoked" => Ok(KeyStatus::Revoked),
            _ => Err(AuthError::InvalidKeyStatus { value: s }),
        }
    }
}
