//! String conversion implementations for authentication types
//!
//! This module provides conversions between authentication types and strings
//! for serialization and human-readable representation.

use super::permissions::{KeyStatus, Permission};

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
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let parts = s.split(':').collect::<Vec<&str>>();
        match parts[0] {
            "read" => Ok(Permission::Read),
            "write" => {
                if parts.len() != 2 {
                    return Err("Write permission requires priority".to_string());
                }
                let priority = parts[1]
                    .parse::<u32>()
                    .map_err(|_| "Invalid priority value".to_string())?;
                Ok(Permission::Write(priority))
            }
            "admin" => {
                if parts.len() != 2 {
                    return Err("Admin permission requires priority".to_string());
                }
                let priority = parts[1]
                    .parse::<u32>()
                    .map_err(|_| "Invalid priority value".to_string())?;
                Ok(Permission::Admin(priority))
            }
            _ => Err(format!("Invalid permission string: {s}")),
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
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.as_str() {
            "active" => Ok(KeyStatus::Active),
            "revoked" => Ok(KeyStatus::Revoked),
            _ => Err(format!("Invalid key status: {s}")),
        }
    }
}
