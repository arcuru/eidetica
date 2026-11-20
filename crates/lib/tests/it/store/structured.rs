//! Shared attribution through real public Store/transaction/backend boundaries.
use std::{
    collections::{BTreeMap, HashMap},
    sync::atomic::{AtomicUsize, Ordering},
};

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

fn signed_contribution(fixture: &Fixture, db: &Database, parent: &Entry, bytes: Vec<u8>) -> Entry {
    Entry::builder(db.root_id().clone())
        .set_parents(vec![parent.id()])
        .set_height(parent.height() + 1)
        .set_subtree_parents("subject", vec![parent.id()])
        .set_subtree_data("subject", bytes)
        .set_metadata(parent.metadata().unwrap().clone())
        .set_auth(eidetica::auth::types::AuthInfo::from_pubkey(
            &fixture.key.public_key(),
        ))
        .build()
        .unwrap()
        .sign(&fixture.key)
        .unwrap()
}
use eidetica::{
    Database, Result, Snapshot, Store,
    backend::{CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest},
    crdt::{CRDT, Codec, Doc, Lww, Map, dag_cbor},
    entry::Entry,
    store::{
        DocStore, OPAQUE_STATE_KEY, PasswordStore, StructuredStore,
        schema::{
            Attributed, ContractDescriptor, ContractId, KeyedPairs, StoreContract, inspect_contract,
        },
    },
};
use ipld_core::ipld::Ipld;
use serde::{Deserialize, Serialize};

struct A;
struct B;
struct Other;
impl StoreContract for A {
    const ID: ContractId = ContractId {
        store: "structured:v0.1",
        data: "example/events:v0.1",
        schema: "example/a:v0.1",
    };
}
impl StoreContract for B {
    const ID: ContractId = ContractId {
        schema: "example/a:v0.2",
        ..A::ID
    };
}
impl StoreContract for Other {
    const ID: ContractId = ContractId {
        store: "other:v0.2",
        ..B::ID
    };
}
type S<C = A> = StructuredStore<Events, C>;

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

static DECODES: AtomicUsize = AtomicUsize::new(0);
#[derive(Serialize)]
#[serde(transparent)]
struct Guarded(u64);
impl<'de> Deserialize<'de> for Guarded {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        DECODES.fetch_add(1, Ordering::SeqCst);
        u64::deserialize(d).map(Self)
    }
}

#[test]
fn test_structured_envelope_gates_before_application_decode() {
    let original = Attributed::<Guarded, A>::new(Guarded(7)).encode().unwrap();
    assert_eq!(
        inspect_contract(&original).unwrap(),
        ContractDescriptor::from(A::ID)
    );
    // Pin the common grammar independently of application/container layout.
    assert_eq!(
        hex::encode(&original),
        "a26576616c75650768636f6e7472616374a36464617461736578616d706c652f6576656e74733a76302e316573746f72656f737472756374757265643a76302e3166736368656d616e6578616d706c652f613a76302e31"
    );
    let canonical: Ipld = dag_cbor::decode(&original).unwrap();
    let mut malformed = vec![vec![], vec![0xa0], b"{}".to_vec()];
    for field in ["schema", "data", "store"] {
        for bad in [
            None,
            Some(Ipld::Null),
            Some(Ipld::String(String::new())),
            Some(Ipld::String("unknown:v9".into())),
        ] {
            let mut value = canonical.clone();
            let Ipld::Map(ref mut outer) = value else {
                unreachable!()
            };
            let Ipld::Map(contract) = outer.get_mut("contract").unwrap() else {
                unreachable!()
            };
            if let Some(bad) = bad {
                contract.insert(field.into(), bad);
            } else {
                contract.remove(field);
            }
            malformed.push(dag_cbor::encode(&value).unwrap());
        }
    }
    for at_contract in [false, true] {
        let mut value = canonical.clone();
        let Ipld::Map(ref mut outer) = value else {
            unreachable!()
        };
        if at_contract {
            let Ipld::Map(contract) = outer.get_mut("contract").unwrap() else {
                unreachable!()
            };
            contract.insert("extra".into(), Ipld::Null);
        } else {
            outer.insert("extra".into(), Ipld::Null);
        }
        malformed.push(dag_cbor::encode(&value).unwrap());
    }
    // Duplicate top-level value field. Canonical validation must not drop it.
    let mut duplicate = original.clone();
    duplicate[0] = 0xa3;
    duplicate.extend_from_slice(&[0x65, b'v', b'a', b'l', b'u', b'e', 0x07]);
    malformed.push(duplicate);
    let mut duplicate_contract = original.clone();
    let offset = duplicate_contract
        .windows(6)
        .position(|w| w == [0xa3, 0x64, b'd', b'a', b't', b'a'])
        .unwrap();
    duplicate_contract[offset] = 0xa4;
    duplicate_contract.extend(dag_cbor::encode("schema").unwrap());
    duplicate_contract.extend(dag_cbor::encode(A::ID.schema).unwrap());
    malformed.push(duplicate_contract);
    let mut trailing = original.clone();
    trailing.push(0);
    malformed.push(trailing);
    for bytes in malformed {
        DECODES.store(0, Ordering::SeqCst);
        assert!(
            Attributed::<Guarded, A>::decode(&bytes).is_err(),
            "{}",
            hex::encode(&bytes)
        );
        assert_eq!(DECODES.load(Ordering::SeqCst), 0);
    }
    DECODES.store(0, Ordering::SeqCst);
    assert_eq!(
        Attributed::<Guarded, A>::decode(&original)
            .unwrap()
            .value()
            .unwrap()
            .0,
        7
    );
    assert!(
        DECODES.load(Ordering::SeqCst) > 0,
        "positive decode control"
    );
    assert!(!Attributed::<Guarded, A>::default().has_source());
    assert!(Attributed::<Guarded, A>::default().encode().is_err());
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
        let encoded = Attributed::<_, A>::new(map.clone()).encode().unwrap();
        let decoded = Attributed::<Map<String, Lww<Numbers>>, A>::decode(&encoded).unwrap();
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
        let Ipld::Map(outer) = &mut raw else { panic!() };
        let Ipld::Map(rows) = outer.get_mut("value").unwrap() else {
            panic!()
        };
        let Ipld::Map(set) = rows.get_mut("").unwrap() else {
            panic!()
        };
        let Ipld::Map(fields) = set.get_mut("Set").unwrap() else {
            panic!()
        };
        fields.insert("wide_u".into(), Ipld::Bytes(vec![0; 15]));
        assert!(
            Attributed::<Map<String, Lww<Numbers>>, A>::decode(&dag_cbor::encode(&raw).unwrap())
                .is_err()
        );
    }
    let btree = BTreeMap::from([("z".to_owned(), 1u64), ("aa".to_owned(), 2)]);
    let hash = HashMap::from([("aa".to_owned(), 2u64), ("z".to_owned(), 1)]);
    assert_eq!(
        Attributed::<_, A>::new(btree).encode().unwrap(),
        Attributed::<_, A>::new(hash).encode().unwrap()
    );
    let pairs = KeyedPairs(vec![(2u64, 1u64), (1, 2)]);
    assert_eq!(
        dag_cbor::encode(&pairs).unwrap(),
        dag_cbor::encode(&KeyedPairs(vec![(1u64, 2u64), (2, 1)])).unwrap()
    );
    assert!(dag_cbor::encode(&KeyedPairs(vec![(1u64, 2u64), (1, 3)])).is_err());
    assert!(dag_cbor::encode(&KeyedPairs(vec![(0.0f64, 1u64), (-0.0, 2)])).is_err());
    for (lo, hi) in [(i64::MIN, 0u64), (i64::MAX, u64::MAX)] {
        let bytes = Attributed::<_, A>::new((lo, hi)).encode().unwrap();
        assert_eq!(
            Attributed::<(i64, u64), A>::decode(&bytes)
                .unwrap()
                .into_value(),
            (lo, hi)
        );
    }
    assert!(
        dag_cbor::decode::<KeyedPairs<u64, u64>>(
            &dag_cbor::encode(&vec![(2u64, 1u64), (1, 2)]).unwrap()
        )
        .is_err()
    );
    // Known tuple cannot authorize lossy application decoding.
    #[derive(Serialize, Deserialize)]
    struct Partial {
        retained: u64,
    }
    let bytes = Attributed::<_, A>::new(BTreeMap::from([
        ("retained".to_owned(), 1u64),
        ("lost".to_owned(), 2),
    ]))
    .encode()
    .unwrap();
    assert!(Attributed::<Partial, A>::decode(&bytes).is_err());
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
                "eidetica/password/eidetica/attributed-opaque"
            } else {
                "eidetica/attributed-opaque"
            }
            .into(),
            version: 1,
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
async fn read_at<C: StoreContract>(
    db: &Database,
    name: &str,
    snapshot: Snapshot,
) -> Result<Attributed<Events, C>> {
    let tx = db.new_transaction_at(&snapshot).await?;
    tx.get_store::<S<C>>(name).await?.read().await
}

#[tokio::test]
async fn test_structured_registry_only_mixed_entry_and_cache() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("empty").await.unwrap();
    let empty = tx.commit().await.unwrap();
    let boundary = Snapshot::from([empty]);
    for _ in 0..2 {
        assert!(
            !read_at::<A>(&db, "empty", boundary.clone())
                .await
                .unwrap()
                .has_source()
        );
        assert!(
            !read_at::<B>(&db, "empty", boundary.clone())
                .await
                .unwrap()
                .has_source()
        );
    }
    let req = request(&db, "empty", &boundary, false).await;
    assert!(
        db.backend()
            .unwrap()
            .resolve_store_state(&req)
            .await
            .unwrap()
            .is_none()
    );
    let tx = db.new_transaction().await.unwrap();
    let a = tx.get_store::<S>("first").await.unwrap();
    let b = tx.get_store::<S<Other>>("second").await.unwrap();
    a.apply(events("one")).await.unwrap();
    b.apply(events("two")).await.unwrap();
    tx.get_store::<DocStore>("legacy")
        .await
        .unwrap()
        .set("k", "legacy")
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let original = db.get_entry(&id).await.unwrap();
    let bytes = original.canonical_bytes().unwrap();
    // Exercise both actual reader directions, not just a synthetic bad header:
    // legacy Doc cannot decode new bytes, and new readers cannot guess the
    // contract of a real untagged legacy contribution. Normal opens also refuse
    // at their registration boundary before either payload decoder runs.
    assert!(Doc::decode(original.data("first").unwrap()).is_err());
    assert!(Attributed::<Events, A>::decode(original.data("legacy").unwrap()).is_err());
    assert!(db.get_store_viewer::<S>("legacy").await.is_err());
    let check = db.new_transaction().await.unwrap();
    assert!(check.get_store::<DocStore>("first").await.is_err());
    assert_eq!(
        inspect_contract(original.data("first").unwrap()).unwrap(),
        ContractDescriptor::from(A::ID)
    );
    assert_eq!(
        inspect_contract(original.data("second").unwrap()).unwrap(),
        ContractDescriptor::from(Other::ID)
    );
    for _ in 0..2 {
        assert_eq!(
            read_at::<A>(&db, "first", Snapshot::from([id.clone()]))
                .await
                .unwrap()
                .into_value(),
            events("one")
        );
        assert_eq!(
            read_at::<Other>(&db, "second", Snapshot::from([id.clone()]))
                .await
                .unwrap()
                .into_value(),
            events("two")
        );
    }
    fixture
        .source
        .backend()
        .clear_derived_store_state()
        .await
        .unwrap();
    assert_eq!(
        read_at::<A>(&db, "first", Snapshot::from([id.clone()]))
            .await
            .unwrap()
            .into_value(),
        events("one")
    );
    assert_eq!(
        db.get_entry(&id).await.unwrap().canonical_bytes().unwrap(),
        bytes
    );
    assert_eq!(db.get_entry(&id).await.unwrap().auth(), original.auth());
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
    assert!(db.get_store_viewer::<S<Other>>("first").await.is_err());
    let tx = db.new_transaction().await.unwrap();
    tx.get_index()
        .await
        .unwrap()
        .set_entry("first", Other::ID.store, Doc::new())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(db.get_store_viewer::<S>("first").await.is_err());
    assert_eq!(
        read_at::<A>(&db, "first", Snapshot::from([id]))
            .await
            .unwrap()
            .into_value(),
        events("one")
    );
}

#[tokio::test]
async fn test_structured_historical_concurrent_and_prefix_refusal() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject").await.unwrap();
    let base = tx.commit().await.unwrap();
    let txa = db
        .new_transaction_at(&Snapshot::from([base.clone()]))
        .await
        .unwrap();
    txa.get_store::<S>("subject")
        .await
        .unwrap()
        .apply(events("a"))
        .await
        .unwrap();
    let a = txa.commit().await.unwrap();
    assert_eq!(
        read_at::<A>(&db, "subject", Snapshot::from([a.clone()]))
            .await
            .unwrap()
            .into_value(),
        events("a")
    );
    let txb = db
        .new_transaction_at(&Snapshot::from([base]))
        .await
        .unwrap();
    txb.get_store::<S<B>>("subject")
        .await
        .unwrap()
        .apply(events("b"))
        .await
        .unwrap();
    let b = txb.commit().await.unwrap();
    assert_eq!(
        read_at::<B>(&db, "subject", Snapshot::from([b.clone()]))
            .await
            .unwrap()
            .into_value(),
        events("b")
    );
    let joined = Snapshot::from([a.clone(), b.clone()]);
    assert!(read_at::<A>(&db, "subject", joined.clone()).await.is_err());
    assert!(read_at::<B>(&db, "subject", joined.clone()).await.is_err());
    let tx = db.new_transaction_at(&joined).await.unwrap();
    let s = tx.get_store::<S>("subject").await.unwrap();
    assert!(s.apply(events("unsafe")).await.is_err());
    assert!(s.local_data().unwrap().is_none());
    // A stale writer may extend its *captured* valid A branch, not latest tips.
    let stale = db
        .new_transaction_at(&Snapshot::from([a.clone()]))
        .await
        .unwrap();
    stale
        .get_store::<S>("subject")
        .await
        .unwrap()
        .apply(events("late"))
        .await
        .unwrap();
    let late = stale.commit().await.unwrap();
    assert_eq!(
        read_at::<A>(&db, "subject", Snapshot::from([late.clone()]))
            .await
            .unwrap()
            .into_value(),
        Events(vec!["a".into(), "late".into()])
    );
    assert_eq!(
        read_at::<A>(&db, "subject", Snapshot::from([a.clone()]))
            .await
            .unwrap()
            .into_value(),
        events("a")
    );
    let sibling = db
        .new_transaction_at(&Snapshot::from([a.clone()]))
        .await
        .unwrap();
    sibling
        .get_store::<S>("subject")
        .await
        .unwrap()
        .apply(events("sibling"))
        .await
        .unwrap();
    let sibling = sibling.commit().await.unwrap();
    let compatible = Snapshot::from([late.clone(), sibling]);
    for _ in 0..2 {
        let value = read_at::<A>(&db, "subject", compatible.clone())
            .await
            .unwrap()
            .into_value();
        assert_eq!(value.0.len(), 3);
        assert_eq!(value.0[0], "a");
        assert!(value.0.contains(&"late".into()) && value.0.contains(&"sibling".into()));
    }
    // Incompatible suffix sharing an already cached A merge base.
    let parent = db.get_entry(&a).await.unwrap();
    let raw = signed_contribution(
        &fixture,
        &db,
        &parent,
        Attributed::<_, B>::new(events("suffix-b"))
            .encode()
            .unwrap(),
    );
    let raw_id = db.insert_raw(raw).await.unwrap();
    assert!(
        read_at::<A>(&db, "subject", Snapshot::from([late, raw_id]))
            .await
            .is_err()
    );
    // Raw transport can retain unsupported data. Explicit diagnostic snapshots
    // do not promote its verification status or constitute normal frontier reads.
    assert_eq!(
        read_at::<B>(&db, "subject", Snapshot::from([b]))
            .await
            .unwrap()
            .into_value(),
        events("b")
    );
}

#[tokio::test]
async fn test_structured_wrong_cache_and_untagged_history() {
    let (fixture, db) = setup_tree().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject")
        .await
        .unwrap()
        .apply(events("a"))
        .await
        .unwrap();
    let a = tx.commit().await.unwrap();
    let boundary = Snapshot::from([a.clone()]);
    fixture
        .source
        .backend()
        .clear_derived_store_state()
        .await
        .unwrap();
    let req = request(&db, "subject", &boundary, false).await;
    seed(
        &db,
        req.clone(),
        Attributed::<_, B>::new(events("poison")).encode().unwrap(),
    )
    .await;
    assert!(
        read_at::<A>(&db, "subject", boundary.clone())
            .await
            .is_err()
    );
    assert_eq!(
        read_at::<B>(&db, "subject", boundary.clone())
            .await
            .unwrap()
            .into_value(),
        events("poison"),
        "cache header is identity, not proof of client computation"
    );
    fixture
        .source
        .backend()
        .clear_derived_store_state()
        .await
        .unwrap();
    assert_eq!(
        read_at::<A>(&db, "subject", boundary)
            .await
            .unwrap()
            .into_value(),
        events("a")
    );
    for bad in [
        vec![],
        b"{}".to_vec(),
        vec![0xa0],
        Attributed::<_, B>::new(events("b")).encode().unwrap(),
    ] {
        let parent = db.get_entry(&a).await.unwrap();
        let raw = signed_contribution(&fixture, &db, &parent, bad);
        let raw_bytes = raw.canonical_bytes().unwrap();
        let id = db.insert_raw(raw).await.unwrap();
        assert!(db.verify_entry_signature(&id).await.unwrap());
        assert!(
            read_at::<A>(&db, "subject", Snapshot::from([id.clone()]))
                .await
                .is_err()
        );
        assert_eq!(
            db.get_entry(&id).await.unwrap().canonical_bytes().unwrap(),
            raw_bytes
        );
    }
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
type F<C = A> = StructuredStore<Fallible, C>;

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
    assert!(tx.get_store::<F<B>>("subject").await.is_err());
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
        db.get_entry(&id).await.unwrap().data("subject").unwrap(),
        &reopened.read().await.unwrap().encode().unwrap()
    );
    // Binding is enforced even before the first contribution exists.
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("new").await.unwrap();
    assert!(S::<B>::load(&tx, "new".into()).await.is_err());
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
    assert!(inspect_contract(original.data("secret").unwrap()).is_err());
    assert!(db.get_store_viewer::<F>("secret").await.is_err());
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
        let wrong = tx.get_store::<PasswordStore<F<B>>>("secret").await.unwrap();
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
        // Existing service cache publication requires Write; do not turn its
        // explicit permission refusal into a successful empty/read fallback.
        assert!(
            second_db
                .get_store_viewer::<PasswordStore<F>>("secret")
                .await
                .is_err()
        );
        let tx = db.new_transaction().await.unwrap();
        tx.get_settings()
            .unwrap()
            .set_auth_key(&public, AuthKey::active(None, Permission::Write(0)))
            .await
            .unwrap();
        tx.commit().await.unwrap();
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
    let mut wrong = tx.get_store::<PasswordStore<F<B>>>("secret").await.unwrap();
    wrong.open("test-password").unwrap();
    let error = wrong.inner().await.unwrap().read().await.err().unwrap();
    assert!(!error.to_string().contains(B::ID.schema));
    assert!(!format!("{error:?}").contains(A::ID.schema));
}
