//! Integration tests: end-to-end user workflows and realistic scenarios
//!
//! Tests complex multi-user and cross-cutting workflows that span multiple subsystems.
//! For single-component behavior, see the dedicated test modules:
//! - user_lifecycle_tests: User creation, login, logout
//! - key_management_tests: Key operations and persistence
//! - database_operations_tests: Database CRUD operations

use super::helpers::*;
use crate::helpers::{add_auth_keys, test_instance};
use eidetica::{
    Database,
    auth::{
        Permission,
        types::{AuthKey, SigKey},
    },
    crdt::Doc,
    store::DocStore,
    sync::{Address, transports::http::HttpTransport},
    user::types::{SyncSettings, TrackedDatabase},
};

// ===== MULTI-USER COLLABORATION SCENARIOS =====

#[tokio::test]
async fn test_independent_users_coexist() {
    let (instance, _) = setup_instance_with_users(&[
        ("alice", None),
        ("bob", Some("bob_pass")),
        ("charlie", None),
    ])
    .await;

    // All users login and create databases
    let mut alice = instance
        .login_user("alice", None)
        .await
        .expect("Alice login");
    let mut bob = instance
        .login_user("bob", Some("bob_pass"))
        .await
        .expect("Bob login");
    let mut charlie = instance
        .login_user("charlie", None)
        .await
        .expect("Charlie login");

    let alice_db = create_named_database(&mut alice, "Alice DB").await;
    let bob_db = create_named_database(&mut bob, "Bob DB").await;
    let charlie_db = create_named_database(&mut charlie, "Charlie DB").await;

    // Each writes their own data
    for (db, data) in [
        (&alice_db, "alice_data"),
        (&bob_db, "bob_data"),
        (&charlie_db, "charlie_data"),
    ] {
        let tx = db.new_transaction().await.expect("Transaction");
        {
            let store = tx.get_store::<DocStore>("data").await.expect("Store");
            store.set("owner", data).await.expect("Write");
        }
        tx.commit().await.expect("Commit");
    }

    // Verify data is independent
    let alice_tx = alice_db.new_transaction().await.expect("Alice tx");
    let alice_store = alice_tx
        .get_store::<DocStore>("data")
        .await
        .expect("Alice store");
    assert_eq!(
        alice_store.get("owner").await.expect("Read").as_text(),
        Some("alice_data")
    );

    let bob_tx = bob_db.new_transaction().await.expect("Bob tx");
    let bob_store = bob_tx
        .get_store::<DocStore>("data")
        .await
        .expect("Bob store");
    assert_eq!(
        bob_store.get("owner").await.expect("Read").as_text(),
        Some("bob_data")
    );

    let charlie_tx = charlie_db.new_transaction().await.expect("Charlie tx");
    let charlie_store = charlie_tx
        .get_store::<DocStore>("data")
        .await
        .expect("Charlie store");
    assert_eq!(
        charlie_store.get("owner").await.expect("Read").as_text(),
        Some("charlie_data")
    );
}

// ===== REALISTIC MULTI-DEVICE SCENARIOS =====

#[tokio::test]
async fn test_multi_device_key_management_and_database_access() {
    let (instance, username) = setup_instance_with_user("alice", None).await;

    // Session 1: Desktop - Create user's first database with default key
    let mut user1 = instance
        .login_user(&username, None)
        .await
        .expect("Desktop login");
    let default_key = user1.get_default_key().expect("Get default key");

    let db1 = create_named_database(&mut user1, "Shared Notes").await;
    let db1_id = db1.root_id().clone();

    // Desktop writes some data
    let tx = db1.new_transaction().await.expect("Desktop write");
    {
        let store = tx.get_store::<DocStore>("notes").await.expect("Store");
        store.set("note1", "Desktop note").await.expect("Write");
    }
    tx.commit().await.expect("Commit");

    user1.logout().expect("Desktop logout");

    // Session 2: Laptop - User adds laptop key and creates another database
    let mut user2 = instance
        .login_user(&username, None)
        .await
        .expect("Laptop login");
    let laptop_key = add_user_key(&mut user2, Some("Laptop")).await;

    let db2 = user2
        .create_database(
            {
                let mut settings = Doc::new();
                settings.set("name", "Laptop Work");
                settings
            },
            &laptop_key,
        )
        .await
        .expect("Create laptop database");
    let db2_id = db2.root_id().clone();

    // Laptop can also access the shared database (created with default key)
    let db1_from_laptop = user2.open_database(&db1_id).await.expect("Load shared db");
    let tx2 = db1_from_laptop
        .new_transaction()
        .await
        .expect("Laptop read");
    let store2 = tx2.get_store::<DocStore>("notes").await.expect("Store");
    assert_eq!(
        store2.get("note1").await.expect("Read").as_text(),
        Some("Desktop note")
    );

    user2.logout().expect("Laptop logout");

    // Session 3: Phone - User adds phone key and verifies access to both databases
    let mut user3 = instance
        .login_user(&username, None)
        .await
        .expect("Phone login");
    let _phone_key = add_user_key(&mut user3, Some("Phone")).await;

    // Phone should have 3 keys now (default, laptop, phone)
    assert_user_key_count(&user3, 3);
    assert_user_has_key(&user3, &default_key);
    assert_user_has_key(&user3, &laptop_key);

    // Phone can access both databases through their respective keys
    let db1_from_phone = user3
        .open_database(&db1_id)
        .await
        .expect("Load shared db from phone");
    assert_database_name(&db1_from_phone, "Shared Notes").await;

    let db2_from_phone = user3
        .open_database(&db2_id)
        .await
        .expect("Load laptop db from phone");
    assert_database_name(&db2_from_phone, "Laptop Work").await;
}

#[tokio::test]
async fn test_team_scenario_multiple_users_own_databases() {
    let (instance, _) =
        setup_instance_with_users(&[("alice", None), ("bob", None), ("charlie", None)]).await;

    // Each team member creates their own project database
    let mut alice = instance
        .login_user("alice", None)
        .await
        .expect("Alice login");
    let mut bob = instance.login_user("bob", None).await.expect("Bob login");
    let mut charlie = instance
        .login_user("charlie", None)
        .await
        .expect("Charlie login");

    let alice_project = create_named_database(&mut alice, "Frontend").await;
    let bob_project = create_named_database(&mut bob, "Backend").await;
    let charlie_project = create_named_database(&mut charlie, "Database").await;

    // Each adds project-specific data
    for (db, component, progress) in [
        (&alice_project, "React", 75),
        (&bob_project, "API", 50),
        (&charlie_project, "Schema", 90),
    ] {
        let tx = db.new_transaction().await.expect("Transaction");
        {
            let store = tx.get_store::<DocStore>("status").await.expect("Store");
            store
                .set("component", component)
                .await
                .expect("Write component");
            store
                .set("progress", progress)
                .await
                .expect("Write progress");
        }
        tx.commit().await.expect("Commit");
    }

    // Verify each has their own data
    let alice_tx = alice_project.new_transaction().await.expect("Alice tx");
    let alice_store = alice_tx
        .get_store::<DocStore>("status")
        .await
        .expect("Alice store");
    assert_eq!(
        alice_store.get("component").await.expect("Read").as_text(),
        Some("React")
    );
    assert_eq!(alice_store.get("progress").await.expect("Read"), 75);

    let bob_tx = bob_project.new_transaction().await.expect("Bob tx");
    let bob_store = bob_tx
        .get_store::<DocStore>("status")
        .await
        .expect("Bob store");
    assert_eq!(
        bob_store.get("component").await.expect("Read").as_text(),
        Some("API")
    );
    assert_eq!(bob_store.get("progress").await.expect("Read"), 50);

    let charlie_tx = charlie_project.new_transaction().await.expect("Charlie tx");
    let charlie_store = charlie_tx
        .get_store::<DocStore>("status")
        .await
        .expect("Charlie store");
    assert_eq!(
        charlie_store
            .get("component")
            .await
            .expect("Read")
            .as_text(),
        Some("Schema")
    );
    assert_eq!(charlie_store.get("progress").await.expect("Read"), 90);
}

// ===== GLOBAL PERMISSION COLLABORATIVE DATABASE SCENARIOS =====

#[tokio::test]
async fn test_collaborative_database_with_global_permissions() {
    println!("\n🧪 TEST: End-to-end collaborative database with global Write permissions");

    // Setup two users on the same instance
    let alice_name = "alice";
    let bob_name = "bob";
    let (instance, _) = setup_instance_with_users(&[(alice_name, None), (bob_name, None)]).await;

    // Alice logs in and creates a database with global Write(10) permission
    let mut alice = login_user(&instance, alice_name, None).await;
    let alice_key = alice.get_default_key().expect("Alice get default key");

    let mut alice_db_settings = Doc::new();
    alice_db_settings.set("name", "Team Workspace");

    // Create the new database with alice_key as the owner
    let alice_db = alice
        .create_database(alice_db_settings, &alice_key)
        .await
        .expect("Alice creates database");
    let db_id = alice_db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_keys(
        &alice_db,
        &[("*", AuthKey::active(Some("*"), Permission::Write(10)))],
    )
    .await;

    // Alice writes initial data
    {
        let tx = alice_db.new_transaction().await.expect("Alice transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        store
            .set("project", "Eidetica")
            .await
            .expect("Write project");
        store
            .set("status", "Alice started the workspace")
            .await
            .expect("Write status");
        tx.commit().await.expect("Alice commits");
    }
    println!("✅ Alice created database with global Write(10) permission and added initial data");

    alice.logout().expect("Alice logout");

    // Bob logs in and discovers he can access the database
    let mut bob = login_user(&instance, bob_name, None).await;
    let bob_key = bob.get_default_key().expect("Bob get default key");

    // Bob discovers available SigKeys for his public key
    let bob_pubkey = bob.get_public_key(&bob_key).expect("Bob public key");

    let sigkeys = Database::find_sigkeys(&instance, &db_id, &bob_pubkey)
        .await
        .expect("Bob discovers SigKeys");

    // Should find the global "*" permission
    assert!(!sigkeys.is_empty(), "Bob should find at least one SigKey");
    let (sigkey, permission) = &sigkeys[0];

    // Verify it's the global permission (encoded as "*:ed25519:..." in pubkey field)
    assert!(sigkey.is_global(), "Should discover global permission");
    assert_eq!(
        permission,
        &Permission::Write(10),
        "Should have Write(10) permission"
    );
    println!("✅ Bob discovered global permission with Write(10)");

    // Get the actual sigkey string for mapping (will be "*:ed25519:...")
    let sigkey_str = match sigkey {
        SigKey::Direct(hint) => hint
            .pubkey
            .clone()
            .or(hint.name.clone())
            .expect("Should have pubkey or name"),
        _ => panic!("Expected Direct SigKey"),
    };

    // Bob adds the database key mapping to his user preferences
    bob.map_key(&bob_key, &db_id, &sigkey_str)
        .await
        .expect("Bob adds database key mapping");
    println!("✅ Bob configured key mapping for the database");

    // Bob loads the database
    let bob_db = bob.open_database(&db_id).await.expect("Bob loads database");
    assert_database_name(&bob_db, "Team Workspace").await;
    println!("✅ Bob successfully loaded the database");

    // Bob reads Alice's data
    {
        let tx = bob_db
            .new_transaction()
            .await
            .expect("Bob read transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        assert_eq!(
            store.get("project").await.expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("status").await.expect("Read").as_text(),
            Some("Alice started the workspace")
        );
    }
    println!("✅ Bob read Alice's data successfully");

    // Bob makes changes (Write permission allows this)
    {
        let tx = bob_db
            .new_transaction()
            .await
            .expect("Bob write transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        store
            .set("contributor", "Bob")
            .await
            .expect("Write contributor");
        store
            .set("status", "Bob joined and contributed")
            .await
            .expect("Update status");
        tx.commit().await.expect("Bob commits changes");
    }
    println!("✅ Bob committed changes successfully");

    bob.logout().expect("Bob logout");

    // Alice logs back in and sees Bob's changes
    let alice2 = login_user(&instance, alice_name, None).await;
    let alice_db2 = alice2
        .open_database(&db_id)
        .await
        .expect("Alice reloads database");

    {
        let tx = alice_db2
            .new_transaction()
            .await
            .expect("Alice read transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        assert_eq!(
            store.get("project").await.expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("contributor").await.expect("Read").as_text(),
            Some("Bob")
        );
        assert_eq!(
            store.get("status").await.expect("Read").as_text(),
            Some("Bob joined and contributed")
        );
    }
    println!("✅ Alice sees Bob's changes - collaboration successful!");

    println!("✅ End-to-end collaborative database flow complete");
}

#[tokio::test]
async fn test_collaborative_database_with_sync_and_global_permissions() {
    use std::time::Duration;

    println!(
        "\n🧪 TEST: End-to-end User API collaborative database with sync and global permissions"
    );

    // === ALICE'S INSTANCE (Server) ===
    println!("\n👤 Setting up Alice's instance...");
    let alice_instance = test_instance().await;
    alice_instance
        .enable_sync()
        .await
        .expect("Failed to enable sync for Alice");

    // Create Alice's user account
    alice_instance
        .create_user("alice", None)
        .await
        .expect("Failed to create Alice");
    let mut alice = alice_instance
        .login_user("alice", None)
        .await
        .expect("Failed to login Alice");
    let alice_key = alice.get_default_key().expect("Alice get default key");

    // Alice creates a database with global Write(10) permission
    println!("📁 Alice creating collaborative database...");
    let mut alice_db_settings = Doc::new();
    alice_db_settings.set("name", "Team Workspace");

    // Create the new database with alice_key as the owner
    let alice_db = alice
        .create_database(alice_db_settings, &alice_key)
        .await
        .expect("Alice creates database");
    let db_id = alice_db.root_id().clone();

    // Add global Write permission (signing key is already Admin(0))
    add_auth_keys(
        &alice_db,
        &[("*", AuthKey::active(Some("*"), Permission::Write(10)))],
    )
    .await;

    // Alice writes initial data
    {
        let tx = alice_db.new_transaction().await.expect("Alice transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        store
            .set("project", "Eidetica")
            .await
            .expect("Write project");
        store
            .set("status", "Alice started the workspace")
            .await
            .expect("Write status");
        tx.commit().await.expect("Alice commits");
    }
    println!("✅ Alice created database {db_id} with global Write(10) permission");

    // Enable sync for this database
    alice
        .track_database(TrackedDatabase {
            database_id: db_id.clone(),
            key_id: alice_key.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .expect("Failed to add database to Alice's preferences");

    // Sync the user database to update combined settings
    let alice_sync = alice_instance.sync().expect("Alice should have sync");
    alice_sync
        .sync_user(alice.user_uuid(), alice.user_database().root_id())
        .await
        .expect("Failed to sync Alice's user database");

    // Alice starts sync server
    let server_addr = {
        let alice_sync = alice_instance.sync().expect("Alice should have sync");
        alice_sync
            .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
            .await
            .expect("Failed to register HTTP transport");
        alice_sync
            .accept_connections()
            .await
            .expect("Failed to start Alice's server");
        let bare_addr = alice_sync
            .get_server_address()
            .await
            .expect("Failed to get server address");
        let addr = Address::http(&bare_addr);
        println!("🌐 Alice's server listening at: {bare_addr}");
        addr
    };

    alice.logout().expect("Alice logout");

    // === BOB'S INSTANCE (Client) ===
    println!("\n👤 Setting up Bob's instance (separate from Alice)...");
    let bob_instance = test_instance().await;
    bob_instance
        .enable_sync()
        .await
        .expect("Failed to enable sync for Bob");

    // Create Bob's user account
    bob_instance
        .create_user("bob", None)
        .await
        .expect("Failed to create Bob");
    let mut bob = bob_instance
        .login_user("bob", None)
        .await
        .expect("Failed to login Bob");
    let bob_key = bob.get_default_key().expect("Bob get default key");

    // Bob syncs with Alice's server to bootstrap the database
    println!("\n🔄 Bob syncing with Alice's server to get the database...");
    {
        let bob_sync = bob_instance.sync().expect("Bob should have sync");
        bob_sync
            .register_transport("http", HttpTransport::builder())
            .await
            .expect("Failed to register HTTP transport");

        // Bootstrap sync - this will get the database from Alice
        bob_sync
            .sync_with_peer(&server_addr, Some(&db_id))
            .await
            .expect("Bob should sync successfully");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("✅ Bob synced with Alice's server");

    // Bob discovers available SigKeys for his public key
    println!("\n🔍 Bob discovering available SigKeys...");
    let bob_pubkey = bob.get_public_key(&bob_key).expect("Bob public key");

    let sigkeys = Database::find_sigkeys(&bob_instance, &db_id, &bob_pubkey)
        .await
        .expect("Bob discovers SigKeys");

    // Should find the global permission (encoded as "*:ed25519:..." in pubkey field)
    assert!(!sigkeys.is_empty(), "Bob should find at least one SigKey");
    let (sigkey, permission) = &sigkeys[0];
    assert!(sigkey.is_global(), "Should discover global permission");
    assert_eq!(permission, &Permission::Write(10));
    println!("✅ Bob discovered global permission with Write(10)");

    // Get the actual sigkey string for mapping (will be "*:ed25519:...")
    let sigkey_str = match sigkey {
        SigKey::Direct(hint) => hint
            .pubkey
            .clone()
            .or(hint.name.clone())
            .expect("Should have pubkey or name"),
        _ => panic!("Expected Direct SigKey"),
    };

    // Bob adds the database key mapping to his user preferences
    bob.map_key(&bob_key, &db_id, &sigkey_str)
        .await
        .expect("Bob adds database key mapping");
    println!("✅ Bob configured key mapping for the database");

    // Bob loads the database
    let bob_db = bob.open_database(&db_id).await.expect("Bob loads database");
    println!("✅ Bob successfully loaded the database");

    // Bob reads Alice's data
    {
        let tx = bob_db
            .new_transaction()
            .await
            .expect("Bob read transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        assert_eq!(
            store.get("project").await.expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("status").await.expect("Read").as_text(),
            Some("Alice started the workspace")
        );
    }
    println!("✅ Bob read Alice's data successfully");

    // Bob makes changes
    {
        let tx = bob_db
            .new_transaction()
            .await
            .expect("Bob write transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        store
            .set("contributor", "Bob")
            .await
            .expect("Write contributor");
        store
            .set("status", "Bob joined and contributed")
            .await
            .expect("Update status");
        tx.commit().await.expect("Bob commits changes");
    }
    println!("✅ Bob committed changes successfully");

    // Bob syncs changes back to Alice's server
    println!("\n🔄 Bob syncing changes back to Alice...");
    {
        let bob_sync = bob_instance.sync().expect("Bob should have sync");
        bob_sync
            .sync_with_peer(&server_addr, Some(&db_id))
            .await
            .expect("Bob should sync back successfully");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("✅ Bob synced changes back to Alice");

    bob.logout().expect("Bob logout");

    // === ALICE VERIFIES BOB'S CHANGES ===
    println!("\n🔍 Alice logging back in to verify Bob's changes...");
    let alice2 = alice_instance
        .login_user("alice", None)
        .await
        .expect("Alice re-login");
    let alice_db2 = alice2
        .open_database(&db_id)
        .await
        .expect("Alice reloads database");

    {
        let tx = alice_db2
            .new_transaction()
            .await
            .expect("Alice read transaction");
        let store = tx.get_store::<DocStore>("team_notes").await.expect("Store");
        assert_eq!(
            store.get("project").await.expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("contributor").await.expect("Read").as_text(),
            Some("Bob"),
            "Alice should see Bob's contribution"
        );
        assert_eq!(
            store.get("status").await.expect("Read").as_text(),
            Some("Bob joined and contributed"),
            "Alice should see Bob's status update"
        );
    }
    println!("✅ Alice sees Bob's changes - bidirectional sync successful!");

    // Cleanup
    {
        let alice_sync = alice_instance.sync().expect("Alice should have sync");
        alice_sync
            .stop_server()
            .await
            .expect("Failed to stop Alice's server");
    }

    println!("\n✅ TEST COMPLETED: User API end-to-end collaborative database with sync works!");
}
