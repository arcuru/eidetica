//! HeightStrategy integration tests.
//!
//! These tests verify that height calculation strategies work correctly
//! when entries are created through the Transaction layer.

use std::sync::Arc;

use eidetica::{
    Clock, FixedClock, HeightStrategy, Instance, Store, backend::database::InMemory,
    instance::LegacyInstanceOps, store::DocStore,
};

/// Helper to create a test instance and database
///
/// Uses FixedClock for more deterministic testing. For tests needing explicit
/// clock control, use `create_test_database_with_clock()` instead.
async fn create_test_database() -> (Instance, eidetica::Database) {
    let clock = Arc::new(FixedClock::default());
    let backend = Box::new(InMemory::new());
    let instance = Instance::open_with_clock(backend, clock)
        .await
        .expect("Failed to create test instance");

    let database = instance.new_database_default("_device_key").await.unwrap();

    (instance, database)
}

/// Helper to create a test instance and database with a custom clock
async fn create_test_database_with_clock(clock: Arc<dyn Clock>) -> (Instance, eidetica::Database) {
    let backend = Box::new(InMemory::new());
    let instance = Instance::open_with_clock(backend, clock)
        .await
        .expect("Failed to create test instance");

    let database = instance.new_database_default("_device_key").await.unwrap();

    (instance, database)
}

#[tokio::test]
async fn test_height_strategy_default_is_incremental() {
    let (_instance, database) = create_test_database().await;

    // Get the default height strategy
    let tx = database.new_transaction().await.unwrap();
    let settings = tx.get_settings().unwrap();
    let strategy = settings.get_height_strategy().await.unwrap();

    assert_eq!(
        strategy,
        HeightStrategy::Incremental,
        "Default strategy should be Incremental"
    );
}

#[tokio::test]
async fn test_height_strategy_incremental_produces_sequential_heights() {
    let (_instance, database) = create_test_database().await;

    // Create several entries and verify sequential heights
    for i in 1..=5 {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("test_data").await.unwrap();
        store.set("value", format!("entry_{i}")).await.unwrap();
        let entry_id = tx.commit().await.unwrap();

        // Fetch the entry to check its height
        let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

        // Height should be i (1, 2, 3, 4, 5)
        assert_eq!(entry.height(), i, "Entry {i} should have height {i}");
    }
}

#[tokio::test]
async fn test_height_strategy_timestamp_produces_timestamp_heights() {
    // Use a fixed clock with a known timestamp
    let clock = Arc::new(eidetica::FixedClock::new(1_700_000_000_000)); // ~Nov 2023
    let (_instance, database) = create_test_database_with_clock(clock.clone()).await;

    // Set timestamp strategy
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Set clock to known value (undoes any auto-advance from setup)
    clock.set(1_700_000_000_100);

    // Hold and create entry - clock stays at 1_700_000_000_100
    let entry_id = {
        let _hold = clock.hold();
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("test_data").await.unwrap();
        store.set("value", "test").await.unwrap();
        tx.commit().await.unwrap()
    };

    // Fetch the entry to check its height
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

    // Height should be the timestamp from our fixed clock
    assert_eq!(
        entry.height(),
        1_700_000_000_100,
        "Entry height should match the fixed clock timestamp"
    );
}

#[tokio::test]
async fn test_height_strategy_timestamp_ensures_monotonic() {
    let (_instance, database) = create_test_database().await;

    // Set timestamp strategy
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Create entries quickly and verify heights are always increasing
    let mut last_height = 0u64;
    for i in 1..=10 {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("test_data").await.unwrap();
        store.set("value", format!("entry_{i}")).await.unwrap();
        let entry_id = tx.commit().await.unwrap();

        // Fetch the entry to check its height
        let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

        assert!(
            entry.height() > last_height,
            "Entry {i} height {} should be > previous height {}",
            entry.height(),
            last_height
        );
        last_height = entry.height();
    }
}

#[tokio::test]
async fn test_height_strategy_subtrees_inherit_database_strategy() {
    // Use a fixed clock with a known timestamp
    let clock = Arc::new(eidetica::FixedClock::new(1_700_000_000_000));
    let (_instance, database) = create_test_database_with_clock(clock.clone()).await;

    // Set timestamp strategy
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Set clock to known value (undoes any auto-advance from setup)
    clock.set(1_700_000_000_100);

    // Hold and create entry - clock stays at 1_700_000_000_100
    let entry_id = {
        let _hold = clock.hold();
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("my_subtree").await.unwrap();
        store.set("value", "test").await.unwrap();
        tx.commit().await.unwrap()
    };

    // Fetch the entry to check its height
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

    // Subtree height should also be a timestamp from our fixed clock
    let subtree_height = entry.subtree_height("my_subtree").unwrap();
    assert_eq!(
        subtree_height, 1_700_000_000_100,
        "Subtree height should match the fixed clock timestamp"
    );
}

#[tokio::test]
async fn test_height_strategy_persisted_in_settings() {
    let (_instance, database) = create_test_database().await;

    // Set timestamp strategy
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Read it back in a new transaction
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        let strategy = settings.get_height_strategy().await.unwrap();
        assert_eq!(strategy, HeightStrategy::Timestamp);
    }
}

#[tokio::test]
async fn test_height_strategy_works_in_same_transaction() {
    // Use a fixed clock with a known timestamp
    let clock = Arc::new(eidetica::FixedClock::new(1_700_000_000_000));
    let (_instance, database) = create_test_database_with_clock(clock.clone()).await;

    // Set clock to known value (undoes any auto-advance from setup)
    clock.set(1_700_000_000_100);

    // Hold and create entry with same-transaction strategy change
    let entry_id = {
        let _hold = clock.hold();

        let tx = database.new_transaction().await.unwrap();

        // Set strategy
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();

        // Create entry in same transaction
        let store = tx.get_store::<DocStore>("test_data").await.unwrap();
        store.set("value", "test").await.unwrap();

        tx.commit().await.unwrap()
    };

    // Fetch the entry to check its height
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

    // Height should be the timestamp from our fixed clock
    assert_eq!(
        entry.height(),
        1_700_000_000_100,
        "Entry height should match fixed clock timestamp (same-transaction strategy change)"
    );
}

#[tokio::test]
async fn test_per_subtree_strategy_via_index() {
    let (_instance, database) = create_test_database().await;

    // First, create a store to register it in _index
    {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("my_store").await.unwrap();
        store.set("key", "value1").await.unwrap();
        tx.commit().await.unwrap();
    }

    // Now set an independent height strategy for the store
    {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("my_store").await.unwrap();
        store
            .set_height_strategy(Some(HeightStrategy::Incremental))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Verify the strategy was persisted
    {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("my_store").await.unwrap();
        let strategy = store.get_height_strategy().await.unwrap();
        assert_eq!(
            strategy,
            Some(HeightStrategy::Incremental),
            "Height strategy should be persisted"
        );
    }

    // Create an entry and verify the subtree has an independent height
    {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("my_store").await.unwrap();
        store.set("key", "value2").await.unwrap();
        let entry_id = tx.commit().await.unwrap();

        // Fetch the entry
        let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

        // Subtree should have an independent height (not 0)
        // Since it's Incremental strategy and has parents, height should be > 0
        let subtree_height = entry.subtree_height("my_store").unwrap();
        assert!(
            subtree_height > 0,
            "Subtree with independent strategy should have non-zero height"
        );
    }
}

#[tokio::test]
async fn test_unregistered_subtree_inherits_tree_height() {
    // Uses FixedClock which defaults to 1704067200000 (2024-01-01 00:00:00 UTC)
    let (_instance, database) = create_test_database().await;

    // Set database to use timestamp strategy
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Create an entry with a store (which auto-registers in _index)
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("new_store").await.unwrap();
    store.set("key", "value").await.unwrap();
    let entry_id = tx.commit().await.unwrap();

    // Fetch the entry
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

    // Tree height should be a timestamp-like value (FixedClock produces values around 1704067200000)
    assert!(
        entry.height() >= 1_000_000_000_000,
        "Tree height {} should be a timestamp from FixedClock",
        entry.height()
    );

    // Subtree should inherit tree height (returned via subtree_height())
    let subtree_height = entry.subtree_height("new_store").unwrap();
    assert_eq!(
        subtree_height,
        entry.height(),
        "Subtree without explicit strategy should inherit tree height"
    );
}

#[tokio::test]
async fn test_mixed_subtree_strategies() {
    // Uses FixedClock which defaults to 1704067200000 (2024-01-01 00:00:00 UTC)
    let (_instance, database) = create_test_database().await;

    // Set database to use timestamp strategy
    {
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_settings().unwrap();
        settings
            .set_height_strategy(HeightStrategy::Timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Create two stores - one with independent strategy, one inheriting
    {
        let tx = database.new_transaction().await.unwrap();

        // Create stores
        let inherit_store = tx.get_store::<DocStore>("inherit_store").await.unwrap();
        inherit_store.set("key", "value").await.unwrap();

        let independent_store = tx.get_store::<DocStore>("independent_store").await.unwrap();
        independent_store.set("key", "value").await.unwrap();
        // Set independent strategy for this one
        independent_store
            .set_height_strategy(Some(HeightStrategy::Incremental))
            .await
            .unwrap();

        tx.commit().await.unwrap();
    }

    // Verify the strategy was persisted before the next transaction
    {
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("independent_store").await.unwrap();
        let strategy = store.get_height_strategy().await.unwrap();
        assert_eq!(
            strategy,
            Some(HeightStrategy::Incremental),
            "Height strategy should be persisted after first commit"
        );
    }

    // Create an entry that writes to both stores
    let tx = database.new_transaction().await.unwrap();
    let inherit_store = tx.get_store::<DocStore>("inherit_store").await.unwrap();
    inherit_store.set("key", "new_value").await.unwrap();

    let independent_store = tx.get_store::<DocStore>("independent_store").await.unwrap();
    independent_store.set("key", "new_value").await.unwrap();

    let entry_id = tx.commit().await.unwrap();

    // Fetch the entry
    let entry = database.backend().unwrap().get(&entry_id).await.unwrap();

    // Tree height should be a timestamp-like value (FixedClock produces values around 1704067200000)
    let tree_height = entry.height();
    assert!(
        tree_height >= 1_000_000_000_000,
        "Tree height {} should be a timestamp from FixedClock",
        tree_height
    );

    // inherit_store should have same height as tree (inherited)
    let inherit_height = entry.subtree_height("inherit_store").unwrap();
    assert_eq!(
        inherit_height, tree_height,
        "Inheriting subtree should match tree height"
    );

    // independent_store should have a different (incremental) height
    let independent_height = entry.subtree_height("independent_store").unwrap();
    // It should be a small integer (2 or 3), not a timestamp
    assert!(
        independent_height < 100,
        "Independent subtree should have incremental height ({}), not timestamp",
        independent_height
    );
}
