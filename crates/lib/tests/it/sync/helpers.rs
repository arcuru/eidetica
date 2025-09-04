//! Helper functions for Sync testing
//!
//! This module provides utilities for testing Sync functionality including
//! setup operations, common test patterns, transport factories, and assertion helpers.

use eidetica::sync::handler::{SyncHandler, SyncHandlerImpl};
use eidetica::sync::peer_types::Address;
use eidetica::sync::transports::iroh::IrohTransport;
use eidetica::{Result, Instance, sync::Sync};
use iroh::RelayMode;
use std::sync::Arc;
use std::time::Duration;

// ===== SETUP HELPERS =====

/// Create a Instance Arc with authentication key
pub fn setup_db() -> Arc<Instance> {
    Arc::new(crate::helpers::setup_db())
}

/// Create a new Sync instance with standard setup
pub fn setup() -> (Arc<Instance>, Sync) {
    let base_db = setup_db();
    let sync = Sync::new(Arc::clone(base_db.backend())).expect("Failed to create Sync");
    (base_db, sync)
}

/// Create Instance with initialized sync module
pub fn setup_instance_with_initialized() -> Instance {
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

// ===== TRANSPORT TESTING HELPERS =====

/// Factory trait for setting up transport testing
///
/// This trait allows tests to work generically across different transport implementations
/// by abstracting transport creation, addressing, and configuration details.
///
/// # Examples
///
/// ```rust
/// use crate::sync::helpers::{TransportFactory, HttpTransportFactory};
///
/// async fn test_sync_with_any_transport<F: TransportFactory>(factory: F) {
///     let (db1, db2) = setup_databases().await?;
///     let sync1 = factory.create_sync(db1.backend().clone())?;
///     // ... rest of test
/// }
///
/// #[tokio::test]
/// async fn test_http_sync() {
///     test_sync_with_any_transport(HttpTransportFactory).await.unwrap();
/// }
/// ```
pub trait TransportFactory: Send + std::marker::Sync {
    /// Create a sync instance with this transport enabled
    fn create_sync(
        &self,
        backend: std::sync::Arc<dyn eidetica::backend::BackendDB>,
    ) -> Result<Sync>;

    /// Get the expected address format for this transport
    fn create_address(&self, server_addr: &str) -> Address;

    /// Get a display name for this transport type
    fn transport_name(&self) -> &'static str;

    /// Get appropriate wait time for this transport type during tests
    fn sync_wait_time(&self) -> Duration {
        if self.transport_name().contains("Iroh") {
            Duration::from_millis(3000) // Iroh needs more time for P2P connections
        } else {
            Duration::from_millis(1000) // HTTP is faster
        }
    }
}

/// Factory for HTTP transport instances
pub struct HttpTransportFactory;

impl TransportFactory for HttpTransportFactory {
    fn create_sync(
        &self,
        backend: std::sync::Arc<dyn eidetica::backend::BackendDB>,
    ) -> Result<Sync> {
        let mut sync = Sync::new(backend)?;
        sync.enable_http_transport()?;
        Ok(sync)
    }

    fn create_address(&self, server_addr: &str) -> Address {
        Address::http(server_addr)
    }

    fn transport_name(&self) -> &'static str {
        "HTTP"
    }
}

/// Factory for Iroh transport instances (relay disabled for fast local testing)
pub struct IrohTransportFactory;

impl TransportFactory for IrohTransportFactory {
    fn create_sync(
        &self,
        backend: std::sync::Arc<dyn eidetica::backend::BackendDB>,
    ) -> Result<Sync> {
        let mut sync = Sync::new(backend)?;
        let transport = IrohTransport::builder()
            .relay_mode(RelayMode::Disabled)
            .build()?;
        sync.add_transport(Box::new(transport))?;
        Ok(sync)
    }

    fn create_address(&self, server_addr: &str) -> Address {
        Address::iroh(server_addr)
    }

    fn transport_name(&self) -> &'static str {
        "Iroh (No Relays)"
    }
}
