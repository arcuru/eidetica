//! Test context for managing test setup and lifecycle.
//!
//! Provides a composable `TestContext` that explicitly shows what each test needs
//! while ensuring proper lifetime management.
//!
//! Right now this calls out to the test helper functions, but that
//! relationship will flip in future changes.

use eidetica::{Database, Instance};

use crate::helpers::test_instance_with_user;

/// Test context for managing Instance and Database lifecycle.
///
/// The Instance must be kept alive for the Database to work (Database holds
/// a weak reference to Instance).
///
/// Use the builder methods to set up what the test needs:
/// - `TestContext::new()` - creates context
/// - `.with_database()` - adds a database
pub struct TestContext {
    // Instance must be kept alive for Database to work
    _instance: Option<Instance>,
    database: Option<Database>,
}

impl TestContext {
    /// Create a new test context.
    pub fn new() -> Self {
        Self {
            _instance: None,
            database: None,
        }
    }

    /// Add a database (creates a test user internally).
    pub fn with_database(self) -> Self {
        let (instance, mut user) = test_instance_with_user("test_user");
        let default_key = user.get_default_key().expect("Failed to get default key");

        let mut settings = eidetica::crdt::Doc::new();
        settings.set("name", "test_database");

        let database = user
            .create_database(settings, &default_key)
            .expect("Failed to create database");

        Self {
            _instance: Some(instance),
            database: Some(database),
        }
    }

    /// Get a reference to the database (panics if not set).
    pub fn database(&self) -> &Database {
        self.database
            .as_ref()
            .expect("database not set - use with_database()")
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}
