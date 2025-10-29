//! Integration tests: end-to-end user workflows and realistic scenarios
//!
//! Tests complex multi-user and cross-cutting workflows that span multiple subsystems.
//! For single-component behavior, see the dedicated test modules:
//! - user_lifecycle_tests: User creation, login, logout
//! - key_management_tests: Key operations and persistence
//! - database_operations_tests: Database CRUD operations

use super::helpers::*;
use eidetica::store::DocStore;

// ===== MULTI-USER COLLABORATION SCENARIOS =====

#[test]
fn test_independent_users_coexist() {
    let (instance, _) = setup_instance_with_users(&[
        ("alice", None),
        ("bob", Some("bob_pass")),
        ("charlie", None),
    ]);

    // All users login and create databases
    let mut alice = instance.login_user("alice", None).expect("Alice login");
    let mut bob = instance
        .login_user("bob", Some("bob_pass"))
        .expect("Bob login");
    let mut charlie = instance.login_user("charlie", None).expect("Charlie login");

    let alice_db = create_named_database(&mut alice, "Alice DB");
    let bob_db = create_named_database(&mut bob, "Bob DB");
    let charlie_db = create_named_database(&mut charlie, "Charlie DB");

    // Each writes their own data
    for (db, data) in [
        (&alice_db, "alice_data"),
        (&bob_db, "bob_data"),
        (&charlie_db, "charlie_data"),
    ] {
        let tx = db.new_transaction().expect("Transaction");
        {
            let store = tx.get_store::<DocStore>("data").expect("Store");
            store.set("owner", data).expect("Write");
        }
        tx.commit().expect("Commit");
    }

    // Verify data is independent
    let alice_tx = alice_db.new_transaction().expect("Alice tx");
    let alice_store = alice_tx.get_store::<DocStore>("data").expect("Alice store");
    assert_eq!(
        alice_store.get("owner").expect("Read").as_text(),
        Some("alice_data")
    );

    let bob_tx = bob_db.new_transaction().expect("Bob tx");
    let bob_store = bob_tx.get_store::<DocStore>("data").expect("Bob store");
    assert_eq!(
        bob_store.get("owner").expect("Read").as_text(),
        Some("bob_data")
    );

    let charlie_tx = charlie_db.new_transaction().expect("Charlie tx");
    let charlie_store = charlie_tx
        .get_store::<DocStore>("data")
        .expect("Charlie store");
    assert_eq!(
        charlie_store.get("owner").expect("Read").as_text(),
        Some("charlie_data")
    );
}

// ===== REALISTIC MULTI-DEVICE SCENARIOS =====

#[test]
fn test_multi_device_key_management_and_database_access() {
    let (instance, username) = setup_instance_with_user("alice", None);

    // Session 1: Desktop - Create user's first database with default key
    let mut user1 = instance.login_user(&username, None).expect("Desktop login");
    let default_key = user1.get_default_key().expect("Get default key");

    let db1 = create_named_database(&mut user1, "Shared Notes");
    let db1_id = db1.root_id().clone();

    // Desktop writes some data
    let tx = db1.new_transaction().expect("Desktop write");
    {
        let store = tx.get_store::<DocStore>("notes").expect("Store");
        store.set("note1", "Desktop note").expect("Write");
    }
    tx.commit().expect("Commit");

    user1.logout().expect("Desktop logout");

    // Session 2: Laptop - User adds laptop key and creates another database
    let mut user2 = instance.login_user(&username, None).expect("Laptop login");
    let laptop_key = add_user_key(&mut user2, Some("Laptop"));

    let db2 = user2
        .create_database(
            {
                let mut settings = eidetica::crdt::Doc::new();
                settings.set_string("name", "Laptop Work");
                settings
            },
            &laptop_key,
        )
        .expect("Create laptop database");
    let db2_id = db2.root_id().clone();

    // Laptop can also access the shared database (created with default key)
    let db1_from_laptop = user2.open_database(&db1_id).expect("Load shared db");
    let tx2 = db1_from_laptop.new_transaction().expect("Laptop read");
    let store2 = tx2.get_store::<DocStore>("notes").expect("Store");
    assert_eq!(
        store2.get("note1").expect("Read").as_text(),
        Some("Desktop note")
    );

    user2.logout().expect("Laptop logout");

    // Session 3: Phone - User adds phone key and verifies access to both databases
    let mut user3 = instance.login_user(&username, None).expect("Phone login");
    let _phone_key = add_user_key(&mut user3, Some("Phone"));

    // Phone should have 3 keys now (default, laptop, phone)
    assert_user_key_count(&user3, 3);
    assert_user_has_key(&user3, &default_key);
    assert_user_has_key(&user3, &laptop_key);

    // Phone can access both databases through their respective keys
    let db1_from_phone = user3
        .open_database(&db1_id)
        .expect("Load shared db from phone");
    assert_database_name(&db1_from_phone, "Shared Notes");

    let db2_from_phone = user3
        .open_database(&db2_id)
        .expect("Load laptop db from phone");
    assert_database_name(&db2_from_phone, "Laptop Work");
}

#[test]
fn test_team_scenario_multiple_users_own_databases() {
    let (instance, _) =
        setup_instance_with_users(&[("alice", None), ("bob", None), ("charlie", None)]);

    // Each team member creates their own project database
    let mut alice = instance.login_user("alice", None).expect("Alice login");
    let mut bob = instance.login_user("bob", None).expect("Bob login");
    let mut charlie = instance.login_user("charlie", None).expect("Charlie login");

    let alice_project = create_named_database(&mut alice, "Frontend");
    let bob_project = create_named_database(&mut bob, "Backend");
    let charlie_project = create_named_database(&mut charlie, "Database");

    // Each adds project-specific data
    for (db, component, progress) in [
        (&alice_project, "React", 75),
        (&bob_project, "API", 50),
        (&charlie_project, "Schema", 90),
    ] {
        let tx = db.new_transaction().expect("Transaction");
        {
            let store = tx.get_store::<DocStore>("status").expect("Store");
            store.set("component", component).expect("Write component");
            store.set("progress", progress).expect("Write progress");
        }
        tx.commit().expect("Commit");
    }

    // Verify each has their own data
    let alice_tx = alice_project.new_transaction().expect("Alice tx");
    let alice_store = alice_tx
        .get_store::<DocStore>("status")
        .expect("Alice store");
    assert_eq!(
        alice_store.get("component").expect("Read").as_text(),
        Some("React")
    );
    assert_eq!(alice_store.get("progress").expect("Read"), 75);

    let bob_tx = bob_project.new_transaction().expect("Bob tx");
    let bob_store = bob_tx.get_store::<DocStore>("status").expect("Bob store");
    assert_eq!(
        bob_store.get("component").expect("Read").as_text(),
        Some("API")
    );
    assert_eq!(bob_store.get("progress").expect("Read"), 50);

    let charlie_tx = charlie_project.new_transaction().expect("Charlie tx");
    let charlie_store = charlie_tx
        .get_store::<DocStore>("status")
        .expect("Charlie store");
    assert_eq!(
        charlie_store.get("component").expect("Read").as_text(),
        Some("Schema")
    );
    assert_eq!(charlie_store.get("progress").expect("Read"), 90);
}

// ===== GLOBAL PERMISSION COLLABORATIVE DATABASE SCENARIOS =====

#[test]
fn test_collaborative_database_with_global_permissions() {
    use eidetica::{
        Database,
        auth::{
            Permission,
            settings::AuthSettings,
            types::{AuthKey, SigKey},
        },
        crdt::Doc,
    };

    println!("\n🧪 TEST: End-to-end collaborative database with global Write permissions");

    // Setup two users on the same instance
    let alice_name = "alice";
    let bob_name = "bob";
    let (instance, _) = setup_instance_with_users(&[(alice_name, None), (bob_name, None)]);

    // Alice logs in and creates a database with global Write(10) permission
    let mut alice = login_user(&instance, alice_name, None);
    let alice_key = alice.get_default_key().expect("Alice get default key");

    let mut alice_db_settings = Doc::new();
    alice_db_settings.set_string("name", "Team Workspace");

    // Configure auth settings with global Write permission
    let mut auth_settings = AuthSettings::new();

    // Add Alice's admin key
    let alice_pubkey = alice.get_public_key(&alice_key).expect("Alice public key");
    auth_settings
        .add_key(
            &alice_key,
            AuthKey::active(&alice_pubkey, Permission::Admin(1)).unwrap(),
        )
        .unwrap();

    // Add global Write(10) permission - anyone can access with Write permission
    auth_settings
        .add_key("*", AuthKey::active("*", Permission::Write(10)).unwrap())
        .unwrap();

    alice_db_settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the new database with alice_key as the owner
    let alice_db = alice
        .create_database(alice_db_settings, &alice_key)
        .expect("Alice creates database");
    let db_id = alice_db.root_id().clone();

    // Alice writes initial data
    {
        let tx = alice_db.new_transaction().expect("Alice transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        store.set("project", "Eidetica").expect("Write project");
        store
            .set("status", "Alice started the workspace")
            .expect("Write status");
        tx.commit().expect("Alice commits");
    }
    println!("✅ Alice created database with global Write(10) permission and added initial data");

    alice.logout().expect("Alice logout");

    // Bob logs in and discovers he can access the database
    let mut bob = login_user(&instance, bob_name, None);
    let bob_key = bob.get_default_key().expect("Bob get default key");

    // Bob discovers available SigKeys for his public key
    let bob_pubkey = bob.get_public_key(&bob_key).expect("Bob public key");

    let sigkeys =
        Database::find_sigkeys(&instance, &db_id, &bob_pubkey).expect("Bob discovers SigKeys");

    // Should find the global "*" permission
    assert!(!sigkeys.is_empty(), "Bob should find at least one SigKey");
    let (sigkey, permission) = &sigkeys[0];

    // Verify it's the global permission
    assert_eq!(
        sigkey,
        &SigKey::Direct("*".to_string()),
        "Should discover global permission"
    );
    assert_eq!(
        permission,
        &Permission::Write(10),
        "Should have Write(10) permission"
    );
    println!("✅ Bob discovered global '*' permission with Write(10)");

    // Bob adds the database key mapping to his user preferences
    bob.map_key(&bob_key, &db_id, "*")
        .expect("Bob adds database key mapping");
    println!("✅ Bob configured key mapping for the database");

    // Bob loads the database
    let bob_db = bob.open_database(&db_id).expect("Bob loads database");
    assert_database_name(&bob_db, "Team Workspace");
    println!("✅ Bob successfully loaded the database");

    // Bob reads Alice's data
    {
        let tx = bob_db.new_transaction().expect("Bob read transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        assert_eq!(
            store.get("project").expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("status").expect("Read").as_text(),
            Some("Alice started the workspace")
        );
    }
    println!("✅ Bob read Alice's data successfully");

    // Bob makes changes (Write permission allows this)
    {
        let tx = bob_db.new_transaction().expect("Bob write transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        store.set("contributor", "Bob").expect("Write contributor");
        store
            .set("status", "Bob joined and contributed")
            .expect("Update status");
        tx.commit().expect("Bob commits changes");
    }
    println!("✅ Bob committed changes successfully");

    bob.logout().expect("Bob logout");

    // Alice logs back in and sees Bob's changes
    let alice2 = login_user(&instance, alice_name, None);
    let alice_db2 = alice2
        .open_database(&db_id)
        .expect("Alice reloads database");

    {
        let tx = alice_db2.new_transaction().expect("Alice read transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        assert_eq!(
            store.get("project").expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("contributor").expect("Read").as_text(),
            Some("Bob")
        );
        assert_eq!(
            store.get("status").expect("Read").as_text(),
            Some("Bob joined and contributed")
        );
    }
    println!("✅ Alice sees Bob's changes - collaboration successful!");

    println!("✅ End-to-end collaborative database flow complete");
}

#[tokio::test]
async fn test_collaborative_database_with_sync_and_global_permissions() {
    use eidetica::{
        Database, Instance,
        auth::{
            Permission,
            settings::AuthSettings,
            types::{AuthKey, SigKey},
        },
        backend::database::InMemory,
        crdt::Doc,
    };
    use std::time::Duration;

    println!(
        "\n🧪 TEST: End-to-end User API collaborative database with sync and global permissions"
    );

    // === ALICE'S INSTANCE (Server) ===
    println!("\n👤 Setting up Alice's instance...");
    let alice_instance =
        Instance::open(Box::new(InMemory::new())).expect("Failed to create Alice's instance");
    alice_instance
        .enable_sync()
        .expect("Failed to enable sync for Alice");

    // Create Alice's user account
    alice_instance
        .create_user("alice", None)
        .expect("Failed to create Alice");
    let mut alice = alice_instance
        .login_user("alice", None)
        .expect("Failed to login Alice");
    let alice_key = alice.get_default_key().expect("Alice get default key");

    // Alice creates a database with global Write(10) permission
    println!("📁 Alice creating collaborative database...");
    let mut alice_db_settings = Doc::new();
    alice_db_settings.set_string("name", "Team Workspace");

    let mut auth_settings = AuthSettings::new();

    // Add Alice's admin key
    let alice_pubkey = alice.get_public_key(&alice_key).expect("Alice public key");
    auth_settings
        .add_key(
            &alice_key,
            AuthKey::active(&alice_pubkey, Permission::Admin(1)).unwrap(),
        )
        .unwrap();

    // Add global Write(10) permission - anyone can access
    auth_settings
        .add_key("*", AuthKey::active("*", Permission::Write(10)).unwrap())
        .unwrap();

    alice_db_settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the new database with alice_key as the owner
    let alice_db = alice
        .create_database(alice_db_settings, &alice_key)
        .expect("Alice creates database");
    let db_id = alice_db.root_id().clone();

    // Alice writes initial data
    {
        let tx = alice_db.new_transaction().expect("Alice transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        store.set("project", "Eidetica").expect("Write project");
        store
            .set("status", "Alice started the workspace")
            .expect("Write status");
        tx.commit().expect("Alice commits");
    }
    println!(
        "✅ Alice created database {} with global Write(10) permission",
        db_id
    );

    // Enable sync for this database
    use eidetica::user::types::{DatabasePreferences, SyncSettings};
    alice
        .add_database(DatabasePreferences {
            database_id: db_id.clone(),
            key_id: alice_key.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .expect("Failed to add database to Alice's preferences");

    // Sync the user database to update combined settings
    let alice_sync = alice_instance.sync().expect("Alice should have sync");
    alice_sync
        .sync_user(alice.user_uuid(), alice.user_database().root_id())
        .expect("Failed to sync Alice's user database");

    // Alice starts sync server
    let server_addr = {
        let alice_sync = alice_instance.sync().expect("Alice should have sync");
        alice_sync
            .enable_http_transport()
            .expect("Failed to enable HTTP transport");
        alice_sync
            .start_server_async("127.0.0.1:0")
            .await
            .expect("Failed to start Alice's server");
        let addr = alice_sync
            .get_server_address_async()
            .await
            .expect("Failed to get server address");
        println!("🌐 Alice's server listening at: {}", addr);
        addr
    };

    alice.logout().expect("Alice logout");

    // === BOB'S INSTANCE (Client) ===
    println!("\n👤 Setting up Bob's instance (separate from Alice)...");
    let bob_instance =
        Instance::open(Box::new(InMemory::new())).expect("Failed to create Bob's instance");
    bob_instance
        .enable_sync()
        .expect("Failed to enable sync for Bob");

    // Create Bob's user account
    bob_instance
        .create_user("bob", None)
        .expect("Failed to create Bob");
    let mut bob = bob_instance
        .login_user("bob", None)
        .expect("Failed to login Bob");
    let bob_key = bob.get_default_key().expect("Bob get default key");

    // Bob syncs with Alice's server to bootstrap the database
    println!("\n🔄 Bob syncing with Alice's server to get the database...");
    {
        let bob_sync = bob_instance.sync().expect("Bob should have sync");
        bob_sync
            .enable_http_transport()
            .expect("Failed to enable HTTP transport");

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

    let sigkeys =
        Database::find_sigkeys(&bob_instance, &db_id, &bob_pubkey).expect("Bob discovers SigKeys");

    // Should find the global "*" permission
    assert!(!sigkeys.is_empty(), "Bob should find at least one SigKey");
    let (sigkey, permission) = &sigkeys[0];
    assert_eq!(
        sigkey,
        &SigKey::Direct("*".to_string()),
        "Should discover global permission"
    );
    assert_eq!(permission, &Permission::Write(10));
    println!("✅ Bob discovered global '*' permission with Write(10)");

    // Bob adds the database key mapping to his user preferences
    bob.map_key(&bob_key, &db_id, "*")
        .expect("Bob adds database key mapping");
    println!("✅ Bob configured key mapping for the database");

    // Bob loads the database
    let bob_db = bob.open_database(&db_id).expect("Bob loads database");
    println!("✅ Bob successfully loaded the database");

    // Bob reads Alice's data
    {
        let tx = bob_db.new_transaction().expect("Bob read transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        assert_eq!(
            store.get("project").expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("status").expect("Read").as_text(),
            Some("Alice started the workspace")
        );
    }
    println!("✅ Bob read Alice's data successfully");

    // Bob makes changes
    {
        let tx = bob_db.new_transaction().expect("Bob write transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        store.set("contributor", "Bob").expect("Write contributor");
        store
            .set("status", "Bob joined and contributed")
            .expect("Update status");
        tx.commit().expect("Bob commits changes");
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
        .expect("Alice re-login");
    let alice_db2 = alice2
        .open_database(&db_id)
        .expect("Alice reloads database");

    {
        let tx = alice_db2.new_transaction().expect("Alice read transaction");
        let store = tx.get_store::<DocStore>("team_notes").expect("Store");
        assert_eq!(
            store.get("project").expect("Read").as_text(),
            Some("Eidetica")
        );
        assert_eq!(
            store.get("contributor").expect("Read").as_text(),
            Some("Bob"),
            "Alice should see Bob's contribution"
        );
        assert_eq!(
            store.get("status").expect("Read").as_text(),
            Some("Bob joined and contributed"),
            "Alice should see Bob's status update"
        );
    }
    println!("✅ Alice sees Bob's changes - bidirectional sync successful!");

    // Cleanup
    {
        let alice_sync = alice_instance.sync().expect("Alice should have sync");
        alice_sync
            .stop_server_async()
            .await
            .expect("Failed to stop Alice's server");
    }

    println!("\n✅ TEST COMPLETED: User API end-to-end collaborative database with sync works!");
}
