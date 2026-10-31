//! Embedded leases deliberately share only an Instance-local registry.

use std::time::Duration;

use eidetica::{
    Database, Instance,
    crdt::Doc,
    lease::{LeaseAcquire, LeaseGuard, MAX_LEASE_NAME_BYTES},
};
use tokio::time::timeout;

async fn setup() -> (Instance, Database) {
    let instance = crate::helpers::test_local_instance().await;
    let mut user = instance.login_account("admin", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let db = user.create_database(Doc::new(), &key).await.unwrap();
    (instance, db)
}

pub(super) fn granted(result: LeaseAcquire) -> LeaseGuard {
    match result {
        LeaseAcquire::Granted(guard) => guard,
        other => panic!("expected grant, got {other:?}"),
    }
}

#[tokio::test]
async fn test_lease_embedded_clones_and_independent_keys() {
    let (instance, db) = setup().await;
    let before = db.snapshot().await.unwrap();
    let lease = db.lease("runtime");
    let mut watch = lease.watch().await.unwrap();
    assert_eq!(watch.status().holder, None);
    let guard = granted(lease.try_acquire().await.unwrap());
    assert!(!guard.is_lost());
    let clone_db = Database::open(&instance.clone(), db.root_id())
        .await
        .unwrap();
    assert!(matches!(
        clone_db.lease("runtime").try_acquire().await.unwrap(),
        LeaseAcquire::AlreadyHeld(_)
    ));
    let status = timeout(Duration::from_secs(2), watch.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, lease.status().await.unwrap());
    assert_eq!(status.generation, 1);
    let _different_name = granted(db.lease("standby").try_acquire().await.unwrap());
    let mut user = instance.login_account("admin", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let other = user.create_database(Doc::new(), &key).await.unwrap();
    let _different_db = granted(other.lease("runtime").try_acquire().await.unwrap());
    // No lease transition creates a signed Entry or changes the DAG.
    assert_eq!(before, db.snapshot().await.unwrap());
    guard.release().await.unwrap();
    assert_eq!(watch.changed().await.unwrap().holder, None);
    let next = granted(lease.try_acquire().await.unwrap());
    drop(next);
    assert_eq!(lease.status().await.unwrap().holder, None);
}

#[tokio::test]
async fn test_lease_distinct_instances_and_restart_clear_state() {
    let (instance, db) = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("db.json");
    instance.snapshot_to_path(&snapshot).unwrap();
    let guard = granted(db.lease("runtime").try_acquire().await.unwrap());
    // Loading the same persisted database does not recover or coordinate leases.
    let reopened = Instance::connect(format!("memory://{}", snapshot.display()))
        .await
        .unwrap();
    let other_db = Database::open(&reopened, db.root_id()).await.unwrap();
    assert_eq!(
        other_db.lease("runtime").status().await.unwrap().generation,
        0
    );
    let other = granted(other_db.lease("runtime").try_acquire().await.unwrap());
    assert!(!guard.is_lost());
    assert!(!other.is_lost());
    drop(other);
    drop(other_db);
    drop(reopened);
    let restarted = Instance::connect(format!("memory://{}", snapshot.display()))
        .await
        .unwrap();
    let restarted_db = Database::open(&restarted, db.root_id()).await.unwrap();
    assert_eq!(
        restarted_db.lease("runtime").status().await.unwrap().holder,
        None
    );
    assert_eq!(
        restarted_db
            .lease("runtime")
            .status()
            .await
            .unwrap()
            .generation,
        0
    );
}

#[tokio::test]
async fn test_lease_embedded_teardown_signals_loss() {
    let (instance, db) = setup().await;
    let lease = db.lease("runtime");
    let mut guard = granted(lease.try_acquire().await.unwrap());
    let mut watch = lease.watch().await.unwrap();
    drop(db);
    drop(instance);
    timeout(Duration::from_secs(2), guard.lost()).await.unwrap();
    assert!(guard.is_lost());
    assert!(watch.changed().await.is_err());
    assert!(lease.status().await.is_err());
}

#[tokio::test]
async fn test_lease_embedded_race_has_one_guard_and_no_ttl() {
    let (_instance, db) = setup().await;
    let lease = db.lease("runtime");
    let results = futures_util::future::join_all((0..24).map(|_| lease.try_acquire())).await;
    let mut guards = Vec::new();
    for result in results {
        match result.unwrap() {
            LeaseAcquire::Granted(guard) => guards.push(guard),
            LeaseAcquire::AlreadyHeld(status) => assert_eq!(status.generation, 1),
            LeaseAcquire::Held(_) => panic!("one embedded Instance has one implicit owner"),
        }
    }
    assert_eq!(guards.len(), 1);
    let status = lease.status().await.unwrap();
    for _ in 0..24 {
        tokio::task::yield_now().await;
        assert_eq!(lease.status().await.unwrap(), status);
    }
    assert!(lease.wait_acquire().await.is_err());
    drop(guards);
    let _next = timeout(Duration::from_secs(2), lease.wait_acquire())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn test_lease_names_are_opaque_and_byte_bounded() {
    let (_instance, db) = setup().await;
    let _empty = granted(db.lease("").try_acquire().await.unwrap());
    let max = "é".repeat(MAX_LEASE_NAME_BYTES / 2);
    let _max = granted(db.lease(&max).try_acquire().await.unwrap());
    let over = db.lease(format!("{max}é"));
    assert!(over.status().await.is_err());
    assert!(over.try_acquire().await.is_err());
    assert!(over.watch().await.is_err());
}
