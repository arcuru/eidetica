//! Authentication validation for Eidetica
//!
//! This module provides validation logic for authentication information,
//! including key resolution, permission checking, and signature verification.

pub mod delegation;
pub mod entry;
pub(crate) mod floors;
pub mod permissions;
pub mod resolver;

#[cfg(test)]
mod tests;

// Re-export the main validator
pub use entry::AuthValidator;

#[cfg(test)]
mod floor_tests;
