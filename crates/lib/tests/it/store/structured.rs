//! Type attribution through real public Store/transaction/backend boundaries.
use std::collections::{BTreeMap, HashMap};

// Own the daemon-side cache administrator as well as the client. Remote clear
// is intentionally a no-op; testing a real clear must act on the owned server.
struct Fixture {
    _client: eidetica::Instance,
    source: eidetica::Instance,
    key: eidetica::auth::crypto::PrivateKey,
    #[cfg(all(unix, feature = "service"))]
    _service: Option<(tokio::sync::watch::Sender<()>, tempfile::TempDir)>,
}

async fn setup_tree() -> (Fixture, Database) {
    let (source, _) = eidetica::Instance::create_backend(
        crate::helpers::test_backend().await,
        eidetica::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let client = source.clone();
    #[cfg(all(unix, feature = "service"))]
    let (client, service) = if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.path().join("structured.sock");
        let server = eidetica::service::ServiceServer::bind(source.clone(), socket.clone())
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(());
        tokio::spawn(server.run(rx));
        let client = eidetica::Instance::connect(format!("unix://{}", socket.display()))
            .await
            .unwrap();
        (client, Some((tx, dir)))
    } else {
        (client, None)
    };
    let mut user = client.login_account("admin", "").await.unwrap();
    let public = user.get_default_key().unwrap();
    let key = user.get_signing_key(&public).unwrap();
    let db = user.create_database(Doc::new(), &public).await.unwrap();
    (
        Fixture {
            _client: client,
            source,
            key,
            #[cfg(all(unix, feature = "service"))]
            _service: service,
        },
        db,
    )
}

/// A valid signed Entry the typed writer would not author.
fn signed_entry(
    fixture: &Fixture,
    db: &Database,
    parents: &[&Entry],
    store_parents: &[&Entry],
    bytes: Option<Vec<u8>>,
    index: Option<&Doc>,
) -> Entry {
    let ids = |entries: &[&Entry]| entries.iter().map(|e| e.id()).collect::<Vec<_>>();
    let mut builder = Entry::builder(db.root_id().clone())
        .set_parents(ids(parents))
        .set_height(parents.iter().map(|e| e.height()).max().unwrap() + 1)
        .set_subtree_parents("subject", ids(store_parents))
        .set_metadata(parents[0].metadata().unwrap().clone())
        .set_auth(eidetica::auth::types::AuthInfo::from_pubkey(
            &fixture.key.public_key(),
        ));
    if let Some(bytes) = bytes {
        builder = builder.set_subtree_data("subject", bytes);
    }
    if let Some(index) = index {
        let tips = parents.iter().filter(|e| e.in_subtree("_index"));
        builder = builder
            .set_subtree_parents("_index", tips.map(|e| e.id()).collect())
            .set_subtree_data("_index", index.encode().unwrap());
    }
    builder.build().unwrap().sign(&fixture.key).unwrap()
}

/// Received through raw storage, then accepted by ordinary verification.
async fn receive(fixture: &Fixture, db: &Database, entry: Entry) -> ID {
    let id = entry.id();
    db.insert_raw(entry).await.unwrap();
    let local = Database::open(&fixture.source, db.root_id()).await.unwrap();
    local.verify().await.unwrap();
    assert_eq!(
        fixture
            .source
            .backend()
            .get_verification_status(&id)
            .await
            .unwrap(),
        eidetica::backend::VerificationStatus::Verified
    );
    id
}

use crate::helpers::LocalBackendTestExt;
use eidetica::{
    Database, Error, ID, Result, Snapshot, Store, Transaction,
    backend::{CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest},
    crdt::{CRDT, Codec, Doc, Lww, Map, dag_cbor, doc::Value},
    entry::Entry,
    store::{
        DocStore, OPAQUE_STATE_KEY, PasswordStore, StoreError, StructuredStore,
        schema::{ContributionType, KeyedPairs, StructuredState, StructuredType},
    },
};
use ipld_core::ipld::Ipld;
use serde::{Deserialize, Serialize};

macro_rules! structured_type {
    ($name:ident, $id:literal, $data:ty) => {
        struct $name;
        impl StructuredType for $name {
            const TYPE_ID: &'static str = $id;
            type Data = $data;
            fn grammar(_: &[Doc]) -> Result<Doc> {
                Ok(Doc::new())
            }
        }
    };
}

structured_type!(A, "contacts:v0.1", Events);
structured_type!(B, "contacts:v0.2", Events);
structured_type!(C, "contacts:v0.3", Events);

/// One Type whose row grammar depends on a config member it owns.
struct Tuned;
impl StructuredType for Tuned {
    const TYPE_ID: &'static str = "tuned:v0.1";
    type Data = Events;
    fn grammar(configs: &[Doc]) -> Result<Doc> {
        let [config] = configs else {
            return Err(StoreError::InvalidConfiguration {
                store: "tuned".into(),
                reason: "ambiguous concurrent config".into(),
            }
            .into());
        };
        let mut grammar = Doc::new();
        if let Some(mode) = config.get("mode") {
            grammar.set("mode", mode.clone());
        }
        Ok(grammar)
    }
}

type S<T = A> = StructuredStore<T>;

// Associative concatenation deliberately has a nonidentity Default. Absent
// subtree nodes must not invent an operation or invoke this application's merge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct Events(Vec<String>);
impl Default for Events {
    fn default() -> Self {
        Self(vec!["not-a-source".into()])
    }
}
impl CRDT for Events {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.iter().chain(&other.0).cloned().collect()))
    }
}
fn events(value: &str) -> Events {
    Events(vec![value.into()])
}
fn native(value: &str) -> Vec<u8> {
    dag_cbor::encode(&events(value)).unwrap()
}

fn at(ids: &[&ID]) -> Snapshot {
    Snapshot::from(ids.iter().map(|id| (*id).clone()).collect::<Vec<_>>())
}
async fn tx_at(db: &Database, ids: &[&ID]) -> Transaction {
    db.new_transaction_at(&at(ids)).await.unwrap()
}
async fn read_at<T: StructuredType>(
    db: &Database,
    name: &str,
    ids: &[&ID],
) -> Result<StructuredState<T>> {
    // `load`, not `get_store`: attribution must not depend on the displayed row.
    S::<T>::load(&tx_at(db, ids).await, name.into())
        .await?
        .read()
        .await
}
async fn type_at(db: &Database, name: &str, ids: &[&ID], entry: &ID) -> Result<ContributionType> {
    tx_at(db, ids)
        .await
        .inspect_contribution(name, entry)
        .await
        .map(|found| found.expect("contribution"))
}
async fn declare<T: StructuredType>(db: &Database, name: &str, ids: &[&ID]) -> ID {
    let tx = tx_at(db, ids).await;
    S::<T>::declare(&tx, name, Doc::new()).await.unwrap();
    tx.commit().await.unwrap()
}
async fn write<T: StructuredType<Data = Events>>(
    db: &Database,
    name: &str,
    ids: &[&ID],
    value: &str,
) -> Result<ID> {
    let tx = tx_at(db, ids).await;
    S::<T>::load(&tx, name.into())
        .await?
        .apply(events(value))
        .await?;
    tx.commit().await
}
#[track_caller]
fn assert_unresolved(error: Error, reason: &str) {
    let Error::Store(error) = &error else {
        panic!("{error}")
    };
    assert!(
        matches!(&**error, StoreError::UnresolvedType { reason: r, .. } if r.contains(reason)),
        "{error}"
    );
}

/// E0 declares A without data, E1 changes to B and writes B data, and F is an A
/// write by a peer that has only seen E0.
async fn offline_fixture() -> (Fixture, Database, ID, ID, ID) {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject").await.unwrap();
    let e0 = tx.commit().await.unwrap();
    let tx = tx_at(&db, &[&e0]).await;
    let b = S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
    b.apply(events("b")).await.unwrap();
    let e1 = tx.commit().await.unwrap();
    let f = write::<A>(&db, "subject", &[&e0], "a").await.unwrap();
    (fixture, db, e0, e1, f)
}

#[tokio::test]
async fn test_structured_contribution_keeps_the_type_its_writer_saw() {
    let (_fixture, db, e0, e1, f) = offline_fixture().await;
    let entry = db.get_entry(&e1).await.unwrap();
    assert_eq!(entry.data("subject").unwrap(), &native("b"), "native bytes");
    assert_eq!(hex::encode(native("b")), "816162");
    assert_eq!(
        db.get_entry(&f).await.unwrap().data("subject").unwrap(),
        &native("a")
    );
    // A parent-only resolver reports A for E1. A current-row resolver reports B
    // for F. Arrival order and the cut that is asked cannot change either.
    for cut in [vec![&e1, &f], vec![&f, &e1]] {
        let b = type_at(&db, "subject", &cut, &e1).await.unwrap();
        assert_eq!(
            (b.type_id.as_str(), b.declared_by),
            (B::TYPE_ID, vec![e1.clone()])
        );
        let a = type_at(&db, "subject", &cut, &f).await.unwrap();
        assert_eq!(
            (a.type_id.as_str(), a.declared_by),
            (A::TYPE_ID, vec![e0.clone()])
        );
        for error in [
            read_at::<A>(&db, "subject", &cut).await.err().unwrap(),
            read_at::<B>(&db, "subject", &cut).await.err().unwrap(),
        ] {
            assert_unresolved(error, "unsupported Type");
        }
        // Only B survives at the join, but it cannot extend the mixed view.
        let tx = tx_at(&db, &cut).await;
        let store = S::<B>::load(&tx, "subject".into()).await.unwrap();
        assert!(store.apply(events("x")).await.is_err());
        assert!(store.local_data().unwrap().is_none());
    }
    assert_eq!(
        read_at::<A>(&db, "subject", &[&f])
            .await
            .unwrap()
            .into_value(),
        events("a")
    );
    assert_eq!(
        read_at::<B>(&db, "subject", &[&e1])
            .await
            .unwrap()
            .into_value(),
        events("b")
    );
    // Outside a cut is not "never registered".
    let error = type_at(&db, "subject", &[&f], &e1).await.err().unwrap();
    assert!(
        matches!(&error, Error::Store(e) if matches!(**e, StoreError::HistoryUnavailable { .. })),
        "{error}"
    );
    assert!(
        tx_at(&db, &[&e1])
            .await
            .inspect_contribution("subject", &e0)
            .await
            .unwrap()
            .is_none(),
        "a declaration without data is not a contribution"
    );
    let x = write::<B>(&db, "subject", &[&e1], "x").await.unwrap();
    assert_eq!(
        type_at(&db, "subject", &[&x], &x)
            .await
            .unwrap()
            .declared_by,
        [e1]
    );
}

#[tokio::test]
async fn test_structured_same_entry_declaration_is_order_independent() {
    let (_fixture, db, e0, ..) = offline_fixture().await;
    for declare_first in [true, false] {
        let tx = tx_at(&db, &[&e0]).await;
        let store = S::<B>::load(&tx, "subject".into()).await.unwrap();
        if declare_first {
            S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
        }
        store.apply(events("b")).await.unwrap();
        if !declare_first {
            S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
        }
        let id = tx.commit().await.unwrap();
        assert_eq!(
            db.get_entry(&id).await.unwrap().data("subject").unwrap(),
            &native("b")
        );
        let found = type_at(&db, "subject", &[&id], &id).await.unwrap();
        assert_eq!(
            (found.type_id.as_str(), found.declared_by),
            (B::TYPE_ID, vec![id.clone()])
        );
        assert!(db.verify_entry_signature(&id).await.unwrap());
    }
    // Same two orders, with bytes of the Type that is being replaced.
    for declare_first in [true, false] {
        let tx = tx_at(&db, &[&e0]).await;
        let store = tx.get_store::<S>("subject").await.unwrap();
        if declare_first {
            S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
        }
        store.apply(events("a")).await.unwrap();
        if !declare_first {
            S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
            // The B handle cannot adopt staged A bytes either.
            let b = S::<B>::load(&tx, "subject".into()).await.unwrap();
            assert!(b.apply(events("b")).await.is_err());
        }
        assert_unresolved(
            tx.commit().await.err().unwrap(),
            "encoded for a different Type",
        );
    }
}

#[tokio::test]
async fn test_structured_observed_type_is_emitted_or_refused_without_effects() {
    let (_fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject").await.unwrap();
    let e0 = tx.commit().await.unwrap();
    let first = write::<A>(&db, "subject", &[&e0], "a").await.unwrap();
    let changed = declare::<B>(&db, "subject", &[&first]).await;
    let entry = db.get_entry(&changed).await.unwrap();
    assert!(entry.in_subtree("subject") && entry.data("subject").is_err());
    assert_eq!(
        type_at(&db, "subject", &[&changed], &first)
            .await
            .unwrap()
            .type_id,
        A::TYPE_ID
    );
    // A metadata-only change leaves the A history readable as A, not as B.
    assert_eq!(
        read_at::<A>(&db, "subject", &[&changed])
            .await
            .unwrap()
            .into_value(),
        events("a")
    );
    assert!(read_at::<B>(&db, "subject", &[&changed]).await.is_err());

    let before = db.snapshot().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    assert!(
        tx.get_store::<S>("subject").await.is_err(),
        "ordinary open refuses"
    );
    let stale = S::<A>::load(&tx, "subject".into()).await.unwrap();
    stale.apply(events("a under B")).await.unwrap();
    tx.get_store::<DocStore>("unrelated")
        .await
        .unwrap()
        .set("key", "not committed")
        .await
        .unwrap();
    assert_unresolved(
        tx.commit().await.err().unwrap(),
        "encoded for a different Type",
    );
    assert_eq!(db.snapshot().await.unwrap(), before);
    let tx = db.new_transaction().await.unwrap();
    assert!(
        tx.get_index()
            .await
            .unwrap()
            .get_entry("unrelated")
            .await
            .is_err()
    );

    // Any other route that changes the staged row is checked at commit too.
    let tx = tx_at(&db, &[&first]).await;
    tx.get_store::<S>("subject")
        .await
        .unwrap()
        .apply(events("x"))
        .await
        .unwrap();
    tx.get_index()
        .await
        .unwrap()
        .set_entry("subject", B::TYPE_ID, Doc::new())
        .await
        .unwrap();
    assert_unresolved(
        tx.commit().await.err().unwrap(),
        "encoded for a different Type",
    );
    // Foreign bytes under a structured row have no encoder Type.
    let tx = tx_at(&db, &[&first]).await;
    tx.get_store::<S>("subject").await.unwrap();
    DocStore::load(&tx, "subject".into())
        .await
        .unwrap()
        .set("k", "v")
        .await
        .unwrap();
    assert_unresolved(tx.commit().await.err().unwrap(), "no encoder Type");
    // So do bytes that replaced a typed value after it was staged.
    let tx = tx_at(&db, &[&first]).await;
    let store = tx.get_store::<S>("subject").await.unwrap();
    store.apply(events("x")).await.unwrap();
    assert!(
        DocStore::load(&tx, "subject".into())
            .await
            .unwrap()
            .set("k", "v")
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn test_structured_conflicts_need_an_explicit_choice() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject").await.unwrap();
    let origin = tx.commit().await.unwrap();
    let left = declare::<B>(&db, "subject", &[&origin]).await;
    let mut right = declare::<C>(&db, "subject", &[&origin]).await;
    // A taller or later declaration does not win.
    right = declare::<C>(&db, "subject", &[&right]).await;
    let join = [&left, &right];
    for error in [
        write::<B>(&db, "subject", &join, "b").await.err().unwrap(),
        write::<C>(&db, "subject", &join, "c").await.err().unwrap(),
    ] {
        assert_unresolved(error, "concurrent declarations disagree");
    }
    // Saving the displayed row would silently choose one.
    let tx = tx_at(&db, &join).await;
    let error = tx
        .get_index()
        .await
        .unwrap()
        .set_subtree_settings("subject", Default::default())
        .await
        .err()
        .unwrap();
    assert_unresolved(error, "concurrent declarations disagree");

    // A valid Entry at the ambiguous cut keeps its unusable Type for good.
    let (l, r) = (
        db.get_entry(&left).await.unwrap(),
        db.get_entry(&right).await.unwrap(),
    );
    let raw = signed_entry(&fixture, &db, &[&l, &r], &[&l, &r], Some(native("?")), None);
    let raw = receive(&fixture, &db, raw).await;
    let repaired = declare::<B>(&db, "subject", &[&raw]).await;
    for cut in [&raw, &repaired] {
        assert_unresolved(
            type_at(&db, "subject", &[cut], &raw).await.err().unwrap(),
            "concurrent declarations disagree",
        );
        assert_unresolved(
            read_at::<B>(&db, "subject", &[cut]).await.err().unwrap(),
            "concurrent declarations disagree",
        );
    }

    let tx = tx_at(&db, &join).await;
    let store = S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
    store.apply(events("resolved")).await.unwrap();
    let resolved = tx.commit().await.unwrap();
    // A dominated declaration reachable from another parent is not a conflict.
    let next = write::<B>(&db, "subject", &[&right, &resolved], "next")
        .await
        .unwrap();
    let found = type_at(&db, "subject", &[&next], &next).await.unwrap();
    assert_eq!(
        (found.type_id.as_str(), found.declared_by),
        (B::TYPE_ID, vec![resolved])
    );
    assert_eq!(
        read_at::<B>(&db, "subject", &[&next])
            .await
            .unwrap()
            .into_value()
            .0,
        ["resolved", "next"]
    );
    // Equal concurrent declarations agree, and both are reported.
    let again = declare::<B>(&db, "subject", &[&next]).await;
    let tx = tx_at(&db, &[&next]).await;
    S::<B>::declare(&tx, "subject", Doc::new()).await.unwrap();
    tx.get_store::<S>("distinct").await.unwrap();
    let other = tx.commit().await.unwrap();
    assert_ne!(again, other);
    let joined = write::<B>(&db, "subject", &[&again, &other], "x")
        .await
        .unwrap();
    let found = type_at(&db, "subject", &[&joined], &joined).await.unwrap();
    assert_eq!(found.type_id, B::TYPE_ID);
    assert_eq!(Snapshot::from(found.declared_by), at(&[&again, &other]));
}

#[tokio::test]
async fn test_structured_removal_replacement_and_malformed_rows_do_not_resurrect() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject").await.unwrap();
    let origin = tx.commit().await.unwrap();
    let parent = db.get_entry(&origin).await.unwrap();
    let mut deleted = Doc::new();
    deleted.set_literal("subject", Value::Deleted);
    let mut replaced = Doc::new();
    let mut row = Doc::atomic();
    row.set("config", Value::Doc(Doc::new()));
    replaced.set_literal("subject", Value::Doc(row));
    let mut malformed = Doc::new();
    malformed.set_literal("subject", "not a row");
    for (index, reason) in [
        (&deleted, "removed"),
        (&replaced, "removed"),
        (&malformed, "malformed"),
    ] {
        let gone = signed_entry(&fixture, &db, &[&parent], &[&parent], None, Some(index));
        let gone = receive(&fixture, &db, gone).await;
        let gone_entry = db.get_entry(&gone).await.unwrap();
        let value = signed_entry(
            &fixture,
            &db,
            &[&gone_entry],
            &[&gone_entry],
            Some(native("x")),
            None,
        );
        let value = receive(&fixture, &db, value).await;
        assert_unresolved(
            type_at(&db, "subject", &[&value], &value)
                .await
                .err()
                .unwrap(),
            reason,
        );
        // A removal racing a declaration is ambiguous in either order.
        let kept = declare::<B>(&db, "subject", &[&origin]).await;
        for join in [[&gone, &kept], [&kept, &gone]] {
            assert!(write::<B>(&db, "subject", &join, "x").await.is_err());
        }
        // An explicit descendant declaration is a fresh start.
        let fresh = declare::<B>(&db, "subject", &[&gone]).await;
        write::<B>(&db, "subject", &[&fresh], "x").await.unwrap();
    }
}

#[tokio::test]
async fn test_structured_genesis_declares_and_writes() {
    let (fixture, _) = setup_tree().await;
    let db = Database::create_with_init(
        &fixture._client,
        fixture.key.clone(),
        Doc::new(),
        async |tx: &Transaction| {
            let store = tx.get_store::<S>("subject").await?;
            store.apply(events("genesis")).await
        },
    )
    .await
    .unwrap();
    let root = db.root_id().clone();
    let found = type_at(&db, "subject", &[&root], &root).await.unwrap();
    assert_eq!(
        (found.type_id.as_str(), found.declared_by),
        (A::TYPE_ID, vec![root.clone()])
    );
    let next = write::<A>(&db, "subject", &[&root], "next").await.unwrap();
    let state = read_at::<A>(&db, "subject", &[&next]).await.unwrap();
    assert_eq!(state.into_value().0, ["genesis", "next"]);
}

#[tokio::test]
async fn test_structured_main_history_types_an_independent_store_branch() {
    let (fixture, db, _, e1, f) = offline_fixture().await;
    let parent = db.get_entry(&e1).await.unwrap();
    // No Store parents: consumes nothing, but has seen the change to B.
    let raw = signed_entry(&fixture, &db, &[&parent], &[], Some(native("branch")), None);
    let raw = receive(&fixture, &db, raw).await;
    let found = type_at(&db, "subject", &[&raw], &raw).await.unwrap();
    assert_eq!(
        (found.type_id.as_str(), found.declared_by),
        (B::TYPE_ID, vec![e1.clone()])
    );
    assert_eq!(
        read_at::<B>(&db, "subject", &[&raw])
            .await
            .unwrap()
            .into_value()
            .0
            .len(),
        2
    );
    // Seen through main links and not consumed is still part of the frontier.
    let a = db.get_entry(&f).await.unwrap();
    let late = signed_entry(&fixture, &db, &[&a], &[], Some(native("late")), None);
    let late = receive(&fixture, &db, late).await;
    assert_eq!(
        type_at(&db, "subject", &[&late], &late)
            .await
            .unwrap()
            .type_id,
        A::TYPE_ID
    );
    assert_eq!(
        read_at::<A>(&db, "subject", &[&late])
            .await
            .unwrap()
            .into_value()
            .0
            .len(),
        2
    );
}

#[tokio::test]
async fn test_structured_unusable_contributions_are_retained_not_skipped() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject")
        .await
        .unwrap()
        .apply(events("a"))
        .await
        .unwrap();
    let a = tx.commit().await.unwrap();
    let parent = db.get_entry(&a).await.unwrap();
    for bad in [
        vec![],
        b"{}".to_vec(),
        vec![0xa0],
        [native("b"), vec![0]].concat(),
    ] {
        let raw = signed_entry(
            &fixture,
            &db,
            &[&parent],
            &[&parent],
            Some(bad.clone()),
            None,
        );
        let bytes = raw.canonical_bytes().unwrap();
        let id = receive(&fixture, &db, raw).await;
        // Zero-length is still an A contribution. The Type rejects the value.
        assert_eq!(
            type_at(&db, "subject", &[&id], &id).await.unwrap().type_id,
            A::TYPE_ID
        );
        assert!(read_at::<A>(&db, "subject", &[&id]).await.is_err());
        assert_eq!(
            db.get_entry(&id).await.unwrap().canonical_bytes().unwrap(),
            bytes
        );
        assert!(db.verify_entry_signature(&id).await.unwrap());
        assert_eq!(
            read_at::<A>(&db, "subject", &[&a])
                .await
                .unwrap()
                .into_value(),
            events("a")
        );
    }

    // Legacy bytes keep their own Type and reader after a new declaration.
    let tx = db.new_transaction_at(&at(&[&a])).await.unwrap();
    tx.get_store::<DocStore>("legacy")
        .await
        .unwrap()
        .set("k", "v")
        .await
        .unwrap();
    let legacy = tx.commit().await.unwrap();
    let original = db
        .get_entry(&legacy)
        .await
        .unwrap()
        .canonical_bytes()
        .unwrap();
    let adopted = declare::<A>(&db, "legacy", &[&legacy]).await;
    assert_eq!(
        type_at(&db, "legacy", &[&adopted], &legacy)
            .await
            .unwrap()
            .type_id,
        "docstore:v0"
    );
    assert_unresolved(
        read_at::<A>(&db, "legacy", &[&adopted])
            .await
            .err()
            .unwrap(),
        "unsupported Type",
    );
    assert!(write::<A>(&db, "legacy", &[&adopted], "x").await.is_err());
    assert_eq!(
        db.get_entry(&legacy)
            .await
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        original
    );
    let old = tx_at(&db, &[&legacy]).await;
    assert_eq!(
        old.get_store::<DocStore>("legacy")
            .await
            .unwrap()
            .get("k")
            .await
            .unwrap()
            .as_text(),
        Some("v")
    );
    // Data whose Store was never registered has no Type to guess.
    assert_unresolved(
        tx_at(&db, &[&a])
            .await
            .inspect_contribution("_settings", db.root_id())
            .await
            .err()
            .unwrap(),
        "no Type was declared",
    );
}

#[tokio::test]
async fn test_structured_config_belongs_to_the_type() {
    let (_fixture, db) = setup_tree().await;
    let mode = |mode: &str| {
        let mut config = Doc::new();
        config.set("mode", mode);
        config.set("label", "ignored by the grammar");
        config
    };
    let tx = db.new_transaction().await.unwrap();
    let store = S::<Tuned>::declare(&tx, "subject", mode("x"))
        .await
        .unwrap();
    store.apply(events("x")).await.unwrap();
    let x = tx.commit().await.unwrap();

    // A grammar-neutral edit neither declares a Type nor changes the view.
    let tx = tx_at(&db, &[&x]).await;
    let mut label = Doc::new();
    label.set("label", "People");
    let store = tx.get_store::<S<Tuned>>("subject").await.unwrap();
    store.set_config(label).await.unwrap();
    store.apply(events("same")).await.unwrap();
    let same = tx.commit().await.unwrap();
    let index = Doc::decode(db.get_entry(&same).await.unwrap().data("_index").unwrap()).unwrap();
    let row = index.get_literal("subject").unwrap().as_doc().unwrap();
    assert!(row.get_literal("type").is_none() && row.get_literal("config").is_some());
    assert_eq!(
        type_at(&db, "subject", &[&same], &same)
            .await
            .unwrap()
            .declared_by,
        std::slice::from_ref(&x)
    );
    assert_eq!(
        read_at::<Tuned>(&db, "subject", &[&same])
            .await
            .unwrap()
            .into_value()
            .0,
        ["x", "same"]
    );
    let config = tx_at(&db, &[&same]).await;
    let config = config
        .get_store::<S<Tuned>>("subject")
        .await
        .unwrap()
        .get_config()
        .await
        .unwrap();
    assert_eq!(config.get_as::<&str>("mode"), Some("x"));
    assert_eq!(config.get_as::<&str>("label"), Some("People"));

    // A config change under one Type is not a new Type, and is not combined.
    let tx = tx_at(&db, &[&same]).await;
    let store = tx.get_store::<S<Tuned>>("subject").await.unwrap();
    store.set_config(mode("y")).await.unwrap();
    assert!(store.apply(events("y")).await.is_err());
    let y = tx.commit().await.unwrap();
    assert_eq!(
        read_at::<Tuned>(&db, "subject", &[&y])
            .await
            .unwrap()
            .into_value()
            .0,
        ["x", "same"],
        "old values keep the config they were written under"
    );
    assert!(write::<Tuned>(&db, "subject", &[&y], "y").await.is_err());
    // Bytes encoded for x are not retagged by a later staged config.
    let tx = tx_at(&db, &[&same]).await;
    let store = tx.get_store::<S<Tuned>>("subject").await.unwrap();
    store.apply(events("x")).await.unwrap();
    store.set_config(mode("y")).await.unwrap();
    assert_unresolved(tx.commit().await.err().unwrap(), "different Store config");
    // The Type cannot resolve concurrent configs, so a join is refused while
    // each branch stays readable.
    let tx = tx_at(&db, &[&same]).await;
    let store = tx.get_store::<S<Tuned>>("subject").await.unwrap();
    store.set_config(mode("z")).await.unwrap();
    let z = tx.commit().await.unwrap();
    for branch in [&y, &z] {
        assert!(read_at::<Tuned>(&db, "subject", &[branch]).await.is_ok());
    }
    assert!(
        write::<Tuned>(&db, "subject", &[&y, &z], "?")
            .await
            .is_err()
    );
    // Neither does the half of a join that matches the values already there.
    let tx = tx_at(&db, &[&same]).await;
    let store = tx.get_store::<S<Tuned>>("subject").await.unwrap();
    store.set_config(mode("x")).await.unwrap();
    let kept = tx.commit().await.unwrap();
    for other in [&y, &z] {
        let error = write::<Tuned>(&db, "subject", &[&kept, other], "?")
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("ambiguous"), "{error}");
    }
    let tx = tx_at(&db, &[&y, &z]).await;
    S::<Tuned>::declare(&tx, "subject", mode("x"))
        .await
        .unwrap();
    let settled = tx.commit().await.unwrap();
    let last = write::<Tuned>(&db, "subject", &[&settled], "last")
        .await
        .unwrap();
    assert_eq!(
        read_at::<Tuned>(&db, "subject", &[&last])
            .await
            .unwrap()
            .into_value()
            .0,
        ["x", "same", "last"]
    );
    // A config edit made through another Type's handle is refused at commit.
    let tx = tx_at(&db, &[&last]).await;
    let stale = S::<A>::load(&tx, "subject".into()).await.unwrap();
    stale.set_config(mode("a")).await.unwrap();
    assert_unresolved(tx.commit().await.err().unwrap(), "config was edited");
    // Settings are independent of both.
    let tx = tx_at(&db, &[&last]).await;
    let store = tx.get_store::<S<Tuned>>("subject").await.unwrap();
    store.set_height_strategy(None).await.unwrap();
    let settings = tx.commit().await.unwrap();
    write::<Tuned>(&db, "subject", &[&settings], "ok")
        .await
        .unwrap();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Numbers {
    lo: i64,
    hi: u64,
    #[serde(with = "eidetica::store::schema::u128_bytes")]
    wide_u: u128,
    #[serde(with = "eidetica::store::schema::i128_bytes")]
    wide_i: i128,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
    pairs: KeyedPairs<(i64, u64), String>,
}

structured_type!(Rows, "rows:v0.1", Map<String, Lww<Numbers>>);

#[test]
fn test_structured_domains_and_complete_nested_state() {
    for (u, i) in [
        (0, 0),
        (1, -1),
        (u128::MAX, i128::MIN),
        (u128::MAX, i128::MAX),
    ] {
        let record = Numbers {
            lo: i64::MIN,
            hi: u64::MAX,
            wide_u: u,
            wide_i: i,
            bytes: vec![0, 255],
            pairs: KeyedPairs(vec![((1, 2), "b".into()), ((-1, 0), "a".into())]),
        };
        let mut map = Map::new();
        map.insert("".to_string(), Lww::Set(record));
        map.insert("literal.key".into(), Lww::Delete);
        map.insert("λ".into(), Lww::NoOp);
        let encoded = StructuredState::<Rows>::new(map.clone()).encode().unwrap();
        assert_eq!(
            encoded,
            dag_cbor::encode(&map).unwrap(),
            "no common framing"
        );
        let decoded = StructuredState::<Rows>::decode(&encoded).unwrap();
        assert_eq!(decoded.encode().unwrap(), encoded);
        let state = decoded.into_value();
        assert_eq!(state.get("literal.key"), Some(&Lww::Delete));
        assert_eq!(state.get("λ"), Some(&Lww::NoOp));
        let Lww::Set(record) = state.get("").unwrap() else {
            panic!()
        };
        assert_eq!(
            (record.lo, record.hi, record.wide_u, record.wide_i),
            (i64::MIN, u64::MAX, u, i)
        );
        let mut next = Map::new();
        next.insert("".into(), Lww::Delete);
        assert_eq!(state.merge(&next).unwrap(), map.merge(&next).unwrap());
        let mut raw: Ipld = dag_cbor::decode(&encoded).unwrap();
        let Ipld::Map(rows) = &mut raw else { panic!() };
        let Ipld::Map(set) = rows.get_mut("").unwrap() else {
            panic!()
        };
        let Ipld::Map(fields) = set.get_mut("Set").unwrap() else {
            panic!()
        };
        fields.insert("wide_u".into(), Ipld::Bytes(vec![0; 15]));
        assert!(StructuredState::<Rows>::decode(&dag_cbor::encode(&raw).unwrap()).is_err());
    }
    let btree = BTreeMap::from([("z".to_owned(), 1u64), ("aa".to_owned(), 2)]);
    let hash = HashMap::from([("aa".to_owned(), 2u64), ("z".to_owned(), 1)]);
    assert_eq!(
        dag_cbor::encode(&btree).unwrap(),
        dag_cbor::encode(&hash).unwrap()
    );
    let pairs = KeyedPairs(vec![(2u64, 1u64), (1, 2)]);
    assert_eq!(
        dag_cbor::encode(&pairs).unwrap(),
        dag_cbor::encode(&KeyedPairs(vec![(1u64, 2u64), (2, 1)])).unwrap()
    );
    assert!(dag_cbor::encode(&KeyedPairs(vec![(1u64, 2u64), (1, 3)])).is_err());
    assert!(dag_cbor::encode(&KeyedPairs(vec![(0.0f64, 1u64), (-0.0, 2)])).is_err());
    for pair in [(i64::MIN, 0u64), (i64::MAX, u64::MAX)] {
        let bytes = dag_cbor::encode(&pair).unwrap();
        assert_eq!(dag_cbor::decode::<(i64, u64)>(&bytes).unwrap(), pair);
    }
    assert!(
        dag_cbor::decode::<KeyedPairs<u64, u64>>(
            &dag_cbor::encode(&vec![(2u64, 1u64), (1, 2)]).unwrap()
        )
        .is_err()
    );
    // A known Type cannot authorize lossy application decoding.
    #[derive(Clone, Serialize, Deserialize)]
    struct Partial {
        retained: u64,
    }
    impl CRDT for Partial {
        fn merge(&self, other: &Self) -> Result<Self> {
            Ok(other.clone())
        }
    }
    structured_type!(Lossy, "lossy:v0.1", Partial);
    let bytes = dag_cbor::encode(&BTreeMap::from([
        ("retained".to_owned(), 1u64),
        ("lost".to_owned(), 2),
    ]))
    .unwrap();
    assert!(StructuredState::<Lossy>::decode(&bytes).is_err());
    assert!(!StructuredState::<A>::default().has_source());
    assert!(StructuredState::<A>::default().encode().is_err());
}

async fn request(db: &Database, name: &str, main: &Snapshot, encrypted: bool) -> StoreStateRequest {
    let backend = db.backend().unwrap();
    let source = backend
        .store_snapshot_at(db.root_id(), name, main)
        .await
        .unwrap();
    StoreStateRequest {
        database: db.root_id().clone(),
        store: name.into(),
        lifecycle: StoreStateLifecycle::Derived,
        scope: CacheScope::Shared,
        source_key: source.cache_key_bytes(),
        projection: ProjectionDescriptor {
            name: if encrypted {
                "eidetica/password/eidetica/structured-opaque"
            } else {
                "eidetica/structured-opaque"
            }
            .into(),
            version: 3,
        },
    }
}
async fn seed(db: &Database, req: StoreStateRequest, bytes: Vec<u8>) {
    let backend = db.backend().unwrap();
    let token = backend.begin_store_state_staging(req).await.unwrap();
    backend
        .stage_store_state_records(
            &token,
            BTreeMap::from([(OPAQUE_STATE_KEY.to_vec(), Some(bytes))]),
        )
        .await
        .unwrap();
    backend.publish_store_state(token).await.unwrap();
}

#[tokio::test]
async fn test_structured_mixed_stores_cache_and_legacy_formats() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("empty").await.unwrap();
    let empty = tx.commit().await.unwrap();
    for _ in 0..2 {
        assert!(
            !read_at::<A>(&db, "empty", &[&empty])
                .await
                .unwrap()
                .has_source()
        );
    }
    let req = request(&db, "empty", &at(&[&empty]), false).await;
    let backend = db.backend().unwrap();
    assert!(backend.resolve_store_state(&req).await.unwrap().is_none());

    let tx = db.new_transaction().await.unwrap();
    let first = tx.get_store::<S>("first").await.unwrap();
    let second = tx.get_store::<S<B>>("second.雪").await.unwrap();
    first.apply(events("one")).await.unwrap();
    second.apply(events("two")).await.unwrap();
    tx.get_store::<DocStore>("legacy")
        .await
        .unwrap()
        .set("k", "legacy")
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let original = db.get_entry(&id).await.unwrap();
    let bytes = original.canonical_bytes().unwrap();
    for (name, expected) in [
        ("first", A::TYPE_ID),
        ("second.雪", B::TYPE_ID),
        ("legacy", "docstore:v0"),
    ] {
        // Inspection needs no code for the Type it reports.
        let found = type_at(&db, name, &[&id], &id).await.unwrap();
        assert_eq!(
            (found.type_id.as_str(), found.declared_by),
            (expected, vec![id.clone()])
        );
    }
    let check = db.new_transaction().await.unwrap();
    assert!(check.get_store::<DocStore>("first").await.is_err());
    assert!(check.get_store::<S>("legacy").await.is_err());
    assert!(check.get_store::<S>("second.雪").await.is_err());
    assert!(read_at::<A>(&db, "legacy", &[&id]).await.is_err());
    // The path form of a literal name is a different, ambiguous registration.
    let tx = db.new_transaction().await.unwrap();
    assert!(tx.get_store::<DocStore>("second.雪").await.is_err());
    tx.get_index()
        .await
        .unwrap()
        .set_entry("second.雪", B::TYPE_ID, Doc::new())
        .await
        .unwrap();
    let alias = tx.commit().await.unwrap();
    assert_unresolved(
        write::<B>(&db, "second.雪", &[&alias], "x")
            .await
            .err()
            .unwrap(),
        "malformed registration",
    );

    for clear in [false, false, true] {
        if clear {
            fixture
                .source
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        assert_eq!(
            read_at::<A>(&db, "first", &[&id])
                .await
                .unwrap()
                .into_value(),
            events("one")
        );
        assert_eq!(
            read_at::<B>(&db, "second.雪", &[&id])
                .await
                .unwrap()
                .into_value(),
            events("two")
        );
    }
    // A cached value of another Type, or untyped bytes, is never decoded.
    let record = {
        let req = request(&db, "second.雪", &at(&[&id]), false).await;
        let view = backend.resolve_store_state(&req).await.unwrap().unwrap();
        backend
            .store_state_record_get(&view, OPAQUE_STATE_KEY)
            .await
            .unwrap()
            .unwrap()
    };
    fixture
        .source
        .backend()
        .clear_derived_store_state()
        .await
        .unwrap();
    for poison in [record, native("poison")] {
        let req = request(&db, "first", &at(&[&id]), false).await;
        seed(&db, req, poison).await;
        assert!(read_at::<A>(&db, "first", &[&id]).await.is_err());
        // Nor can a matching cache overrule the source.
        assert_unresolved(
            read_at::<B>(&db, "first", &[&id]).await.err().unwrap(),
            "unsupported Type",
        );
        fixture
            .source
            .backend()
            .clear_derived_store_state()
            .await
            .unwrap();
    }
    assert_eq!(
        read_at::<A>(&db, "first", &[&id])
            .await
            .unwrap()
            .into_value(),
        events("one")
    );
    let after = db.get_entry(&id).await.unwrap();
    assert_eq!(after.canonical_bytes().unwrap(), bytes);
    assert_eq!(after.auth(), original.auth());
    assert!(db.verify_entry_signature(&id).await.unwrap());
    assert_eq!(
        db.get_store_viewer::<DocStore>("legacy")
            .await
            .unwrap()
            .get("k")
            .await
            .unwrap()
            .as_text(),
        Some("legacy")
    );
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
#[derive(Default)]
struct Fallible(Vec<String>);
impl Serialize for Fallible {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        if self.0.iter().any(|x| x == "serialize-secret") {
            return Err(serde::ser::Error::custom("serialize-secret"));
        }
        self.0.serialize(s)
    }
}
impl CRDT for Fallible {
    fn merge(&self, other: &Self) -> Result<Self> {
        if other.0.iter().any(|x| x == "merge-secret") {
            return Err(eidetica::store::StoreError::ImplementationError {
                store: "secret-schema".into(),
                reason: "merge-secret".into(),
            }
            .into());
        }
        Ok(Self(self.0.iter().chain(&other.0).cloned().collect()))
    }
}
structured_type!(Fa, "fallible:v0.1", Fallible);
structured_type!(Fb, "fallible:v0.2", Fallible);
type F<T = Fa> = StructuredStore<T>;

#[tokio::test]
async fn test_structured_atomic_staging_and_cloned_handles() {
    let (_fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    let s = tx.get_store::<F>("subject").await.unwrap();
    s.apply(Fallible(vec!["first".into()])).await.unwrap();
    let before = s.local_data().unwrap().unwrap().encode().unwrap();
    for failure in ["merge-secret", "serialize-secret"] {
        assert!(s.apply(Fallible(vec![failure.into()])).await.is_err());
        assert_eq!(s.local_data().unwrap().unwrap().encode().unwrap(), before);
    }
    assert!(tx.get_store::<F<Fb>>("subject").await.is_err());
    assert_eq!(s.local_data().unwrap().unwrap().encode().unwrap(), before);
    let mut joins = vec![];
    for i in 0..12 {
        let handle = s.clone();
        joins.push(tokio::spawn(async move {
            handle.apply(Fallible(vec![format!("{i}")])).await.unwrap();
        }));
    }
    for join in joins {
        join.await.unwrap();
    }
    assert_eq!(s.read().await.unwrap().into_value().0.len(), 13);
    tx.get_store::<DocStore>("unrelated")
        .await
        .unwrap()
        .set("k", "kept")
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let reopened = db.get_store_viewer::<F>("subject").await.unwrap();
    assert_eq!(reopened.read().await.unwrap().into_value().0.len(), 13);
    assert_eq!(
        type_at(&db, "subject", &[&id], &id).await.unwrap().type_id,
        Fa::TYPE_ID
    );
    assert_eq!(
        db.get_store_viewer::<DocStore>("unrelated")
            .await
            .unwrap()
            .get("k")
            .await
            .unwrap()
            .as_text(),
        Some("kept")
    );
    for name in ["", "_index"] {
        let tx = db.new_transaction().await.unwrap();
        assert!(S::<A>::load(&tx, name.into()).await.is_err());
    }
}

structured_type!(Pa, "private:v0.1", Lww<u64>);
structured_type!(Pb, "private:a-much-longer-application-name:v0.2", Lww<u64>);
type E<T> = PasswordStore<StructuredStore<T>>;

async fn unlocked<T: StructuredType>(tx: &Transaction) -> E<T> {
    let mut wrapped = tx.get_store::<E<T>>("secret").await.unwrap();
    wrapped.open("test-password").unwrap();
    wrapped
}
async fn public_config(db: &Database, id: &ID) -> eidetica::store::PasswordStoreConfig {
    let index = Doc::decode(db.get_entry(id).await.unwrap().data("_index").unwrap()).unwrap();
    let row = index.get_literal("secret").unwrap().as_doc().unwrap();
    let config = row.get_literal("config").unwrap().as_doc().unwrap();
    eidetica::store::PasswordStoreConfig::try_from(config).unwrap()
}

#[tokio::test]
async fn test_structured_encrypted_inner_type_is_historical_and_private() {
    let (_fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    let mut wrapped = tx.get_store::<E<Pa>>("secret").await.unwrap();
    wrapped
        .initialize("test-password", Doc::new())
        .await
        .unwrap();
    wrapped
        .inner()
        .await
        .unwrap()
        .apply(Lww::Set(7))
        .await
        .unwrap();
    let first = tx.commit().await.unwrap();

    // Same Entry, both orders.
    let tx = tx_at(&db, &[&first]).await;
    let wrapped = unlocked::<Pa>(&tx).await;
    wrapped
        .inner()
        .await
        .unwrap()
        .apply(Lww::Set(8))
        .await
        .unwrap();
    let wrapped = wrapped
        .declare_inner::<StructuredStore<Pb>>(Doc::new())
        .await
        .unwrap();
    assert!(
        wrapped
            .inner()
            .await
            .unwrap()
            .apply(Lww::Set(9))
            .await
            .is_err()
    );
    let error = tx.commit().await.err().unwrap();
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("private:"), "{text}");
    }
    let mut forks = Vec::new();
    for _ in 0..2 {
        let tx = tx_at(&db, &[&first]).await;
        let wrapped = unlocked::<Pa>(&tx).await;
        let wrapped = wrapped
            .declare_inner::<StructuredStore<Pb>>(Doc::new())
            .await
            .unwrap();
        let id = tx.commit().await.unwrap();
        assert!(wrapped.is_open());
        let entry = db.get_entry(&id).await.unwrap();
        assert!(entry.data("secret").is_err());
        // No public Type change, and no length that depends on the inner Type.
        let text = String::from_utf8(entry.data("_index").unwrap().clone()).unwrap();
        assert!(
            !text.contains("private:") && !text.contains("\"type\""),
            "{text}"
        );
        let (before, after) = (
            public_config(&db, &first).await,
            public_config(&db, &id).await,
        );
        assert_eq!(
            before.wrapped_config.ciphertext.len(),
            after.wrapped_config.ciphertext.len()
        );
        assert_ne!(before.wrapped_config, after.wrapped_config);
        forks.push(id);
    }
    let [left, right] = &forks[..] else { panic!() };
    // The public Type and bytes say nothing about the inner Type.
    for id in [&first, left] {
        assert_eq!(
            type_at(&db, "secret", &[id], &first).await.unwrap().type_id,
            "encrypted:password:v0"
        );
    }
    // Separately encrypted equal metadata is not a conflict.
    let tx = tx_at(&db, &[left, right]).await;
    let wrapped = unlocked::<Pb>(&tx).await;
    assert_eq!(
        wrapped
            .inspect_contribution(&first)
            .await
            .unwrap()
            .unwrap()
            .type_id,
        Pa::TYPE_ID,
        "today's inner Type must not relabel it"
    );
    assert!(wrapped.inner().await.unwrap().read().await.is_err());
    let locked = tx.get_store::<E<Pb>>("secret").await.unwrap();
    assert!(locked.inspect_contribution(&first).await.is_err());
    // A direct typed handle cannot reach past a locked wrapper.
    let tx = tx_at(&db, &[left, right]).await;
    assert!(
        S::<Pa>::load(&tx, "secret".into())
            .await
            .unwrap()
            .read()
            .await
            .is_err()
    );
    assert!(S::<Pa>::declare(&tx, "secret", Doc::new()).await.is_err());
    assert_eq!(
        unlocked::<Pa>(&tx_at(&db, &[&first]).await)
            .await
            .inner()
            .await
            .unwrap()
            .read()
            .await
            .unwrap()
            .into_value(),
        Lww::Set(7)
    );
    // An inner config conflict is unavailable, not whichever row is displayed.
    let mut configs = Vec::new();
    for mode in ["x", "y"] {
        let mut config = Doc::new();
        config.set("mode", mode);
        let tx = tx_at(&db, &[&first]).await;
        unlocked::<Pa>(&tx)
            .await
            .declare_inner::<StructuredStore<Pa>>(config)
            .await
            .unwrap();
        configs.push(tx.commit().await.unwrap());
    }
    let tx = tx_at(&db, &[&configs[0], left]).await;
    let displayed = tx.get_index().await.unwrap();
    assert!(
        displayed
            .set_subtree_settings("secret", Default::default())
            .await
            .is_err()
    );
    for join in [[&configs[0], left], [left, &configs[0]]] {
        let tx = tx_at(&db, &join).await;
        let mut opened = tx.get_store::<E<Pa>>("secret").await.unwrap();
        if opened.open("test-password").is_ok() {
            opened
                .inner()
                .await
                .unwrap()
                .apply(Lww::Set(1))
                .await
                .unwrap();
            assert!(tx.commit().await.is_err());
        }
        let tx = tx_at(&db, &join).await;
        let mut opened = tx.get_store::<E<Pb>>("secret").await.unwrap();
        if opened.open("test-password").is_ok() {
            assert!(
                opened
                    .inner()
                    .await
                    .unwrap()
                    .apply(Lww::Set(1))
                    .await
                    .is_err()
            );
        }
    }
    // Equal-size values have equal ciphertext and cache sizes under either Type.
    let tx = db.new_transaction_at(&at(&[&first])).await.unwrap();
    unlocked::<Pa>(&tx)
        .await
        .inner()
        .await
        .unwrap()
        .apply(Lww::Set(9))
        .await
        .unwrap();
    let next = tx.commit().await.unwrap();
    let len = async |id: &ID| {
        db.get_entry(id)
            .await
            .unwrap()
            .data("secret")
            .unwrap()
            .len()
    };
    assert_eq!(len(&first).await, len(&next).await);
}

#[tokio::test]
async fn test_structured_encrypted_inner_cache_and_redaction() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    let mut wrapped = tx.get_store::<PasswordStore<F>>("secret").await.unwrap();
    assert!(wrapped.inner().await.is_err());
    wrapped
        .initialize("test-password", Doc::new())
        .await
        .unwrap();
    let s = wrapped.inner().await.unwrap();
    s.apply(Fallible(vec!["first".into()])).await.unwrap();
    for failure in ["merge-secret", "serialize-secret"] {
        let error = s.apply(Fallible(vec![failure.into()])).await.err().unwrap();
        for rendering in [error.to_string(), format!("{error:?}")] {
            assert!(!rendering.contains(failure));
            assert!(!rendering.contains("secret-schema"));
        }
    }
    let id = tx.commit().await.unwrap();
    let original = db.get_entry(&id).await.unwrap();
    let bytes = original.canonical_bytes().unwrap();
    assert!(dag_cbor::decode::<Ipld>(original.data("secret").unwrap()).is_err());
    let bare = db.get_store_viewer::<F>("secret").await.unwrap();
    assert!(bare.read().await.is_err());
    for _ in 0..2 {
        let tx = db.new_transaction().await.unwrap();
        let mut wrapped = tx.get_store::<PasswordStore<F>>("secret").await.unwrap();
        assert!(wrapped.open("wrong").is_err());
        wrapped.open("test-password").unwrap();
        assert_eq!(
            wrapped
                .inner()
                .await
                .unwrap()
                .read()
                .await
                .unwrap()
                .into_value(),
            Fallible(vec!["first".into()])
        );
        let wrong = tx
            .get_store::<PasswordStore<F<Fb>>>("secret")
            .await
            .unwrap();
        // A locked wrapper cannot construct an inner handle even if another
        // handle on this transaction already registered an encryptor.
        assert!(wrong.inner().await.is_err());
    }
    assert_eq!(
        db.get_entry(&id).await.unwrap().canonical_bytes().unwrap(),
        bytes
    );
    let snapshot = Snapshot::from([id]);
    let encrypted_request = request(&db, "secret", &snapshot, true).await;
    assert!(
        db.backend()
            .unwrap()
            .resolve_store_state(&encrypted_request)
            .await
            .unwrap()
            .is_some(),
        "the actual encrypted layout must be routed"
    );
    assert!(
        db.backend()
            .unwrap()
            .resolve_store_state(&request(&db, "secret", &snapshot, false).await)
            .await
            .unwrap()
            .is_none()
    );
    #[cfg(all(unix, feature = "service"))]
    if let Some((_, dir)) = &fixture._service {
        use eidetica::auth::types::{AuthKey, Permission};
        crate::helpers::create_account(&fixture.source, "reader", "")
            .await
            .unwrap();
        let second = eidetica::Instance::connect(format!(
            "unix://{}",
            dir.path().join("structured.sock").display()
        ))
        .await
        .unwrap();
        let user = second.login_account("reader", "").await.unwrap();
        let public = user.get_default_key().unwrap();
        // A denied request is not mistaken for an empty, supported typed view.
        assert!(
            second
                .backend()
                .resolve_store_state(&encrypted_request)
                .await
                .is_err()
        );
        let tx = db.new_transaction().await.unwrap();
        tx.get_settings()
            .unwrap()
            .set_auth_key(&public, AuthKey::active(None, Permission::Read))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Now Read is genuinely granted, yet the first login's client cache
        // remains invisible. A typed read with the password builds its own.
        assert!(
            second
                .backend()
                .resolve_store_state(&encrypted_request)
                .await
                .unwrap()
                .is_none()
        );
        let foreign = StoreStateRequest {
            scope: CacheScope::Login("foreign-login".into()),
            ..encrypted_request.clone()
        };
        assert!(
            second
                .backend()
                .resolve_store_state(&foreign)
                .await
                .is_err()
        );
        let second_db = Database::open(&second, db.root_id())
            .await
            .unwrap()
            .with_key(user.get_signing_key(&public).unwrap());
        // A verified Read login can build its own bounded private cache.
        let mut wrapped = second_db
            .get_store_viewer::<PasswordStore<F>>("secret")
            .await
            .unwrap();
        assert!(wrapped.inner().await.is_err());
        wrapped.open("test-password").unwrap();
        assert_eq!(
            wrapped
                .inner()
                .await
                .unwrap()
                .read()
                .await
                .unwrap()
                .into_value(),
            Fallible(vec!["first".into()])
        );
        assert!(
            second
                .backend()
                .resolve_store_state(&encrypted_request)
                .await
                .unwrap()
                .is_some()
        );
    }
    fixture
        .source
        .backend()
        .clear_derived_store_state()
        .await
        .unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut wrong = tx
        .get_store::<PasswordStore<F<Fb>>>("secret")
        .await
        .unwrap();
    let error = wrong.open("test-password").err().unwrap();
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains(Fa::TYPE_ID), "{text}");
    }
}
