//! Private-boundary controls: publication adoption, present-empty staging and
//! encrypted application failures cannot be reached through a public raw writer.
use super::*;
use crate::{
    Instance, NewAccount,
    backend::{CacheScope, StoreStateRequest, database::InMemory},
    store::{
        PasswordStore, StructuredStore,
        schema::{Attributed, ContractId, StoreContract},
    },
};

struct A;
struct B;
impl StoreContract for A {
    const ID: ContractId = ContractId {
        store: "structured:v0.1",
        data: "eidetica/lww:v0.1",
        schema: "private/schema:v0.1",
    };
}
impl StoreContract for B {
    const ID: ContractId = ContractId {
        schema: "private/schema:v0.2",
        ..A::ID
    };
}
type Data<C = A> = Attributed<crate::crdt::Lww<u64>, C>;
type S<C = A> = StructuredStore<crate::crdt::Lww<u64>, C>;

async fn database() -> (Instance, Database) {
    let (instance, _) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let (key, _) = crate::auth::crypto::generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    (instance, db)
}
async fn seed(db: &Database, request: StoreStateRequest, bytes: Vec<u8>) {
    let token = db.ops().begin_store_state_staging(request).await.unwrap();
    db.ops()
        .stage_store_state_records(
            &token,
            BTreeMap::from([(state::OPAQUE_STATE_KEY.to_vec(), Some(bytes))]),
        )
        .await
        .unwrap();
    db.ops().publish_store_state(token).await.unwrap();
}

#[tokio::test]
async fn structured_publication_adopted_winner_is_gated() {
    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    let req = state::opaque_request(
        db.root_id(),
        "subject",
        S::<A>::state_model().descriptor(),
        Snapshot::from([db.root_id().clone()]).cache_key_bytes(),
        CacheScope::Shared,
    );
    // Simulate another publisher winning after the consumer's resolve miss.
    seed(
        &db,
        req.clone(),
        Data::<B>::new(crate::crdt::Lww::Set(9)).encode().unwrap(),
    )
    .await;
    let value = Data::<A>::new(crate::crdt::Lww::Set(1));
    assert!(
        tx.cache_full_state("subject", req.clone(), value, Some(Data::has_source))
            .await
            .is_err()
    );
    let winner = state::load_cached(db.ops(), &req).await.unwrap().unwrap();
    assert!(
        Data::<B>::decode(&winner).is_ok(),
        "failed adoption never overwrites the winner"
    );
    db.ops().clear_derived_store_state().await.unwrap();
    seed(
        &db,
        req.clone(),
        Data::<A>::new(crate::crdt::Lww::Set(9)).encode().unwrap(),
    )
    .await;
    assert!(
        tx.cache_full_state(
            "subject",
            req,
            Data::<A>::new(crate::crdt::Lww::Set(1)),
            Some(Data::has_source)
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn structured_present_empty_staging_is_not_absence() {
    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    let s = tx.get_store::<S>("subject").await.unwrap();
    tx.update_subtree("subject", Vec::new()).await.unwrap();
    assert!(s.read().await.is_err());
    assert!(s.local_data().is_err());
    assert!(s.apply(crate::crdt::Lww::Set(1)).await.is_err());
    assert!(
        tx.entry_builder
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .data("subject")
            .unwrap()
            .is_empty()
    );
}

#[derive(Clone, Default, Serialize)]
#[serde(transparent)]
struct Malicious(u64);
impl<'de> Deserialize<'de> for Malicious {
    fn deserialize<D: serde::Deserializer<'de>>(_: D) -> std::result::Result<Self, D::Error> {
        Err(serde::de::Error::custom("private-application-error"))
    }
}
impl CRDT for Malicious {
    fn merge(&self, _: &Self) -> Result<Self> {
        panic!("malformed data must not reach merge")
    }
}
type Bad = StructuredStore<Malicious, A>;

#[tokio::test]
async fn structured_encrypted_replay_cache_and_winner_errors_are_redacted() {
    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    let mut wrapped = tx.get_store::<PasswordStore<Bad>>("secret").await.unwrap();
    wrapped.initialize("pass", Doc::new()).await.unwrap();
    // The grammar is valid, but the declared application refuses to decode it.
    let plaintext = Attributed::<u64, A>::new(1).encode().unwrap();
    tx.update_subtree("secret", plaintext.clone())
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut wrapped = tx.get_store::<PasswordStore<Bad>>("secret").await.unwrap();
    wrapped.open("pass").unwrap();
    let s = wrapped.inner().await.unwrap();
    // Unlocking one static wrapped Store family must not let direct load bypass
    // its type gate on an empty or malicious source.
    struct Wrong;
    impl StoreContract for Wrong {
        const ID: ContractId = ContractId {
            store: "other:v0.1",
            ..A::ID
        };
    }
    assert!(
        StructuredStore::<Malicious, Wrong>::load(&tx, "secret".into())
            .await
            .is_err()
    );
    let req = state::opaque_request(
        db.root_id(),
        "secret",
        tx.encrypted_projection_descriptor("secret", Bad::state_model().descriptor()),
        Snapshot::from([id]).cache_key_bytes(),
        CacheScope::Shared,
    );
    assert_eq!(
        req.projection.name,
        "eidetica/password/eidetica/attributed-opaque"
    );
    let check = |error: Error| {
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains("private-application-error"));
            assert!(!text.contains(A::ID.schema));
        }
    };
    check(s.read().await.err().unwrap()); // cold replay
    let encrypted = tx.encrypt_if_needed("secret", &plaintext).unwrap();
    seed(&db, req.clone(), encrypted).await;
    check(s.read().await.err().unwrap()); // exact cache
    let value = Attributed::<Malicious, A>::new(Malicious(1));
    let result = tx
        .cache_full_state("secret", req, value, Some(Attributed::has_source))
        .await;
    check(tx.structured_error("secret", "read", result.err().unwrap())); // adopted winner
}
