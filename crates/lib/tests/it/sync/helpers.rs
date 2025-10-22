//! Helper functions for Sync testing
//!
//! This module provides utilities for testing Sync functionality including
//! setup operations, common test patterns, transport factories, and assertion helpers.

use std::{sync::Arc, time::Duration};

use eidetica::{
    Instance, Result,
    sync::{
        Sync,
        handler::{SyncHandler, SyncHandlerImpl},
        peer_types::Address,
        transports::iroh::IrohTransport,
    },
};
use iroh::RelayMode;

// ===== SETUP HELPERS =====

/// Create an Instance with authentication key
pub fn setup_db() -> Instance {
    let (instance, _user) = crate::helpers::setup_db();
    instance
}

/// Create a new Sync instance with standard setup
pub fn setup() -> (Instance, Sync) {
    let base_db = setup_db();
    let sync = Sync::new(base_db.clone()).expect("Failed to create Sync");
    (base_db, sync)
}

/// Create Instance with initialized sync module
pub fn setup_instance_with_initialized() -> Instance {
    let (instance, _user) = crate::helpers::setup_db();
    instance.enable_sync().expect("Failed to initialize sync");
    instance
}

/// Create a test SyncHandler for transport-specific tests
///
/// Returns (Instance, Handler) where the Instance must be kept alive
/// for the handler's WeakInstance to remain valid.
pub fn setup_test_handler() -> (Instance, Arc<dyn SyncHandler>) {
    let base_db = setup_db();
    let sync = Sync::new(base_db.clone()).expect("Failed to create Sync");
    let handler = Arc::new(SyncHandlerImpl::new(
        base_db.clone(),
        sync.sync_tree_root_id().clone(),
    ));
    (base_db, handler)
}

/// Test helper function for backward compatibility with existing tests.
/// Creates a SyncHandlerImpl from a Sync instance and delegates to it.
pub async fn handle_request(
    sync: &Sync,
    request: &eidetica::sync::protocol::SyncRequest,
) -> eidetica::sync::protocol::SyncResponse {
    let handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );
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
pub fn set_multiple_settings(sync: &Sync, settings: &[(&str, &str)]) {
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
    fn create_sync(&self, instance: Instance) -> Result<Sync>;

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
    fn create_sync(&self, instance: Instance) -> Result<Sync> {
        let sync = Sync::new(instance)?;
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
    fn create_sync(&self, instance: Instance) -> Result<Sync> {
        let sync = Sync::new(instance)?;
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

// ===== BOOTSTRAP TESTING HELPERS =====

use eidetica::{
    Database, Entry,
    auth::Permission as AuthPermission,
    crdt::Doc,
    sync::protocol::{SyncRequest, SyncResponse, SyncTreeRequest},
};

/// Create a server instance configured for bootstrap testing
///
/// # Arguments
/// * `auto_approve` - Whether to enable bootstrap auto-approval
///
/// # Returns
/// (Instance, Database, Sync, tree_id)
#[allow(deprecated)]
pub fn setup_bootstrap_server(
    auto_approve: bool,
) -> (Instance, Database, Sync, eidetica::entry::ID) {
    let instance = crate::helpers::test_instance();

    // Add server admin key
    instance
        .add_private_key("server_admin")
        .expect("Failed to add server admin key");

    // Create database with appropriate bootstrap policy
    let mut settings = Doc::new();
    settings.set_string("name", "Bootstrap Test Database");

    // Configure auth policy
    let mut auth_doc = Doc::new();
    let mut policy_doc = Doc::new();
    policy_doc
        .set_json("bootstrap_auto_approve", auto_approve)
        .expect("Failed to set policy");
    auth_doc.set_doc("policy", policy_doc);

    // Add server admin key to auth settings
    let server_pubkey = instance
        .get_formatted_public_key("server_admin")
        .expect("Failed to get server public key");

    auth_doc
        .set_json(
            "server_admin",
            serde_json::json!({
                "pubkey": server_pubkey,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set server admin auth");

    // Add device key to auth settings for sync handler operations
    // The sync handler needs this key to authenticate when operating on the database
    let device_pubkey = instance
        .get_formatted_public_key("_device_key")
        .expect("Failed to get device public key");

    auth_doc
        .set_json(
            "_device_key",
            serde_json::json!({
                "pubkey": device_pubkey,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set device key auth");

    settings.set_doc("auth", auth_doc);

    let database = instance
        .new_database(settings, "server_admin")
        .expect("Failed to create database");
    let tree_id = database.root_id().clone();

    // Create sync instance
    let sync = Sync::new(instance.clone()).expect("Failed to create sync");

    (instance, database, sync, tree_id)
}

/// Create a server with manual approval (auto_approve = false)
pub fn setup_manual_approval_server() -> (Instance, Database, Sync, eidetica::entry::ID) {
    setup_bootstrap_server(false)
}

/// Create a server with auto approval (auto_approve = true)
pub fn setup_auto_approval_server() -> (Instance, Database, Sync, eidetica::entry::ID) {
    setup_bootstrap_server(true)
}

/// Start a sync server with common settings
pub async fn start_sync_server(sync: &Sync) -> String {
    sync.enable_http_transport()
        .expect("Failed to enable HTTP transport");
    sync.start_server_async("127.0.0.1:0")
        .await
        .expect("Failed to start server");
    sync.get_server_address_async()
        .await
        .expect("Failed to get server address")
}

/// Create a client instance for bootstrap testing
///
/// # Arguments
/// * `key_name` - Name for the client's private key
///
/// # Returns
/// (Instance, Sync)
#[allow(deprecated)]
pub fn setup_bootstrap_client(key_name: &str) -> (Instance, Sync) {
    let instance = crate::helpers::test_instance();

    instance
        .add_private_key(key_name)
        .expect("Failed to add client key");

    let sync = Sync::new(instance.clone()).expect("Failed to create sync");

    (instance, sync)
}

/// Create a simple client with default key name
pub fn setup_simple_client() -> (Instance, Sync) {
    setup_bootstrap_client("client_key")
}

/// Create a SyncTreeRequest for bootstrap testing
pub fn create_bootstrap_request(
    tree_id: &eidetica::entry::ID,
    requesting_key: &str,
    key_name: &str,
    permission: AuthPermission,
) -> SyncRequest {
    SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some(requesting_key.to_string()),
        requesting_key_name: Some(key_name.to_string()),
        requested_permission: Some(permission),
    })
}

/// Create and submit a bootstrap request, returning the request ID
pub async fn create_pending_bootstrap_request(
    handler: &SyncHandlerImpl,
    tree_id: &eidetica::entry::ID,
    requesting_key: &str,
    key_name: &str,
    permission: AuthPermission,
) -> String {
    let request = create_bootstrap_request(tree_id, requesting_key, key_name, permission);
    let response = handler.handle_request(&request).await;

    match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    }
}

/// Approve a bootstrap request with error handling
pub fn approve_request(sync: &Sync, request_id: &str, approver_key: &str) -> Result<()> {
    sync.approve_bootstrap_request(request_id, approver_key)
}

/// Create a standard test tree entry
pub fn create_test_tree_entry() -> Entry {
    Entry::root_builder()
        .set_subtree_data(
            "messages",
            r#"{"msg1": {"text": "Hello from test!", "timestamp": 1234567890}}"#,
        )
        .build()
        .expect("Failed to build test entry")
}

/// Assert that a response is BootstrapPending
pub fn assert_bootstrap_pending(response: &SyncResponse) -> &str {
    match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    }
}

/// Assert that sync has expected number of pending requests
pub fn assert_request_stored(sync: &Sync, expected_count: usize) {
    let pending_requests = sync
        .pending_bootstrap_requests()
        .expect("Failed to list pending requests");
    assert_eq!(
        pending_requests.len(),
        expected_count,
        "Expected {} pending requests, found {}",
        expected_count,
        pending_requests.len()
    );
}

/// Create a sync handler for testing
pub fn create_test_sync_handler(sync: &Sync) -> SyncHandlerImpl {
    SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    )
}

// ===== USER API HELPERS =====

use eidetica::user::User;

/// Creates a server instance with a user, key, and bootstrap-enabled database
///
/// Returns (Instance, User, key_id: String, Database, TreeId)
pub fn setup_server_with_bootstrap_database(
    username: &str,
    key_name: &str,
    db_name: &str,
) -> (Instance, User, String, Database, eidetica::entry::ID) {
    let server_instance = setup_instance_with_initialized();
    server_instance.create_user(username, None).unwrap();
    let mut server_user = server_instance.login_user(username, None).unwrap();
    let server_key_id = server_user.add_private_key(Some(key_name)).unwrap();

    let mut settings = Doc::new();
    settings.set_string("name", db_name);

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Add bootstrap auto-approval policy
    set_bootstrap_auto_approve(&server_database, true).unwrap();

    (
        server_instance,
        server_user,
        server_key_id,
        server_database,
        tree_id,
    )
}

/// Creates a client instance with an indexed user and key (for concurrent test scenarios)
///
/// Creates a user with name format "client_user_{index}" and key "client_key_{index}"
/// Returns (Instance, User, key_id: String)
pub fn setup_indexed_client(index: usize) -> (Instance, User, String) {
    let client_instance = setup_instance_with_initialized();
    let client_username = format!("client_user_{}", index);
    client_instance.create_user(&client_username, None).unwrap();
    let mut client_user = client_instance.login_user(&client_username, None).unwrap();

    let client_key_display_name = format!("client_key_{}", index);
    let client_key_id = client_user
        .add_private_key(Some(&client_key_display_name))
        .unwrap();

    (client_instance, client_user, client_key_id)
}

/// Requests database access and establishes the database-key mapping
///
/// This helper combines the common pattern of:
/// 1. Enabling HTTP transport
/// 2. Requesting database access
/// 3. Waiting for sync completion
/// 4. Adding the database-key mapping
pub async fn request_and_map_database_access(
    instance: &mut Instance,
    user: &mut User,
    server_addr: &str,
    tree_id: &eidetica::entry::ID,
    key_id: &str,
    permission: AuthPermission,
    sync_delay_ms: u64,
) -> Result<()> {
    {
        let client_sync = instance.sync().expect("Sync not initialized");
        client_sync.enable_http_transport()?;

        user.request_database_access(&client_sync, server_addr, tree_id, key_id, permission)
            .await?;
    } // Drop Arc before sleep

    // Wait for sync to complete
    tokio::time::sleep(Duration::from_millis(sync_delay_ms)).await;

    // Establish database-key mapping
    // Use the same key_id as the SigKey identifier (common pattern)
    user.map_key(key_id, tree_id, key_id)?;

    Ok(())
}

/// Simplified version with default sync delay (100ms) and Write(5) permission
pub async fn request_database_access_default(
    instance: &mut Instance,
    user: &mut User,
    server_addr: &str,
    tree_id: &eidetica::entry::ID,
    key_id: &str,
) -> Result<()> {
    request_and_map_database_access(
        instance,
        user,
        server_addr,
        tree_id,
        key_id,
        AuthPermission::Write(5),
        100,
    )
    .await
}

/// Creates a user with a key and an empty database
///
/// Returns (User, key_id: String, Database, TreeId)
pub fn setup_user_with_database(
    instance: &Instance,
    username: &str,
    key_name: &str,
) -> (User, String, Database, eidetica::entry::ID) {
    instance.create_user(username, None).unwrap();
    let mut user = instance.login_user(username, None).unwrap();
    let key_id = user.add_private_key(Some(key_name)).unwrap();

    let database = user.create_database(Doc::new(), &key_id).unwrap();
    let tree_id = database.root_id().clone();

    (user, key_id, database, tree_id)
}

/// Creates a sync handler for testing bootstrap policy resolution
///
/// Uses a temporary sync instance to get the sync tree root ID.
pub fn create_database_sync_handler(_database: &Database, instance: &Instance) -> SyncHandlerImpl {
    let temp_sync = Sync::new(instance.clone()).unwrap();
    SyncHandlerImpl::new(instance.clone(), temp_sync.sync_tree_root_id().clone())
}

/// Sets the bootstrap auto-approval policy on a database
pub fn set_bootstrap_auto_approve(database: &Database, enabled: bool) -> Result<()> {
    let tx = database.new_transaction()?;
    let db_settings = tx.get_settings()?;
    db_settings.update_auth_settings(|auth| {
        let mut policy_doc = Doc::new();
        policy_doc.set_json("bootstrap_auto_approve", enabled)?;
        auth.as_doc_mut().set_doc("policy", policy_doc);
        Ok(())
    })?;
    tx.commit()?;
    Ok(())
}
