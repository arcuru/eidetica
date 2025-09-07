//! Authentication type definitions for Eidetica
//!
//! This module contains all the core types used for authentication,
//! organized into logical submodules for better maintainability.

pub mod conversions;
pub mod delegation;
pub mod keys;
pub mod permissions;

#[cfg(test)]
mod tests;

// Re-export all types for backward compatibility
// Re-export string conversions when needed
#[allow(unused_imports)]
pub use conversions::*;
pub use delegation::*;
pub use keys::*;
pub use permissions::*;
