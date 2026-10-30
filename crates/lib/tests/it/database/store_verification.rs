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
};

use crate::helpers::{LocalBackendTestExt, test_backend};

struct Fixture {
    instance: Instance,
    db: Database,
    key: PrivateKey,
}

impl Fixture {
    async fn new() -> Self {
        let (instance, _) =
            Instance::create_backend(test_backend().await, NewAccount::passwordless("admin"))
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
        let entry = Entry::builder(self.db.root_id().clone())
            .set_parents(main.to_vec())
            .set_height(height)
            .set_metadata(
                serde_json::to_vec(&serde_json::json!({
                    "settings_tips": [self.db.root_id()], "entropy": null
                }))
                .unwrap(),
            )
            .set_subtree_data(store, b"{}")
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
async fn test_store_verification_requires_complete_maximal_frontier() {
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
        let invalid = f.entry(&[a.id(), b.id()], "data", &parents, 10, None);
        f.ingest(&invalid, VerificationStatus::Failed).await;
    }
    for parents in [vec![a.id(), b.id()], vec![b.id(), a.id()]] {
        let valid = f.entry(&[b.id(), a.id()], "data", &parents, 10, None);
        f.ingest(&valid, VerificationStatus::Verified).await;
    }
    // A redundant main parent must not make its older Store contribution maximal.
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
    f.ingest(&omitted, VerificationStatus::Failed).await;
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
    assert_eq!(f.status(&omitted).await, VerificationStatus::Failed);
    assert_eq!(f.status(&valid).await, VerificationStatus::Verified);
    assert_eq!(f.db.snapshot().await.unwrap(), Snapshot::from([valid.id()]));
}
