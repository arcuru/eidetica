//! Helper functions for Sync testing
//!
//! This module provides utilities for testing Sync functionality including
//! setup operations, common test patterns, and assertion helpers.

use eidetica::sync::handler::{SyncHandler, SyncHandlerImpl};
use eidetica::{basedb::BaseDB, sync::Sync};
use std::sync::Arc;

// ===== SETUP HELPERS =====

/// Create a BaseDB Arc with authentication key
pub fn setup_db() -> Arc<BaseDB> {
    Arc::new(crate::helpers::setup_db())
}

/// Create a new Sync instance with standard setup
pub fn setup() -> (Arc<BaseDB>, Sync) {
    let base_db = setup_db();
    let sync = Sync::new(Arc::clone(base_db.backend())).expect("Failed to create Sync");
    (base_db, sync)
}

/// Create BaseDB with initialized sync module
pub fn setup_basedb_with_initialized() -> BaseDB {
    let base_db = crate::helpers::setup_db();
    base_db.with_sync().expect("Failed to initialize sync")
}

/// Create a test SyncHandler for transport-specific tests
pub fn setup_test_handler() -> Arc<dyn SyncHandler> {
    let base_db = setup_db();
    Arc::new(SyncHandlerImpl::new(
        base_db.backend().clone(),
        "_device_key",
    ))
}

/// Test helper function for backward compatibility with existing tests.
/// Creates a SyncHandlerImpl from a Sync instance and delegates to it.
pub async fn handle_request(
    sync: &Sync,
    request: &eidetica::sync::protocol::SyncRequest,
) -> eidetica::sync::protocol::SyncResponse {
    let handler = SyncHandlerImpl::new(sync.backend().clone(), "_device_key");
    handler.handle_request(request).await
}

// ===== ASSERTION HELPERS =====

/// Assert that a setting has the expected value
pub fn assert_setting(sync: &Sync, key: &str, expected_value: &str) {
    let actual_value = sync.get_setting(key).expect("Failed to get setting");
    assert_eq!(actual_value, Some(expected_value.to_string()));
}

/// Assert that a setting does not exist
pub fn assert_setting_not_found(sync: &Sync, key: &str) {
    let actual_value = sync.get_setting(key).expect("Failed to get setting");
    assert_eq!(actual_value, None);
}

/// Assert that two sync instances refer to the same tree
pub fn assert_trees_equal(sync1: &Sync, sync2: &Sync) {
    assert_eq!(sync1.sync_tree_root_id(), sync2.sync_tree_root_id());
}

// ===== OPERATION HELPERS =====

/// Set multiple settings on a sync instance
pub fn set_multiple_settings(sync: &mut Sync, settings: &[(&str, &str)]) {
    for (key, value) in settings {
        sync.set_setting(*key, *value)
            .unwrap_or_else(|_| panic!("Failed to set setting: {key} = {value}"));
    }
}

/// Assert multiple settings have expected values
pub fn assert_multiple_settings(sync: &Sync, expected: &[(&str, &str)]) {
    for (key, expected_value) in expected {
        assert_setting(sync, key, expected_value);
    }
}
