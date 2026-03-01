//! Wire-format error type for the service protocol.
//!
//! `ServiceError` carries enough information to reconstruct an appropriate
//! `crate::Error` on the client side without requiring the full error type
//! hierarchy to be serializable.

use serde::{Deserialize, Serialize};

use crate::backend::BackendError;
use crate::entry::ID;
use crate::instance::InstanceError;

/// Wire-format error for the service protocol.
///
/// Captures the originating module, the discriminant name, and the Display message
/// of a `crate::Error` so the client can reconstruct an appropriate error variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceError {
    /// The originating module (from `Error::module()`)
    pub module: String,
    /// The discriminant name (e.g. "EntryNotFound")
    pub kind: String,
    /// The Display message
    pub message: String,
}

impl From<&crate::Error> for ServiceError {
    fn from(err: &crate::Error) -> Self {
        let module = err.module().to_string();
        let kind = error_kind_name(err);
        let message = err.to_string();
        ServiceError {
            module,
            kind,
            message,
        }
    }
}

/// Reconstruct a `crate::Error` from a `ServiceError`.
///
/// Matches on module + kind to produce the most specific error variant possible.
/// Falls back to a generic IO error with the original message for unrecognized
/// combinations.
pub fn service_error_to_eidetica_error(err: ServiceError) -> crate::Error {
    match (err.module.as_str(), err.kind.as_str()) {
        ("backend", "EntryNotFound") => BackendError::EntryNotFound {
            id: extract_id_from_message(&err.message).unwrap_or_default(),
        }
        .into(),
        ("backend", "VerificationStatusNotFound") => BackendError::VerificationStatusNotFound {
            id: extract_id_from_message(&err.message).unwrap_or_else(|| ID::from("")),
        }
        .into(),
        ("backend", "EntryNotInTree") => BackendError::EntryNotInTree {
            entry_id: ID::default(),
            tree_id: ID::default(),
        }
        .into(),
        ("backend", "NoCommonAncestor") => {
            BackendError::NoCommonAncestor { entry_ids: vec![] }.into()
        }
        ("backend", "EmptyEntryList") => BackendError::EmptyEntryList {
            operation: err.message.clone(),
        }
        .into(),
        ("instance", "DatabaseNotFound") => InstanceError::DatabaseNotFound {
            name: err.message.clone(),
        }
        .into(),
        ("instance", "EntryNotFound") => InstanceError::EntryNotFound {
            entry_id: extract_id_from_message(&err.message).unwrap_or_default(),
        }
        .into(),
        ("instance", "InstanceAlreadyExists") => InstanceError::InstanceAlreadyExists.into(),
        ("instance", "DeviceKeyNotFound") => InstanceError::DeviceKeyNotFound.into(),
        ("instance", "AuthenticationRequired") => InstanceError::AuthenticationRequired.into(),
        _ => {
            // Fall back to an IO error carrying the original message
            crate::Error::Io(std::io::Error::other(format!(
                "[{}::{}] {}",
                err.module, err.kind, err.message
            )))
        }
    }
}

/// Extract an ID from an error message like "Entry not found: <id>".
fn extract_id_from_message(message: &str) -> Option<ID> {
    message
        .rsplit(": ")
        .next()
        .and_then(|s| ID::parse(s.trim()).ok())
}

/// Get the discriminant name of an error variant.
fn error_kind_name(err: &crate::Error) -> String {
    match err {
        crate::Error::Io(_) => "Io".to_string(),
        crate::Error::Serialize(_) => "Serialize".to_string(),
        crate::Error::Auth(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Backend(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Instance(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::CRDT(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Store(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Transaction(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Sync(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Entry(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::Id(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
        crate::Error::User(e) => format!("{e:?}")
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or_else(|| format!("{e:?}"), |(name, _)| name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_error_from_backend_not_found() {
        let test_id = ID::from_bytes("abc123");
        let err = crate::Error::Backend(Box::new(BackendError::EntryNotFound { id: test_id }));
        let se = ServiceError::from(&err);
        assert_eq!(se.module, "backend");
        assert_eq!(se.kind, "EntryNotFound");
        assert!(se.message.contains("abc123") || se.message.contains("Entry not found"));
    }

    #[test]
    fn test_service_error_from_instance_error() {
        let err = crate::Error::Instance(Box::new(InstanceError::DeviceKeyNotFound));
        let se = ServiceError::from(&err);
        assert_eq!(se.module, "instance");
        assert_eq!(se.kind, "DeviceKeyNotFound");
    }

    #[test]
    fn test_roundtrip_backend_entry_not_found() {
        let original = crate::Error::Backend(Box::new(BackendError::EntryNotFound {
            id: ID::from_bytes("test-id"),
        }));
        let se = ServiceError::from(&original);
        let reconstructed = service_error_to_eidetica_error(se);
        assert!(reconstructed.is_not_found());
    }

    #[test]
    fn test_roundtrip_instance_already_exists() {
        let original = crate::Error::Instance(Box::new(InstanceError::InstanceAlreadyExists));
        let se = ServiceError::from(&original);
        let reconstructed = service_error_to_eidetica_error(se);
        assert!(reconstructed.is_conflict());
    }

    #[test]
    fn test_unknown_error_falls_back_to_io() {
        let se = ServiceError {
            module: "unknown".to_string(),
            kind: "SomethingWeird".to_string(),
            message: "something happened".to_string(),
        };
        let err = service_error_to_eidetica_error(se);
        assert!(err.is_io_error());
    }

    #[test]
    fn test_service_error_serde_roundtrip() {
        let se = ServiceError {
            module: "backend".to_string(),
            kind: "EntryNotFound".to_string(),
            message: "Entry not found: test123".to_string(),
        };
        let json = serde_json::to_string(&se).unwrap();
        let deserialized: ServiceError = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.module, se.module);
        assert_eq!(deserialized.kind, se.kind);
        assert_eq!(deserialized.message, se.message);
    }
}
