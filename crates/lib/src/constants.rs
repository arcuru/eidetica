//! Constants used throughout the Eidetica library.
//!
//! This module provides central definitions for internal strings and other constants
//! used within the library, especially for reserved subtree names.

/// Reserved subtree name for storing tree settings.
pub const SETTINGS: &str = "_settings";

/// Reserved subtree name for marking root entries.
pub const ROOT: &str = "_root";

/// System database name for Instance configuration and management
pub const INSTANCE: &str = "_instance";

/// System database name for user directory
pub const USERS: &str = "_users";

/// System database name for database tracking
pub const DATABASES: &str = "_databases";

/// Global permission key for universal access.
pub const GLOBAL_PERMISSION_KEY: &str = "*";
