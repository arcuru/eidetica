//! Bootstrap sync failure scenario tests.
//!
//! This module tests that the bootstrap sync process properly rejects unauthorized
//! access attempts, invalid keys, and permission boundary violations. These tests
//! expect secure behavior and will fail until proper security is implemented.

use super::helpers::*;
use eidetica::{
    Instance,
    auth::Permission,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
};
use std::time::Duration;

/// Test bootstrap behavior when the requesting key lacks sufficient admin permissions.
///
/// This test expects SECURE behavior: Bootstrap should fail with a permission error,
/// and unauthorized clients should not receive database content or be added
/// to the auth configuration.
#[tokio::test]
async fn test_bootstrap_permission_denied_insufficient_admin() {
    println!("\n🧪 TEST: Bootstrap with insufficient admin permissions (should be rejected)");

    // Setup server with restricted auth policy - only specific admin keys allowed
    let mut server_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create server instance");

    // Add server admin key
    server_instance
        .add_private_key("server_admin")
        .expect("Failed to add server admin key");

    // Create database with only server_admin having access
    let mut settings = Doc::new();
    settings.set_string("name", "Restricted Database");

    let server_admin_pubkey = server_instance
        .get_formatted_public_key("server_admin")
        .expect("Failed to get server admin public key")
        .expect("Server admin key should exist");

    // Set strict auth policy - only server_admin has permission to manage auth
    let mut auth_doc = Doc::new();
    auth_doc
        .set_json(
            "server_admin",
            serde_json::json!({
                "pubkey": server_admin_pubkey,
                "permissions": {"Admin": 0},  // Highest admin priority
                "status": "Active"
            }),
        )
        .expect("Failed to set server admin auth");

    settings.set_node("auth", auth_doc);

    // Create the database
    let server_database = server_instance
        .new_database(settings, "server_admin")
        .expect("Failed to create restricted database");

    let restricted_tree_id = server_database.root_id().clone();
    println!(
        "🔐 Created restricted database with ID: {}",
        restricted_tree_id
    );

    // Start server
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    let server_addr = start_sync_server(server_sync).await;

    // Setup client with its own key
    let mut client_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create client instance");

    client_instance
        .add_private_key("unauthorized_client")
        .expect("Failed to add client key");

    let client_pubkey = client_instance
        .get_formatted_public_key("unauthorized_client")
        .expect("Failed to get client public key")
        .expect("Client key should exist");

    println!(
        "👤 Client attempting bootstrap with unauthorized key: {}",
        client_pubkey
    );

    // Enable client sync
    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap with key approval request - should be REJECTED by default
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &restricted_tree_id,
            "unauthorized_client", // Client's key name
            Permission::Write(10), // Requested permission level
        )
        .await;

    // Wait for any async processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("🔍 Bootstrap result: {:?}", bootstrap_result);

    // EXPECTED SECURE BEHAVIOR: Bootstrap should fail for unauthorized client
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should be REJECTED for unauthorized client - test fails because security is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Client should not receive the database
    assert!(
        client_instance.load_database(&restricted_tree_id).is_err(),
        "Client should be DENIED access to restricted database - test fails because security is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Server database auth config should NOT be modified
    let server_auth_settings = server_database
        .get_settings()
        .expect("Failed to get server database settings")
        .get_all()
        .expect("Failed to get all settings");

    // Check that auth section was NOT modified to include unauthorized key
    if let Some(auth_node) = server_auth_settings.get("auth")
        && let Value::Doc(auth_doc) = auth_node
    {
        // EXPECTED SECURE BEHAVIOR: Should NOT contain the unauthorized client key
        assert!(
            !auth_doc.as_hashmap().contains_key("unauthorized_client"),
            "Unauthorized client key should NOT be added to server auth config - test fails because security is not implemented"
        );
        println!(
            "🔍 Server auth keys should remain unchanged: {:?}",
            auth_doc.as_hashmap().keys().collect::<Vec<_>>()
        );
    }

    println!(
        "✅ TEST: Expected secure behavior (will fail until security is properly implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    server_sync.stop_server_async().await.unwrap();
}

/// Test bootstrap behavior when the database has no authentication configuration
/// but the client is requesting key approval.
///
/// This test expects SECURE behavior: Should either fail because there's no auth
/// framework to approve keys against, or succeed only if the database explicitly
/// allows unauthenticated access with proper validation.
#[tokio::test]
async fn test_bootstrap_permission_denied_no_auth_config() {
    println!(
        "\n🧪 TEST: Bootstrap key approval with no auth config (should have defined behavior)"
    );

    // Setup server with a database that has NO authentication configuration
    let mut server_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create server instance");

    server_instance
        .add_private_key("server_key")
        .expect("Failed to add server key");

    // Create database with NO auth configuration
    let mut settings = Doc::new();
    settings.set_string("name", "Unprotected Database");
    // Explicitly NOT setting any "auth" configuration

    let server_database = server_instance
        .new_database(settings, "server_key")
        .expect("Failed to create unprotected database");

    let unprotected_tree_id = server_database.root_id().clone();
    println!(
        "🔓 Created database with no auth config, ID: {}",
        unprotected_tree_id
    );

    // Start server
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    let server_addr = start_sync_server(server_sync).await;

    // Setup client
    let mut client_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create client instance");

    client_instance
        .add_private_key("client_key")
        .expect("Failed to add client key");

    let _client_pubkey = client_instance
        .get_formatted_public_key("client_key")
        .expect("Failed to get client public key")
        .expect("Client key should exist");

    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap with key approval request on database with no auth config — should be REJECTED
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &unprotected_tree_id,
            "client_key",
            Permission::Write(10),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("🔍 Bootstrap result: {:?}", bootstrap_result);

    // EXPECTED SECURE BEHAVIOR: Bootstrap should have well-defined behavior for no-auth databases
    // For now, we expect it to fail until proper policy is defined
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL when no auth framework exists to validate against - test fails because security policy is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Client should not receive database without proper authorization
    assert!(
        client_instance.load_database(&unprotected_tree_id).is_err(),
        "Client should not receive database without proper authorization framework - test fails because security is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Server database should NOT have auth config modified without authorization
    let server_auth_settings = server_database
        .get_settings()
        .expect("Failed to get server database settings")
        .get_all()
        .expect("Failed to get all settings");

    // Check that NO auth section was created by unauthorized bootstrap
    if let Some(auth_node) = server_auth_settings.get("auth")
        && let Value::Doc(auth_doc) = auth_node
    {
        // EXPECTED SECURE BEHAVIOR: Auth config should NOT be created by unauthorized bootstrap
        assert!(
            !auth_doc.as_hashmap().contains_key("client_key"),
            "Auth config should NOT be created by unauthorized bootstrap - test fails because security is not implemented"
        );
        println!(
            "🔍 Server should not have unauthorized auth modifications: {:?}",
            auth_doc.as_hashmap().keys().collect::<Vec<_>>()
        );
    }
    // If no auth section exists, that's the expected secure behavior

    println!(
        "✅ TEST: Expected secure behavior for no-auth database (will fail until proper security policy is implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    server_sync.stop_server_async().await.unwrap();
}

/// Test bootstrap behavior with malformed public key data.
///
/// This test expects SECURE behavior: Bootstrap should fail with clear error
/// for malformed keys and proper validation should be in place.
#[tokio::test]
async fn test_bootstrap_invalid_public_key_format() {
    println!("\n🧪 TEST: Bootstrap with malformed public key format");

    // Setup server
    let mut server_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create server instance");

    server_instance
        .add_private_key("server_key")
        .expect("Failed to add server key");

    // Create database
    let mut settings = Doc::new();
    settings.set_string("name", "Test Database");

    let server_database = server_instance
        .new_database(settings, "server_key")
        .expect("Failed to create database");

    let tree_id = server_database.root_id().clone();

    // Start server
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    let server_addr = start_sync_server(server_sync).await;

    // Setup client with malformed key name (this tests key validation during bootstrap)
    let mut client_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create client instance");

    // Note: We can't directly test malformed keys in the current API since
    // add_private_key() creates valid keys. This test documents the need for
    // key format validation during the bootstrap process itself.
    client_instance
        .add_private_key("client_with_spaces_and_symbols!@#")
        .expect("Failed to add client key");

    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap - current implementation may accept any key name
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &tree_id,
            "client_with_spaces_and_symbols!@#",
            Permission::Write(10),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!(
        "🔍 Bootstrap result with unusual key name: {:?}",
        bootstrap_result
    );

    // EXPECTED SECURE BEHAVIOR: Should fail with proper validation
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL with unusual key name due to proper validation - test fails because validation is not implemented"
    );

    println!("✅ Expected secure behavior: Bootstrap rejected unusual key name");

    // Cleanup
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    server_sync.stop_server_async().await.unwrap();
}

/// Test bootstrap behavior with revoked key status.
///
/// This test expects SECURE behavior: Bootstrap should fail for revoked or
/// inactive keys with proper status validation.
#[tokio::test]
async fn test_bootstrap_with_revoked_key() {
    println!("\n🧪 TEST: Bootstrap attempt with revoked key");

    // Setup server with auth configuration including a revoked key
    let mut server_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create server instance");

    server_instance
        .add_private_key("server_admin")
        .expect("Failed to add server admin key");

    server_instance
        .add_private_key("revoked_client")
        .expect("Failed to add revoked client key");

    let server_admin_pubkey = server_instance
        .get_formatted_public_key("server_admin")
        .expect("Failed to get server admin public key")
        .expect("Server admin key should exist");

    let revoked_client_pubkey = server_instance
        .get_formatted_public_key("revoked_client")
        .expect("Failed to get revoked client public key")
        .expect("Revoked client key should exist");

    // Create database with auth configuration including the revoked key
    let mut settings = Doc::new();
    settings.set_string("name", "Database With Revoked Key");

    let mut auth_doc = Doc::new();
    auth_doc
        .set_json(
            "server_admin",
            serde_json::json!({
                "pubkey": server_admin_pubkey,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set server admin auth");

    auth_doc
        .set_json(
            "revoked_client",
            serde_json::json!({
                "pubkey": revoked_client_pubkey,
                "permissions": {"Write": 10},
                "status": "Revoked"  // Key is explicitly revoked
            }),
        )
        .expect("Failed to set revoked client auth");

    settings.set_node("auth", auth_doc);

    let server_database = server_instance
        .new_database(settings, "server_admin")
        .expect("Failed to create database");

    let tree_id = server_database.root_id().clone();

    // Start server
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    let server_addr = start_sync_server(server_sync).await;

    // Setup different client instance (to simulate external client using revoked key)
    let mut client_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create client instance");

    // Note: In a real scenario, the client would have the private key corresponding
    // to the revoked public key. For testing, we create a key with the same name.
    client_instance
        .add_private_key("attempting_revoked_access")
        .expect("Failed to add client key");

    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap with a different key (since we can't use the actual revoked key easily)
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &tree_id,
            "attempting_revoked_access",
            Permission::Write(10),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!(
        "🔍 Bootstrap result with new key on database containing revoked keys: {:?}",
        bootstrap_result
    );

    // EXPECTED SECURE BEHAVIOR: Bootstrap should fail for any attempt with revoked key context
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL when database contains revoked keys and proper validation exists - test fails because key status validation is not implemented"
    );

    println!(
        "✅ TEST: Expected secure behavior for revoked key scenario (will fail until key status validation is implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    server_sync.stop_server_async().await.unwrap();
}

/// Test bootstrap behavior when requesting permissions that exceed granted levels.
///
/// This test expects SECURE behavior: Bootstrap should either reject excessive
/// permission requests or grant only appropriate permission levels based on policy.
#[tokio::test]
async fn test_bootstrap_exceeds_granted_permissions() {
    println!("\n🧪 TEST: Bootstrap requesting excessive permissions");

    // Setup server with policy allowing only Read permissions for new clients
    let mut server_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create server instance");

    server_instance
        .add_private_key("server_admin")
        .expect("Failed to add server admin key");

    let server_admin_pubkey = server_instance
        .get_formatted_public_key("server_admin")
        .expect("Failed to get server admin public key")
        .expect("Server admin key should exist");

    // Create database with restrictive auth policy
    let mut settings = Doc::new();
    settings.set_string("name", "Restrictive Permission Database");

    let mut auth_doc = Doc::new();
    auth_doc
        .set_json(
            "server_admin",
            serde_json::json!({
                "pubkey": server_admin_pubkey,
                "permissions": {"Admin": 0},
                "status": "Active"
            }),
        )
        .expect("Failed to set server admin auth");

    // TODO: Add policy configuration that limits new client permissions to Read only
    settings.set_node("auth", auth_doc);

    let server_database = server_instance
        .new_database(settings, "server_admin")
        .expect("Failed to create database");

    let tree_id = server_database.root_id().clone();

    // Start server
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    let server_addr = start_sync_server(server_sync).await;

    // Setup client requesting Admin permissions (should be excessive)
    let mut client_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create client instance");

    client_instance
        .add_private_key("greedy_client")
        .expect("Failed to add client key");

    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap requesting Admin permissions (excessive for a new client)
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &tree_id,
            "greedy_client",
            Permission::Admin(0), // Requesting highest admin level
        )
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!(
        "🔍 Bootstrap result requesting excessive Admin permissions: {:?}",
        bootstrap_result
    );

    // EXPECTED SECURE BEHAVIOR: Bootstrap should fail or limit excessive permission requests
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL when requesting excessive Admin permissions - test fails because permission validation is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: No permissions should be granted for failed bootstrap
    let server_auth_settings = server_database
        .get_settings()
        .expect("Failed to get server database settings")
        .get_all()
        .expect("Failed to get all settings");

    if let Some(auth_node) = server_auth_settings.get("auth")
        && let Value::Doc(auth_doc) = auth_node
    {
        // EXPECTED SECURE BEHAVIOR: Greedy client should NOT be in auth config
        assert!(
            !auth_doc.as_hashmap().contains_key("greedy_client"),
            "Greedy client should NOT be granted any permissions for excessive request - test fails because permission validation is not implemented"
        );
    }
    println!(
        "✅ TEST: Expected secure behavior for excessive permission requests (will fail until permission validation is implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync_mut().expect("Server should have sync");
    server_sync.stop_server_async().await.unwrap();
}
