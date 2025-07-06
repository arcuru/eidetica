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
