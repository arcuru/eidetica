//! Authentication module for Eidetica
//!
//! This module provides cryptographic authentication, hierarchical permissions,
//! and User Authentication Trees while maintaining integration with the existing
//! CRDT and Merkle-DAG infrastructure.

pub mod crypto;
pub mod errors;
pub mod permission;
pub mod settings;
pub mod types;
pub mod validation;

// Re-export main types for easier access
pub use crypto::*;
pub use errors::AuthError;
pub use permission::*;
pub use settings::*;
pub use types::*;
pub use validation::AuthValidator;

#[cfg(test)]
/// Generate a valid, formatted Ed25519 public key for testing
///
/// Returns a public key string in the format "ed25519:<base64_url_unpadded_key>"
/// suitable for use in AuthKey constructors and other authentication tests.
///
/// This is a convenience function for tests that need a valid public key but
/// don't need the corresponding private key for signing operations.
pub fn generate_public_key() -> String {
    let (_, verifying_key) = generate_keypair();
    format_public_key(&verifying_key)
}
