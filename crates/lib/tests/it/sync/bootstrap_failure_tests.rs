//! Bootstrap sync failure scenario tests.
//!
//! This module tests that the bootstrap sync process properly rejects unauthorized
//! access attempts, invalid keys, and permission boundary violations. These tests
//! expect secure behavior and will fail until proper security is implemented.

#![allow(deprecated)] // Uses LegacyInstanceOps

use super::helpers::*;
use crate::helpers::test_instance_with_user_and_key;
use eidetica::{
    auth::{
        AuthSettings, Permission,
        types::{AuthKey, KeyStatus},
    },
    crdt::{Doc, doc::Value},
    instance::LegacyInstanceOps,
};

/// Test bootstrap behavior when the requesting key lacks sufficient admin permissions.
///
/// This test expects SECURE behavior: Bootstrap should fail with a permission error,
/// and unauthorized clients should not receive database content or be added
/// to the auth configuration.
#[tokio::test]
#[allow(deprecated)] // Uses get_formatted_public_key for _device_key
async fn test_bootstrap_permission_denied_insufficient_admin() {
    println!("\n🧪 TEST: Bootstrap with insufficient admin permissions (should be rejected)");

    // Setup server with restricted auth policy - only specific admin keys allowed
    let (server_instance, mut server_user, server_admin_key_id) =
        test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on server");

    // Create database with only server_admin having access
    let mut settings = Doc::new();
    settings.set("name", "Restricted Database");

    // Set strict auth policy - only server_admin has permission to manage auth
    let server_admin_pubkey = server_user
        .get_public_key(&server_admin_key_id)
        .expect("Failed to get server admin public key");

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(
            &server_admin_key_id,
            AuthKey::active(&server_admin_pubkey, Permission::Admin(0))
                .expect("Failed to create admin key"),
        )
        .expect("Failed to add server admin auth");

    // Add device key to auth settings for sync handler operations
    let device_pubkey = server_instance
        .get_formatted_public_key("_device_key")
        .await
        .expect("Failed to get device public key");

    auth_settings
        .add_key(
            "_device_key",
            AuthKey::active(&device_pubkey, Permission::Admin(0))
                .expect("Failed to create device key"),
        )
        .expect("Failed to add device key auth");

    settings.set("auth", auth_settings.as_doc().clone());

    // Create the database
    let server_database = server_user
        .create_database(settings, &server_admin_key_id)
        .await
        .expect("Failed to create restricted database");

    let restricted_tree_id = server_database.root_id().clone();
    println!("🔐 Created restricted database with ID: {restricted_tree_id}");

    // Start server
    let server_addr = {
        let server_sync = server_instance.sync().expect("Server should have sync");
        start_sync_server(&server_sync).await
    };

    // Setup client with its own key
    let (client_instance, _client_user, client_key_id) =
        test_instance_with_user_and_key("client_user", Some("unauthorized_client")).await;
    client_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on client");

    println!("👤 Client attempting bootstrap with unauthorized key: {client_key_id}");

    // Enable client sync
    let bootstrap_result = {
        let client_sync = client_instance.sync().expect("Client should have sync");
        client_sync
            .enable_http_transport()
            .await
            .expect("Failed to enable HTTP transport");

        // Attempt bootstrap with key approval request - should be REJECTED by default
        let result = client_sync
            .sync_with_peer_for_bootstrap(
                &server_addr,
                &restricted_tree_id,
                "unauthorized_client", // Client's key name
                Permission::Write(10), // Requested permission level
            )
            .await;

        // Flush any pending sync work
        client_sync.flush().await.ok();
        result
    };

    println!("🔍 Bootstrap result: {bootstrap_result:?}");

    // EXPECTED SECURE BEHAVIOR: Bootstrap should fail for unauthorized client
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should be REJECTED for unauthorized client - test fails because security is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Client should not receive the database
    assert!(
        client_instance
            .load_database(&restricted_tree_id)
            .await
            .is_err(),
        "Client should be DENIED access to restricted database - test fails because security is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Server database auth config should NOT be modified
    let server_auth_settings = server_database
        .get_settings()
        .await
        .expect("Failed to get server database settings")
        .get_all()
        .await
        .expect("Failed to get all settings");

    // Check that auth section was NOT modified to include unauthorized key
    if let Some(auth_node) = server_auth_settings.get("auth")
        && let Value::Doc(auth_doc) = auth_node
    {
        // EXPECTED SECURE BEHAVIOR: Should NOT contain the unauthorized client key
        assert!(
            !auth_doc.contains_key(&client_key_id),
            "Unauthorized client key should NOT be added to server auth config - test fails because security is not implemented"
        );
        println!(
            "🔍 Server auth keys should remain unchanged: {:?}",
            auth_doc.keys().collect::<Vec<_>>()
        );
    }

    println!(
        "✅ TEST: Expected secure behavior (will fail until security is properly implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync().expect("Server should have sync");
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap behavior when the database has no authentication configuration
/// but the client is requesting key approval.
///
/// This test expects SECURE behavior: Should either fail because there's no auth
/// framework to approve keys against, or succeed only if the database explicitly
/// allows unauthenticated access with proper validation.
#[tokio::test]
#[allow(deprecated)] // Uses get_formatted_public_key for _device_key
async fn test_bootstrap_permission_denied_no_auth_config() {
    println!(
        "\n🧪 TEST: Bootstrap key approval with no auth config (should have defined behavior)"
    );

    // Setup server with a database that has NO authentication configuration
    let (server_instance, mut server_user, server_key_id) =
        test_instance_with_user_and_key("server_user", Some("server_key")).await;
    server_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on server");

    // Create database with NO auth configuration
    let mut settings = Doc::new();
    settings.set("name", "Unprotected Database");
    // Explicitly NOT setting any "auth" configuration

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .expect("Failed to create unprotected database");

    let unprotected_tree_id = server_database.root_id().clone();
    println!("🔓 Created database with no auth config, ID: {unprotected_tree_id}");

    // Start server
    let server_addr = {
        let server_sync = server_instance.sync().expect("Server should have sync");
        start_sync_server(&server_sync).await
    };

    // Setup client
    let (client_instance, _client_user, client_key_id) =
        test_instance_with_user_and_key("client_user", Some("client_key")).await;
    client_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on client");

    let bootstrap_result = {
        let client_sync = client_instance.sync().expect("Client should have sync");
        client_sync
            .enable_http_transport()
            .await
            .expect("Failed to enable HTTP transport");

        // Attempt bootstrap with key approval request on database with no auth config — should be REJECTED
        let result = client_sync
            .sync_with_peer_for_bootstrap(
                &server_addr,
                &unprotected_tree_id,
                &client_key_id,
                Permission::Write(10),
            )
            .await;

        // Flush any pending sync work
        client_sync.flush().await.ok();
        result
    };

    println!("🔍 Bootstrap result: {bootstrap_result:?}");

    // EXPECTED SECURE BEHAVIOR: Bootstrap should have well-defined behavior for no-auth databases
    // For now, we expect it to fail until proper policy is defined
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL when no auth framework exists to validate against - test fails because security policy is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Client should not receive database without proper authorization
    assert!(
        client_instance
            .load_database(&unprotected_tree_id)
            .await
            .is_err(),
        "Client should not receive database without proper authorization framework - test fails because security is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: Server database should NOT have auth config modified without authorization
    let server_auth_settings = server_database
        .get_settings()
        .await
        .expect("Failed to get server database settings")
        .get_all()
        .await
        .expect("Failed to get all settings");

    // Check that NO auth section was created by unauthorized bootstrap
    if let Some(auth_node) = server_auth_settings.get("auth")
        && let Value::Doc(auth_doc) = auth_node
    {
        // EXPECTED SECURE BEHAVIOR: Auth config should NOT be created by unauthorized bootstrap
        assert!(
            !auth_doc.contains_key(&client_key_id),
            "Auth config should NOT be created by unauthorized bootstrap - test fails because security is not implemented"
        );
        println!(
            "🔍 Server should not have unauthorized auth modifications: {:?}",
            auth_doc.keys().collect::<Vec<_>>()
        );
    }
    // If no auth section exists, that's the expected secure behavior

    println!(
        "✅ TEST: Expected secure behavior for no-auth database (will fail until proper security policy is implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync().expect("Server should have sync");
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap behavior with malformed public key data.
///
/// This test expects SECURE behavior: Bootstrap should fail with clear error
/// for malformed keys and proper validation should be in place.
#[tokio::test]
async fn test_bootstrap_invalid_public_key_format() {
    println!("\n🧪 TEST: Bootstrap with malformed public key format");

    // Setup server
    let (server_instance, mut server_user, server_key_id) =
        test_instance_with_user_and_key("server_user", Some("server_key")).await;
    server_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on server");

    // Create database
    let mut settings = Doc::new();
    settings.set("name", "Test Database");

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .expect("Failed to create database");

    let tree_id = server_database.root_id().clone();

    // Start server
    let server_addr = {
        let server_sync = server_instance.sync().expect("Server should have sync");
        start_sync_server(&server_sync).await
    };

    // Setup client with malformed key name (this tests key validation during bootstrap)
    let (client_instance, _client_user, client_key_id) =
        test_instance_with_user_and_key("client_user", Some("client_with_spaces_and_symbols!@#"))
            .await;
    client_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on client");

    // Note: We can't directly test malformed keys in the current API since
    // add_private_key() creates valid keys. This test documents the need for
    // key format validation during the bootstrap process itself.

    let client_sync = client_instance.sync().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .await
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap - current implementation may accept any key name
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &tree_id,
            &client_key_id,
            Permission::Write(10),
        )
        .await;

    // Flush any pending sync work
    client_sync.flush().await.ok();

    println!("🔍 Bootstrap result with unusual key name: {bootstrap_result:?}");

    // EXPECTED SECURE BEHAVIOR: Should fail with proper validation
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL with unusual key name due to proper validation - test fails because validation is not implemented"
    );

    println!("✅ Expected secure behavior: Bootstrap rejected unusual key name");

    // Cleanup
    let server_sync = server_instance.sync().expect("Server should have sync");
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap behavior with revoked key status.
///
/// This test expects SECURE behavior: Bootstrap should fail for revoked or
/// inactive keys with proper status validation.
#[tokio::test]
#[allow(deprecated)] // Uses get_formatted_public_key for _device_key
async fn test_bootstrap_with_revoked_key() {
    println!("\n🧪 TEST: Bootstrap attempt with revoked key");

    // Setup server with auth configuration including a revoked key
    let (server_instance, mut server_user, server_admin_key_id) =
        test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on server");

    // Create a second key on the same user to represent a revoked client
    let revoked_client_key_id = server_user
        .add_private_key(Some("revoked_client"))
        .await
        .expect("Failed to add revoked client key");

    // Create database with auth configuration including the revoked key
    let mut settings = Doc::new();
    settings.set("name", "Database With Revoked Key");

    let server_admin_pubkey = server_user
        .get_public_key(&server_admin_key_id)
        .expect("Failed to get server admin public key");
    let revoked_client_pubkey = server_user
        .get_public_key(&revoked_client_key_id)
        .expect("Failed to get revoked client public key");

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(
            &server_admin_key_id,
            AuthKey::active(&server_admin_pubkey, Permission::Admin(0))
                .expect("Failed to create admin key"),
        )
        .expect("Failed to add server admin auth");

    auth_settings
        .add_key(
            &revoked_client_key_id,
            AuthKey::new(
                &revoked_client_pubkey,
                Permission::Write(10),
                KeyStatus::Revoked,
            )
            .expect("Failed to create revoked client key"),
        )
        .expect("Failed to add revoked client auth");

    // Add device key to auth settings for sync handler operations
    let device_pubkey = server_instance
        .get_formatted_public_key("_device_key")
        .await
        .expect("Failed to get device public key");

    auth_settings
        .add_key(
            "_device_key",
            AuthKey::active(&device_pubkey, Permission::Admin(0))
                .expect("Failed to create device key"),
        )
        .expect("Failed to add device key auth");

    settings.set("auth", auth_settings.as_doc().clone());

    let server_database = server_user
        .create_database(settings, &server_admin_key_id)
        .await
        .expect("Failed to create database");

    let tree_id = server_database.root_id().clone();

    // Start server
    let server_addr = {
        let server_sync = server_instance.sync().expect("Server should have sync");
        start_sync_server(&server_sync).await
    };

    // Setup different client instance (to simulate external client using revoked key)
    let (client_instance, _client_user, client_key_id) =
        test_instance_with_user_and_key("client_user", Some("attempting_revoked_access")).await;
    client_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on client");

    // Note: In a real scenario, the client would have the private key corresponding
    // to the revoked public key. For testing, we create a key with the same name.

    let client_sync = client_instance.sync().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .await
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap with a different key (since we can't use the actual revoked key easily)
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &tree_id,
            &client_key_id,
            Permission::Write(10),
        )
        .await;

    // Flush any pending sync work
    client_sync.flush().await.ok();

    println!(
        "🔍 Bootstrap result with new key on database containing revoked keys: {bootstrap_result:?}"
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
    let server_sync = server_instance.sync().expect("Server should have sync");
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap behavior when requesting permissions that exceed granted levels.
///
/// This test expects SECURE behavior: Bootstrap should either reject excessive
/// permission requests or grant only appropriate permission levels based on policy.
#[tokio::test]
#[allow(deprecated)] // Uses get_formatted_public_key for _device_key
async fn test_bootstrap_exceeds_granted_permissions() {
    println!("\n🧪 TEST: Bootstrap requesting excessive permissions");

    // Setup server with policy allowing only Read permissions for new clients
    let (server_instance, mut server_user, server_admin_key_id) =
        test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on server");

    // Create database with restrictive auth policy
    let mut settings = Doc::new();
    settings.set("name", "Restrictive Permission Database");

    let server_admin_pubkey = server_user
        .get_public_key(&server_admin_key_id)
        .expect("Failed to get server admin public key");

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(
            &server_admin_key_id,
            AuthKey::active(&server_admin_pubkey, Permission::Admin(0))
                .expect("Failed to create admin key"),
        )
        .expect("Failed to add server admin auth");

    // Add device key to auth settings for sync handler operations
    let device_pubkey = server_instance
        .get_formatted_public_key("_device_key")
        .await
        .expect("Failed to get device public key");

    auth_settings
        .add_key(
            "_device_key",
            AuthKey::active(&device_pubkey, Permission::Admin(0))
                .expect("Failed to create device key"),
        )
        .expect("Failed to add device key auth");

    // TODO: Add policy configuration that limits new client permissions to Read only
    settings.set("auth", auth_settings.as_doc().clone());

    let server_database = server_user
        .create_database(settings, &server_admin_key_id)
        .await
        .expect("Failed to create database");

    let tree_id = server_database.root_id().clone();

    // Start server
    let server_addr = {
        let server_sync = server_instance.sync().expect("Server should have sync");
        start_sync_server(&server_sync).await
    };

    // Setup client requesting Admin permissions (should be excessive)
    let (client_instance, _client_user, client_key_id) =
        test_instance_with_user_and_key("client_user", Some("greedy_client")).await;
    client_instance
        .enable_sync()
        .await
        .expect("Failed to initialize sync on client");

    let client_sync = client_instance.sync().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .await
        .expect("Failed to enable HTTP transport");

    // Attempt bootstrap requesting Admin permissions (excessive for a new client)
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &tree_id,
            &client_key_id,
            Permission::Admin(0), // Requesting highest admin level
        )
        .await;

    // Flush any pending sync work
    client_sync.flush().await.ok();

    println!("🔍 Bootstrap result requesting excessive Admin permissions: {bootstrap_result:?}");

    // EXPECTED SECURE BEHAVIOR: Bootstrap should fail or limit excessive permission requests
    assert!(
        bootstrap_result.is_err(),
        "Bootstrap should FAIL when requesting excessive Admin permissions - test fails because permission validation is not implemented"
    );

    // EXPECTED SECURE BEHAVIOR: No permissions should be granted for failed bootstrap
    let server_auth_settings = server_database
        .get_settings()
        .await
        .expect("Failed to get server database settings")
        .get_all()
        .await
        .expect("Failed to get all settings");

    if let Some(auth_node) = server_auth_settings.get("auth")
        && let Value::Doc(auth_doc) = auth_node
    {
        // EXPECTED SECURE BEHAVIOR: Greedy client should NOT be in auth config
        assert!(
            !auth_doc.contains_key(&client_key_id),
            "Greedy client should NOT be granted any permissions for excessive request - test fails because permission validation is not implemented"
        );
    }
    println!(
        "✅ TEST: Expected secure behavior for excessive permission requests (will fail until permission validation is implemented)"
    );

    // Cleanup
    let server_sync = server_instance.sync().expect("Server should have sync");
    server_sync.stop_server().await.unwrap();
}
