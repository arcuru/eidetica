//! Global Permission Transaction Tests
//!
//! This module contains progressive tests to diagnose and verify global permission
//! authentication in transactions. Tests are organized by complexity level.

use eidetica::{
    Instance,
    auth::{
        crypto::{format_public_key, generate_keypair},
        types::{AuthKey, KeyStatus, Permission},
    },
    backend::database::InMemory,
    crdt::Doc,
    store::DocStore,
};

/// Helper to create a database with global "*" permission configured
fn setup_database_with_global_permission() -> (Instance, eidetica::Database, String) {
    // Create instance with backend
    let instance = Instance::new(Box::new(InMemory::new()));

    // Generate a keypair for the client using global permission
    let (signing_key, verifying_key) = generate_keypair();
    let public_key_str = format_public_key(&verifying_key);

    // Store the private key for the "*" global key
    instance
        .backend()
        .store_private_key("*", signing_key)
        .expect("Failed to store private key");

    // Create database settings with global "*" permission
    let mut settings = Doc::new();
    let mut auth_section = Doc::new();

    // Add global permission key
    let global_auth_key = AuthKey {
        pubkey: "*".to_string(), // Wildcard pubkey means "accept any valid key"
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };
    auth_section
        .set_json("*", &global_auth_key)
        .expect("Failed to set global auth key");

    settings.set_node("auth", auth_section);

    // Create database (we need an admin key to create the database initially)
    let (admin_signing_key, admin_verifying_key) = generate_keypair();
    let admin_public_key_str = format_public_key(&admin_verifying_key);
    instance
        .backend()
        .store_private_key("admin_key", admin_signing_key)
        .expect("Failed to store admin key");

    // Add admin key to auth settings for database creation
    let mut auth_section = match settings.get("auth") {
        Some(eidetica::crdt::doc::Value::Node(node)) => node.clone(),
        _ => panic!("Expected auth section to be a node"),
    };
    let admin_auth_key = AuthKey {
        pubkey: admin_public_key_str,
        permissions: Permission::Admin(1),
        status: KeyStatus::Active,
    };
    auth_section
        .set_json("admin_key", &admin_auth_key)
        .expect("Failed to set admin auth key");
    settings.set_node("auth", auth_section);

    let database = instance
        .new_database(settings, "admin_key")
        .expect("Failed to create database");

    (instance, database, public_key_str)
}

#[test]
fn test_level_1_transaction_builds_entry_with_pubkey() {
    println!("🧪 LEVEL 1: Testing transaction builds entry with pubkey for global permission");

    let (_instance, mut database, expected_pubkey) = setup_database_with_global_permission();

    // Configure database to use global permission for transactions
    database.set_default_auth_key("*");

    // Create a transaction
    let transaction = database
        .new_transaction()
        .expect("Should create transaction with global permission");

    // Add some data to the transaction
    let store = transaction
        .get_store::<DocStore>("test_data")
        .expect("Failed to get test store");
    store.set("key", "value").expect("Failed to set test data");

    // Build the entry but don't commit yet
    // We need to access the transaction internals to get the built entry
    // This is tricky since Transaction doesn't expose the built entry directly

    // For now, let's try to commit and catch what happens
    match transaction.commit() {
        Ok(_) => {
            println!(
                "✅ LEVEL 1 PASSED: Transaction with global permission committed successfully"
            );
        }
        Err(e) => {
            println!("❌ LEVEL 1 FAILED: {}", e);
            println!("Expected pubkey: {}", expected_pubkey);

            // This will help us understand what went wrong
            panic!("Level 1 test failed: {}", e);
        }
    }
}
