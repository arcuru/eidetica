//! User system for Eidetica
//!
//! Provides multi-user support with password-based authentication,
//! encrypted key storage, and per-user database preferences.

pub mod crypto;
pub mod errors;
pub mod key_manager;
pub mod system_databases;
pub mod types;
pub mod user_session;

pub use errors::UserError;
pub use key_manager::UserKeyManager;
pub use types::*;
pub use user_session::User;
