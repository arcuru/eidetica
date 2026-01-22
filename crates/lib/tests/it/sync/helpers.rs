//! Helper functions for Sync testing
//!
//! This module provides utilities for testing Sync functionality including
//! setup operations, common test patterns, transport factories, and assertion helpers.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use eidetica::{
    Instance, Result,
    entry::ID,
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
pub async fn setup_db() -> Instance {
    let (instance, _user) = crate::helpers::setup_db().await;
    instance
}

/// Create a new Sync instance with standard setup
pub async fn setup() -> (Instance, Sync) {
    let base_db = setup_db().await;
    let sync = Sync::new(base_db.clone())
        .await
        .expect("Failed to create Sync");
    (base_db, sync)
}

/// Create Instance with initialized sync module
pub async fn setup_instance_with_initialized() -> Instance {
    let (instance, _user) = crate::helpers::setup_db().await;
    instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync");
    instance
}

/// Create a test SyncHandler for transport-specific tests
///
/// Returns (Instance, Handler) where the Instance must be kept alive
/// for the handler's WeakInstance to remain valid.
pub async fn setup_test_handler() -> (Instance, Arc<dyn SyncHandler>) {
    let base_db = setup_db().await;
    let sync = Sync::new(base_db.clone())
        .await
        .expect("Failed to create Sync");
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
    // Create empty context for tests
    let context = eidetica::sync::protocol::RequestContext::default();
    handler.handle_request(request, &context).await
}

// ===== ASSERTION HELPERS =====

/// Assert that a setting has the expected value
pub async fn assert_setting(sync: &Sync, key: &str, expected_value: &str) {
    let actual_value = sync.get_setting(key).await.expect("Failed to get setting");
    assert_eq!(actual_value, Some(expected_value.to_string()));
}

/// Assert that a setting does not exist
pub async fn assert_setting_not_found(sync: &Sync, key: &str) {
    let actual_value = sync.get_setting(key).await.expect("Failed to get setting");
    assert_eq!(actual_value, None);
}

/// Assert that two sync instances refer to the same tree
pub fn assert_trees_equal(sync1: &Sync, sync2: &Sync) {
    assert_eq!(sync1.sync_tree_root_id(), sync2.sync_tree_root_id());
}

// ===== OPERATION HELPERS =====

/// Set multiple settings on a sync instance
pub async fn set_multiple_settings(sync: &Sync, settings: &[(&str, &str)]) {
    for (key, value) in settings {
        sync.set_setting(*key, *value)
            .await
            .unwrap_or_else(|_| panic!("Failed to set setting: {key} = {value}"));
    }
}

/// Assert multiple settings have expected values
pub async fn assert_multiple_settings(sync: &Sync, expected: &[(&str, &str)]) {
    for (key, expected_value) in expected {
        assert_setting(sync, key, expected_value).await;
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
#[async_trait]
pub trait TransportFactory: Send + std::marker::Sync {
    /// Create a sync instance with this transport enabled
    async fn create_sync(&self, instance: Instance) -> Result<Sync>;

    /// Get the expected address format for this transport
    fn create_address(&self, server_addr: &str) -> Address;

    /// Get a display name for this transport type
    fn transport_name(&self) -> &'static str;
}

/// Factory for HTTP transport instances
pub struct HttpTransportFactory;

#[async_trait]
impl TransportFactory for HttpTransportFactory {
    async fn create_sync(&self, instance: Instance) -> Result<Sync> {
        let sync = Sync::new(instance).await?;
        sync.enable_http_transport().await?;
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

#[async_trait]
impl TransportFactory for IrohTransportFactory {
    async fn create_sync(&self, instance: Instance) -> Result<Sync> {
        let sync = Sync::new(instance).await?;
        let transport = IrohTransport::builder()
            .relay_mode(RelayMode::Disabled)
            .build()?;
        sync.add_transport("iroh", Box::new(transport)).await?;
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

/// Create a server instance configured for manual bootstrap approval
///
/// # Returns
/// (Instance, User, key_id, Database, Sync, tree_id)
///
/// # Implementation Note
/// This function uses the admin for sync handler operations. The device key
/// is accessed via Instance internals and will be migrated to InstanceMetadata shortly
pub async fn setup_manual_approval_server() -> (Instance, User, String, Database, Sync, ID) {
    let (instance, mut user, key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;

    // Initialize sync
    instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync");

    // Create database with manual approval (no global wildcard permission)
    let mut settings = Doc::new();
    settings.set("name", "Bootstrap Test Database");

    let mut auth_doc = Doc::new();

    // Add user's key to auth settings by key_id (required for User::create_database)
    // The key_id is the public key string (e.g., "ed25519:...")
    auth_doc
        .set_json(
            &key_id,
            serde_json::json!({
                "pubkey": key_id,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set user key auth");

    // Add device key to auth settings for sync handler operations
    let device_pubkey = instance.device_id_string();

    auth_doc
        .set_json(
            "admin",
            serde_json::json!({
                "pubkey": device_pubkey,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set device key auth");

    settings.set("auth", auth_doc);

    let database = user
        .create_database(settings, &key_id)
        .await
        .expect("Failed to create database");
    let tree_id = database.root_id().clone();

    // Create sync instance
    let sync = Sync::new(instance.clone())
        .await
        .expect("Failed to create sync");

    // Enable sync for this database
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .expect("Failed to enable sync for database");

    (instance, user, key_id, database, sync, tree_id)
}

/// Create a server with global wildcard permission for automatic bootstrap approval
///
/// # Returns
/// (Instance, User, key_id, Database, Sync, tree_id)
pub async fn setup_global_wildcard_server() -> (
    Instance,
    eidetica::user::User,
    String,
    Database,
    Sync,
    eidetica::entry::ID,
) {
    let (instance, mut user, key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;

    // Initialize sync
    instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync");

    // Create database with global wildcard permission
    let mut settings = Doc::new();
    settings.set("name", "Bootstrap Test Database");

    let mut auth_doc = Doc::new();

    // Add user's key to auth settings by key_id (required for User::create_database)
    // The key_id is the public key string (e.g., "ed25519:...")
    auth_doc
        .set_json(
            &key_id,
            serde_json::json!({
                "pubkey": key_id,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set user key auth");

    // Add device key to auth settings for sync handler operations
    let device_pubkey = instance.device_id_string();

    auth_doc
        .set_json(
            "admin",
            serde_json::json!({
                "pubkey": device_pubkey,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set device key auth");

    // Add global wildcard permission for automatic bootstrap approval
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set global wildcard permission");

    settings.set("auth", auth_doc);

    let database = user
        .create_database(settings, &key_id)
        .await
        .expect("Failed to create database");
    let tree_id = database.root_id().clone();

    // Create sync instance
    let sync = Sync::new(instance.clone())
        .await
        .expect("Failed to create sync");

    // Enable sync for this database
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .expect("Failed to enable sync for database");

    (instance, user, key_id, database, sync, tree_id)
}

/// Create a server with auto approval (auto_approve = true)
///
/// Returns (Instance, User, key_id, Database, Arc<Sync>, tree_id)
pub async fn setup_auto_approval_server() -> (
    Instance,
    User,
    String,
    Database,
    Arc<Sync>,
    eidetica::entry::ID,
) {
    let (instance, user, key_id, database, tree_id, sync) =
        setup_sync_enabled_server_with_auto_approve("server_user", "server_key", "test_database")
            .await;

    (instance, user, key_id, database, sync, tree_id)
}

/// Start a sync server with common settings
pub async fn start_sync_server(sync: &Sync) -> String {
    sync.enable_http_transport()
        .await
        .expect("Failed to enable HTTP transport");
    sync.start_server("127.0.0.1:0")
        .await
        .expect("Failed to start server");
    sync.get_server_address()
        .await
        .expect("Failed to get server address")
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
        peer_pubkey: None,
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
    let context = eidetica::sync::protocol::RequestContext::default();
    let response = handler.handle_request(&request, &context).await;

    match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    }
}

/// Approve a bootstrap request using a User's key
pub async fn approve_request(
    user: &eidetica::user::User,
    sync: &Sync,
    request_id: &str,
    approver_key_id: &str,
) -> Result<()> {
    user.approve_bootstrap_request(sync, request_id, approver_key_id)
        .await
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
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    }
}

/// Assert that sync has expected number of pending requests
pub async fn assert_request_stored(sync: &Sync, expected_count: usize) {
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
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
/// Uses global wildcard permission for automatic bootstrap approval.
///
/// Returns (Instance, User, key_id: String, Database, TreeId)
///
/// # Implementation Note
/// This function uses the admin for sync handler operations.
pub async fn setup_server_with_bootstrap_database(
    username: &str,
    key_name: &str,
    db_name: &str,
) -> (Instance, User, String, Database, eidetica::entry::ID) {
    let server_instance = setup_instance_with_initialized().await;
    server_instance.create_user(username, None).await.unwrap();
    let mut server_user = server_instance.login_user(username, None).await.unwrap();
    let server_key_id = server_user.add_private_key(Some(key_name)).await.unwrap();

    let mut settings = Doc::new();
    settings.set("name", db_name);

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Add global wildcard permission for automatic bootstrap approval
    set_global_wildcard_permission(&server_database)
        .await
        .unwrap();

    // Add admin to the database's auth configuration so sync handler can modify the database
    let device_key_name = "admin";
    let device_pubkey = server_instance.device_id_string();

    // Add admin as Admin to the database
    let tx = server_database.new_transaction().await.unwrap();
    let settings_store = tx.get_settings().unwrap();
    let device_auth_key =
        eidetica::auth::types::AuthKey::active(device_pubkey, eidetica::auth::Permission::Admin(0))
            .unwrap();
    settings_store
        .set_auth_key(device_key_name, device_auth_key)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Add bootstrap auto-approval policy
    set_global_wildcard_permission(&server_database)
        .await
        .unwrap();

    // Enable sync for this database
    let sync = server_instance.sync().expect("Sync should be initialized");
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    // Sync the user database to update combined settings
    sync.sync_user(
        server_user.user_uuid(),
        server_user.user_database().root_id(),
    )
    .await
    .unwrap();

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
pub async fn setup_indexed_client(index: usize) -> (Instance, User, String) {
    let client_instance = setup_instance_with_initialized().await;
    let client_username = format!("client_user_{index}");
    client_instance
        .create_user(&client_username, None)
        .await
        .unwrap();
    let mut client_user = client_instance
        .login_user(&client_username, None)
        .await
        .unwrap();

    let client_key_display_name = format!("client_key_{index}");
    let client_key_id = client_user
        .add_private_key(Some(&client_key_display_name))
        .await
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
        client_sync.enable_http_transport().await?;

        user.request_database_access(&client_sync, server_addr, tree_id, key_id, permission)
            .await?;
    } // Drop Arc before sleep

    // Wait for sync to complete
    tokio::time::sleep(Duration::from_millis(sync_delay_ms)).await;

    // Establish database-key mapping
    // Use the same key_id as the SigKey identifier (common pattern)
    user.map_key(key_id, tree_id, key_id).await?;

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

/// Sets global wildcard permission on a database for automatic bootstrap approval
///
/// # Arguments
/// * `database` - The database to configure
/// * `permission` - The permission level to grant (e.g., Write(0) allows all Write/Read requests)
///
/// # Examples
/// ```ignore
/// // Allow all Write and Read requests (but not Admin)
/// set_global_wildcard_permission_with_level(&db, Permission::Write(0))?;
///
/// // Allow only Read requests
/// set_global_wildcard_permission_with_level(&db, Permission::Read)?;
/// ```
pub async fn set_global_wildcard_permission_with_level(
    database: &Database,
    permission: eidetica::auth::Permission,
) -> Result<()> {
    let tx = database.new_transaction().await?;
    let db_settings = tx.get_settings()?;
    db_settings
        .set_auth_key(
            "*",
            eidetica::auth::types::AuthKey::active("*".to_string(), permission).unwrap(),
        )
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Sets global wildcard permission with Write(0) - allows all Write and Read requests
///
/// This is a convenience wrapper that sets Write(0) permission, which allows:
/// - All Write requests (Write(1), Write(5), Write(100), etc.)
/// - All Read requests
/// - But denies Admin requests.
pub async fn set_global_wildcard_permission(database: &Database) -> Result<()> {
    set_global_wildcard_permission_with_level(database, eidetica::auth::Permission::Write(0)).await
}

// ===== SYNC-ENABLED DATABASE HELPERS =====

use eidetica::user::types::{SyncSettings, TrackedDatabase};

/// Creates a server with a sync-enabled database ready to serve sync requests.
///
/// This helper sets up the complete workflow:
/// - Creates instance with sync initialized
/// - Creates user with a private key
/// - Creates database
/// - Enables sync for the database in user preferences
/// - Syncs the user database to update combined settings
///
/// # Returns
/// (Instance, User, key_id, Database, tree_id, Arc<Sync>)
///
/// # Implementation Note
/// This function uses the admin for sync handler operations.
pub async fn setup_sync_enabled_server(
    username: &str,
    key_name: &str,
    db_name: &str,
) -> (
    Instance,
    User,
    String,
    Database,
    eidetica::entry::ID,
    Arc<Sync>,
) {
    let server_instance = setup_instance_with_initialized().await;
    server_instance.create_user(username, None).await.unwrap();
    let mut server_user = server_instance.login_user(username, None).await.unwrap();
    let server_key_id = server_user.add_private_key(Some(key_name)).await.unwrap();

    let mut settings = Doc::new();
    settings.set("name", db_name);

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Add admin to the database's auth configuration so sync handler can modify the database
    let device_key_name = "admin";
    let device_pubkey = server_instance.device_id_string();

    // Add admin as Admin to the database
    let tx = server_database.new_transaction().await.unwrap();
    let settings_store = tx.get_settings().unwrap();
    let device_auth_key =
        eidetica::auth::types::AuthKey::active(device_pubkey, eidetica::auth::Permission::Admin(0))
            .unwrap();
    settings_store
        .set_auth_key(device_key_name, device_auth_key)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Enable sync for this database
    let sync = server_instance.sync().expect("Sync should be initialized");
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    // Sync the user database to update combined settings
    sync.sync_user(
        server_user.user_uuid(),
        server_user.user_database().root_id(),
    )
    .await
    .unwrap();

    (
        server_instance,
        server_user,
        server_key_id,
        server_database,
        tree_id,
        sync,
    )
}

/// Creates a server with sync-enabled database AND bootstrap auto-approval.
///
/// Combines sync enablement with bootstrap auto-approval for bootstrap tests.
///
/// # Returns
/// (Instance, User, key_id, Database, tree_id, Arc<Sync>)
pub async fn setup_sync_enabled_server_with_auto_approve(
    username: &str,
    key_name: &str,
    db_name: &str,
) -> (
    Instance,
    User,
    String,
    Database,
    eidetica::entry::ID,
    Arc<Sync>,
) {
    let (instance, user, key_id, database, tree_id, sync) =
        setup_sync_enabled_server(username, key_name, db_name).await;

    // Add bootstrap auto-approval policy
    set_global_wildcard_permission(&database).await.unwrap();

    (instance, user, key_id, database, tree_id, sync)
}

/// Creates a client with sync initialized, ready to request database access.
///
/// # Returns
/// (Instance, User, key_id, Arc<Sync>)
pub async fn setup_sync_enabled_client(
    username: &str,
    key_name: &str,
) -> (Instance, User, String, Arc<Sync>) {
    let client_instance = setup_instance_with_initialized().await;
    client_instance.create_user(username, None).await.unwrap();
    let mut client_user = client_instance.login_user(username, None).await.unwrap();
    let client_key_id = client_user.add_private_key(Some(key_name)).await.unwrap();

    let sync = client_instance.sync().expect("Sync should be initialized");

    (client_instance, client_user, client_key_id, sync)
}

/// Enable sync for a database created without the User API.
///
/// This directly updates the sync tree for databases created via instance.new_database()
/// instead of user.create_database().
///
/// TODO: This should go away eventually or be replaced by the User API
pub async fn enable_sync_for_instance_database(
    sync: &Sync,
    database_id: &eidetica::entry::ID,
) -> Result<()> {
    use eidetica::store::DocStore;
    use eidetica::user::types::SyncSettings;

    // Open the sync tree to set combined settings
    let instance = sync.instance()?;
    let signing_key = instance.device_key().clone();

    let sync_database = eidetica::Database::open(
        instance.clone(),
        sync.sync_tree_root_id(),
        signing_key,
        "admin".to_string(),
    )
    .await?;

    let tx = sync_database.new_transaction().await?;
    let database_users = tx.get_store::<DocStore>("database_users").await?;

    // Create enabled sync settings
    let settings = SyncSettings {
        sync_enabled: true,
        sync_on_commit: false,
        interval_seconds: None,
        properties: Default::default(),
    };

    let db_id_str = database_id.to_string();
    let settings_json = serde_json::to_string(&settings).map_err(|e| {
        eidetica::Error::Sync(eidetica::sync::error::SyncError::SerializationError(
            e.to_string(),
        ))
    })?;

    database_users
        .set_path(
            eidetica::path!(&db_id_str, "combined_settings"),
            settings_json,
        )
        .await?;

    tx.commit().await?;

    Ok(())
}

/// Creates a public (unauthenticated) sync-enabled database with wildcard "*" permissions.
///
/// This is useful for testing unauthenticated sync scenarios where clients can
/// access the database without providing credentials.
///
/// # Returns
/// (Instance, User, key_id, Database, tree_id, Arc<Sync>)
///
/// # Implementation Note
/// This function uses the admin for sync handler operations.
pub async fn setup_public_sync_enabled_server(
    username: &str,
    key_name: &str,
    db_name: &str,
) -> (
    Instance,
    User,
    String,
    Database,
    eidetica::entry::ID,
    Arc<Sync>,
) {
    let server_instance = setup_instance_with_initialized().await;
    server_instance.create_user(username, None).await.unwrap();
    let mut server_user = server_instance.login_user(username, None).await.unwrap();
    let server_key_id = server_user.add_private_key(Some(key_name)).await.unwrap();

    // Create database settings with wildcard "*" permission for public access
    let mut settings = Doc::new();
    settings.set("name", db_name);

    // Add auth config with wildcard permission for unauthenticated access
    let mut auth_settings = eidetica::auth::AuthSettings::new();

    let device_pubkey = server_instance.device_id_string();

    // Add device key for database operations
    auth_settings
        .add_key(
            "admin",
            eidetica::auth::AuthKey::active(&device_pubkey, eidetica::auth::Permission::Admin(0))
                .unwrap(),
        )
        .unwrap();

    // Add user's key (server_key_id is already the formatted public key)
    auth_settings
        .add_key(
            &server_key_id,
            eidetica::auth::AuthKey::active(&server_key_id, eidetica::auth::Permission::Admin(0))
                .unwrap(),
        )
        .unwrap();

    // Add wildcard "*" permission to allow unauthenticated read access
    auth_settings
        .add_key(
            "*",
            eidetica::auth::AuthKey::active("*", eidetica::auth::Permission::Read).unwrap(),
        )
        .unwrap();

    settings.set("auth", auth_settings.as_doc().clone());

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Enable sync for this database
    let sync = server_instance.sync().expect("Sync should be initialized");
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    // Sync the user database to update combined settings
    sync.sync_user(
        server_user.user_uuid(),
        server_user.user_database().root_id(),
    )
    .await
    .unwrap();

    (
        server_instance,
        server_user,
        server_key_id,
        server_database,
        tree_id,
        sync,
    )
}
