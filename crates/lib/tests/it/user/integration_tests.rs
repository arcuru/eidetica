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
        .new_database(
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
    let db1_from_laptop = user2.load_database(&db1_id).expect("Load shared db");
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
        .load_database(&db1_id)
        .expect("Load shared db from phone");
    assert_database_name(&db1_from_phone, "Shared Notes");

    let db2_from_phone = user3
        .load_database(&db2_id)
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
