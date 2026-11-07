//! Signed adversarial Store edges exercise acceptance, not raw storage.
use eidetica::{
    Database, Entry, Instance, NewAccount, Snapshot,
    auth::{
        crypto::{PrivateKey, sign_entry, verify_entry_signature},
        types::SigKey,
    },
    backend::VerificationStatus,
    crdt::Doc,
    entry::ID,
    store::DocStore,
};

use crate::helpers::{LocalBackendTestExt, test_backend};

struct Fixture {
    instance: Instance,
    db: Database,
    key: PrivateKey,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_backend(test_backend().await).await
    }

    async fn with_backend(backend: Box<dyn eidetica::backend::BackendImpl>) -> Self {
        let (instance, _) = Instance::create_backend(backend, NewAccount::passwordless("admin"))
            .await
            .unwrap();
        let key = PrivateKey::generate();
        let db = Database::create(&instance, key.clone(), Doc::new())
            .await
            .unwrap();
        Self { instance, db, key }
    }

    fn entry(
        &self,
        main: &[ID],
        store: &str,
        parents: &[ID],
        height: u64,
        store_height: Option<u64>,
    ) -> Entry {
        let mut data = Doc::new();
        data.set(format!("entry_{height}"), "present");
        let entry = Entry::builder(self.db.root_id().clone())
            .set_parents(main.to_vec())
            .set_height(height)
            .set_metadata(
                serde_json::to_vec(&serde_json::json!({
                    "settings_tips": [self.db.root_id()], "entropy": null
                }))
                .unwrap(),
            )
            .set_subtree_data(store, serde_json::to_vec(&data).unwrap())
            .set_subtree_parents(store, parents.to_vec())
            .set_subtree_height(store, store_height)
            .build()
            .unwrap()
            .with_auth(|auth| auth.key = SigKey::from_pubkey(&self.key.public_key()));
        let signature = sign_entry(&entry, &self.key).unwrap();
        let entry = entry.with_auth(|auth| auth.signature = Some(signature));
        verify_entry_signature(&entry, &self.key.public_key()).unwrap();
        entry
    }

    async fn status(&self, entry: &Entry) -> VerificationStatus {
        self.instance
            .backend()
            .get_verification_status(&entry.id())
            .await
            .unwrap()
    }

    async fn ingest(&self, entry: &Entry, status: VerificationStatus) {
        self.instance.backend().put(entry.clone()).await.unwrap();
        self.db.verify().await.unwrap();
        assert_eq!(
            self.status(entry).await,
            status,
            "main {:?}; Store {:?}; height {}",
            entry.parents(),
            entry.subtree_parents("data"),
            entry.height()
        );
        assert_eq!(
            self.instance.backend().get(&entry.id()).await.unwrap(),
            *entry,
            "raw bytes survive any verdict"
        );
    }

    async fn first(&self) -> Entry {
        let first = self.entry(&[self.db.root_id().clone()], "data", &[], 1, None);
        self.ingest(&first, VerificationStatus::Verified).await;
        first
    }
}

#[tokio::test]
async fn test_store_verification_rejects_sibling_and_wrong_targets() {
    let f = Fixture::new().await;
    let first = f.first().await;
    let sibling = f.entry(&[f.db.root_id().clone()], "data", &[], 2, None);
    f.ingest(&sibling, VerificationStatus::Verified).await;
    let other = f.entry(&[first.id()], "other", &[], 3, None);
    f.ingest(&other, VerificationStatus::Verified).await;
    let foreign = Fixture::new().await;
    let foreign_entry = foreign.first().await;
    f.instance
        .backend()
        .put(foreign_entry.clone())
        .await
        .unwrap();

    for target in [
        sibling.id(),
        other.id(),
        foreign_entry.id(),
        ID::from_bytes("absent-store-parent"),
    ] {
        let invalid = f.entry(&[other.id()], "data", &[target], 10, None);
        f.ingest(&invalid, VerificationStatus::Failed).await;
    }
}

#[tokio::test]
async fn test_store_verification_allows_omitted_nonmaximal_and_redundant_parents() {
    let f = Fixture::new().await;
    let first = f.first().await;
    let a = f.entry(&[first.id()], "data", &[first.id()], 2, None);
    let b = f.entry(&[first.id()], "data", &[first.id()], 3, None);
    for entry in [&a, &b] {
        f.ingest(entry, VerificationStatus::Verified).await;
    }
    for parents in [
        vec![],
        vec![a.id()],
        vec![first.id()],
        vec![first.id(), a.id(), b.id()],
    ] {
        let valid = f.entry(&[a.id(), b.id()], "data", &parents, 10, None);
        f.ingest(&valid, VerificationStatus::Verified).await;
    }
    for parents in [vec![a.id(), b.id()], vec![b.id(), a.id()]] {
        let valid = f.entry(&[b.id(), a.id()], "data", &parents, 10, None);
        f.ingest(&valid, VerificationStatus::Verified).await;
    }
    // Redundant main parents do not change the ordinary Store-parent contract.
    let valid = f.entry(&[first.id(), a.id()], "data", &[a.id()], 11, None);
    f.ingest(&valid, VerificationStatus::Verified).await;
}

#[tokio::test]
async fn test_store_verification_preserves_untouched_stores_and_first_creation() {
    let f = Fixture::new().await;
    let root = f.db.get_root().await.unwrap();
    assert_eq!(f.status(&root).await, VerificationStatus::Verified);
    assert!(root.subtree_parents("_settings").unwrap().is_empty());
    let first = f.first().await;
    let middle = f.entry(&[first.id()], "other", &[], 2, None);
    f.ingest(&middle, VerificationStatus::Verified).await;
    let valid = f.entry(&[middle.id()], "data", &[first.id()], 3, Some(50));
    f.ingest(&valid, VerificationStatus::Verified).await;
    let omitted = f.entry(&[middle.id()], "data", &[], 3, None);
    f.ingest(&omitted, VerificationStatus::Verified).await;
}

#[tokio::test]
async fn test_store_verification_checks_effective_heights_not_main_heights() {
    for parent_height in [None, Some(100)] {
        let f = Fixture::new().await;
        let first = f.entry(&[f.db.root_id().clone()], "data", &[], 1, parent_height);
        f.ingest(&first, VerificationStatus::Verified).await;
        let effective = first.subtree_height("data").unwrap();
        for height in [Some(effective), Some(effective - 1)] {
            let invalid = f.entry(&[first.id()], "data", &[first.id()], 10, height);
            f.ingest(&invalid, VerificationStatus::Failed).await;
        }
        let gap = f.entry(
            &[first.id()],
            "data",
            &[first.id()],
            10,
            Some(effective + 50),
        );
        f.ingest(&gap, VerificationStatus::Verified).await;
        let inherited = f.entry(&[first.id()], "data", &[first.id()], effective + 1, None);
        f.ingest(&inherited, VerificationStatus::Verified).await;
        if parent_height.is_some() {
            let invalid = f.entry(&[first.id()], "data", &[first.id()], 2, None);
            f.ingest(&invalid, VerificationStatus::Failed).await;
        }
    }
}

#[tokio::test]
async fn test_store_verification_retry_propagates_failure_and_retains_raw_entries() {
    for valid in [false, true] {
        let f = Fixture::new().await;
        let first = f.first().await;
        let late = f.entry(&[first.id()], "data", &[first.id()], 2, Some(100));
        let child = f.entry(
            &[late.id()],
            "data",
            &[late.id()],
            3,
            Some(100 + u64::from(valid)),
        );
        let descendant = f.entry(&[child.id()], "data", &[child.id()], 4, Some(200));
        for entry in [&descendant, &child] {
            f.ingest(entry, VerificationStatus::Unverified).await;
        }
        // Bytes alone are not proof: even a held but Unverified parent blocks.
        f.instance.backend().put(late.clone()).await.unwrap();
        assert_eq!(f.status(&late).await, VerificationStatus::Unverified);
        assert_eq!(f.status(&child).await, VerificationStatus::Unverified);
        f.db.verify().await.unwrap();
        assert_eq!(f.status(&late).await, VerificationStatus::Verified);
        let expected = if valid {
            VerificationStatus::Verified
        } else {
            VerificationStatus::Failed
        };
        for entry in [&child, &descendant] {
            f.ingest(entry, expected).await;
        }
        let tips = f.db.snapshot().await.unwrap();
        assert_eq!(tips.contains(&descendant.id()), valid);
        assert_eq!(tips.contains(&late.id()), !valid);
    }
}

#[tokio::test]
async fn test_store_verification_invalid_evidence_beats_incomplete_ancestry() {
    let f = Fixture::new().await;
    let first = f.first().await;
    let missing = ID::from_bytes("missing-main-parent");
    for main in [vec![first.id(), missing.clone()], vec![missing, first.id()]] {
        let invalid = f.entry(&main, "data", &[first.id()], 10, Some(1));
        f.ingest(&invalid, VerificationStatus::Failed).await;
        // An independent ordinary Store root is valid, but must wait for its
        // entire main ancestry like any other Entry.
        let omitted = f.entry(&main, "data", &[], 10, Some(100));
        f.ingest(&omitted, VerificationStatus::Unverified).await;
        let invalid_settings = f.entry(&main, "_settings", &[], 10, None);
        f.ingest(&invalid_settings, VerificationStatus::Failed)
            .await;
    }
}

#[tokio::test]
async fn test_store_verification_incomplete_cut_is_not_an_empty_frontier() {
    let f = Fixture::new().await;
    let late = f.entry(&[f.db.root_id().clone()], "data", &[], 1, None);
    let omitted = f.entry(&[late.id()], "data", &[], 2, None);
    let valid = f.entry(&[late.id()], "data", &[late.id()], 2, None);
    for entry in [&omitted, &valid] {
        f.ingest(entry, VerificationStatus::Unverified).await;
    }
    f.ingest(&late, VerificationStatus::Verified).await;
    assert_eq!(f.status(&omitted).await, VerificationStatus::Verified);
    assert_eq!(f.status(&valid).await, VerificationStatus::Verified);
    assert_eq!(
        f.db.snapshot().await.unwrap(),
        Snapshot::from([omitted.id(), valid.id()])
    );
}

/// Main ancestry observes B but does not consume B's Store contribution.
/// Exercise acceptance, snapshot discovery, cold/warm reads and authored joins.
#[tokio::test]
async fn test_store_verification_main_descendant_keeps_unconsumed_tip() {
    let f = Fixture::new().await;
    let a = f.first().await;
    let b = f.entry(&[a.id()], "data", &[a.id()], 2, None);
    let c = f.entry(&[b.id()], "data", &[a.id()], 3, None);
    f.ingest(&b, VerificationStatus::Verified).await;
    f.ingest(&c, VerificationStatus::Verified).await;
    assert_eq!(f.db.snapshot().await.unwrap(), Snapshot::from([c.id()]));
    let frontier = Snapshot::from([b.id(), c.id()]);
    let backend = f.instance.backend();
    assert_eq!(
        backend
            .store_snapshot(f.db.root_id(), "data")
            .await
            .unwrap(),
        frontier
    );
    for boundary in [
        Snapshot::from([c.id()]),
        Snapshot::from([a.id(), b.id(), c.id()]),
    ] {
        assert_eq!(
            backend
                .store_snapshot_at(f.db.root_id(), "data", &boundary)
                .await
                .unwrap(),
            frontier
        );
    }
    let middle = f.entry(&[c.id()], "other", &[], 4, None);
    f.ingest(&middle, VerificationStatus::Verified).await;
    assert_eq!(
        backend
            .store_snapshot_at(f.db.root_id(), "data", &Snapshot::from([middle.id()]))
            .await
            .unwrap(),
        frontier
    );
    for _ in 0..2 {
        let view = f.db.get_store_viewer::<DocStore>("data").await.unwrap();
        for height in 1..=3 {
            assert_eq!(
                view.get(&format!("entry_{height}")).await.unwrap(),
                eidetica::crdt::doc::Value::Text("present".into())
            );
        }
    }
    // Ordinary transaction discovery must select BOTH tips, not just main C.
    let tx = f.db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("joined", "yes")
        .await
        .unwrap();
    let join = tx.commit().await.unwrap();
    let joined = backend.get(&join).await.unwrap();
    assert_eq!(
        Snapshot::from(joined.subtree_parents("data").unwrap()),
        frontier
    );
    assert_eq!(
        backend.get_verification_status(&join).await.unwrap(),
        VerificationStatus::Verified
    );
    assert_eq!(
        backend
            .store_snapshot_at(f.db.root_id(), "data", &f.db.snapshot().await.unwrap())
            .await
            .unwrap(),
        Snapshot::from([join])
    );
    let view = f.db.get_store_viewer::<DocStore>("data").await.unwrap();
    assert_eq!(
        view.get("entry_2").await.unwrap(),
        eidetica::crdt::doc::Value::Text("present".into())
    );
    assert_eq!(
        view.get("entry_3").await.unwrap(),
        eidetica::crdt::doc::Value::Text("present".into())
    );
}

/// Child-first sync must not mistake an unresolved main cut for an absent Store
/// parent; after the dependency arrives both independent Store tips are visible.
#[tokio::test]
async fn test_store_verification_independent_branch_dependency_retry() {
    let f = Fixture::new().await;
    let a = f.first().await;
    let b = f.entry(&[a.id()], "data", &[a.id()], 2, None);
    let c = f.entry(&[b.id()], "data", &[a.id()], 3, None);
    f.ingest(&c, VerificationStatus::Unverified).await;
    f.ingest(&b, VerificationStatus::Verified).await;
    assert_eq!(f.status(&c).await, VerificationStatus::Verified);
    assert_eq!(
        f.instance
            .backend()
            .store_snapshot_at(f.db.root_id(), "data", &f.db.snapshot().await.unwrap())
            .await
            .unwrap(),
        Snapshot::from([b.id(), c.id()])
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_store_verification_branch_survives_reopen() {
    #[cfg(feature = "sqlite")]
    use eidetica::backend::database::Sqlite;
    #[cfg(feature = "postgres")]
    use eidetica::backend::database::SqlxBackend;
    use eidetica::backend::{BackendImpl, database::InMemory};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("branches");
    let modes = [
        Some("memory"),
        cfg!(feature = "sqlite").then_some("sqlite"),
        (cfg!(feature = "postgres") && std::env::var("TEST_BACKEND").as_deref() == Ok("postgres"))
            .then_some("postgres"),
    ];
    #[cfg(feature = "postgres")]
    let (pg_url, schema) = (
        std::env::var("TEST_POSTGRES_URL")
            .unwrap_or_else(|_| "postgres://localhost/eidetica_test".into()),
        format!("store_branch_{}", uuid::Uuid::new_v4().simple()),
    );
    for mode in modes.into_iter().flatten() {
        let backend: Box<dyn BackendImpl> = match mode {
            "memory" => Box::new(InMemory::new()),
            #[cfg(feature = "sqlite")]
            "sqlite" => Box::new(Sqlite::open(&path).await.unwrap()),
            #[cfg(feature = "postgres")]
            "postgres" => Box::new(
                SqlxBackend::test_connect_postgres_schema(&pg_url, schema.clone())
                    .await
                    .unwrap(),
            ),
            _ => unreachable!(),
        };
        let f = Fixture::with_backend(backend).await;
        let a = f.first().await;
        let b = f.entry(&[a.id()], "data", &[a.id()], 2, None);
        let c = f.entry(&[b.id()], "data", &[a.id()], 3, None);
        f.ingest(&b, VerificationStatus::Verified).await;
        f.ingest(&c, VerificationStatus::Verified).await;
        // Populate a real derived read before restart as well as retained tips.
        f.db.get_store_viewer::<DocStore>("data")
            .await
            .unwrap()
            .get("entry_2")
            .await
            .unwrap();
        let root = f.db.root_id().clone();
        let key = f.key.clone();
        if mode == "memory" {
            f.instance
                .backend()
                .local_engine()
                .unwrap()
                .as_any()
                .downcast_ref::<InMemory>()
                .unwrap()
                .save_to_file(&path)
                .unwrap();
        }
        drop(f);
        let reopened: Box<dyn BackendImpl> = match mode {
            "memory" => {
                let loaded = InMemory::load_from_file(&path).await.unwrap();
                std::fs::remove_file(&path).unwrap();
                Box::new(loaded)
            }
            #[cfg(feature = "sqlite")]
            "sqlite" => Box::new(Sqlite::open(&path).await.unwrap()),
            #[cfg(feature = "postgres")]
            "postgres" => {
                SqlxBackend::test_await_postgres_namespace_release(
                    &pg_url,
                    &schema,
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
                Box::new(
                    SqlxBackend::test_connect_postgres_schema(&pg_url, schema.clone())
                        .await
                        .unwrap(),
                )
            }
            _ => unreachable!(),
        };
        let instance = Instance::open_backend(reopened).await.unwrap();
        let db = Database::open(&instance, &root)
            .await
            .unwrap()
            .with_key(key);
        for entry in [&a, &b, &c] {
            assert_eq!(
                instance
                    .backend()
                    .get_verification_status(&entry.id())
                    .await
                    .unwrap(),
                VerificationStatus::Verified
            );
        }
        assert_eq!(db.snapshot().await.unwrap(), Snapshot::from([c.id()]));
        assert_eq!(
            instance
                .backend()
                .store_snapshot_at(&root, "data", &Snapshot::from([c.id()]))
                .await
                .unwrap(),
            Snapshot::from([b.id(), c.id()])
        );
        let view = db.get_store_viewer::<DocStore>("data").await.unwrap();
        assert_eq!(
            view.get("entry_2").await.unwrap(),
            eidetica::crdt::doc::Value::Text("present".into())
        );
        assert_eq!(
            view.get("entry_3").await.unwrap(),
            eidetica::crdt::doc::Value::Text("present".into())
        );
        instance
            .backend()
            .clear_derived_store_state()
            .await
            .unwrap();
        let cold = db.get_store_viewer::<DocStore>("data").await.unwrap();
        assert_eq!(
            cold.get("entry_2").await.unwrap(),
            eidetica::crdt::doc::Value::Text("present".into())
        );
    }
}
