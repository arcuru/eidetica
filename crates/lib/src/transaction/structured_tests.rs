//! Private-boundary controls: verification status, staged raw bytes and encrypted
//! application failures cannot be reached through a public raw writer.
use super::*;
use crate::{
    Instance, NewAccount,
    backend::{CacheScope, StoreStateRequest, database::InMemory},
    crdt::{Lww, dag_cbor},
    store::{
        PasswordStore, StoreError, StructuredStore,
        schema::{StructuredState, StructuredType},
        state,
    },
};
use std::sync::atomic::AtomicUsize;

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

structured_type!(A, "private:v0.1", Lww<u64>);
structured_type!(B, "private:v0.2", Lww<u64>);
type S<T = A> = StructuredStore<T>;

async fn database() -> (Instance, Database) {
    let (instance, _) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let (key, _) = crate::auth::crypto::generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    (instance, db)
}

fn request<T: StructuredType>(
    db: &Database,
    tx: &Transaction,
    store: &str,
    boundary: &Snapshot,
) -> StoreStateRequest {
    state::opaque_request(
        db.root_id(),
        store,
        tx.encrypted_projection_descriptor(store, S::<T>::state_model().descriptor()),
        boundary.cache_key_bytes(),
        CacheScope::Shared,
    )
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
async fn structured_present_empty_staging_is_not_absence() {
    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    let s = tx.get_store::<S>("subject").await.unwrap();
    tx.update_subtree("subject", Vec::new()).await.unwrap();
    assert!(s.read().await.is_err());
    assert!(s.local_data().is_err());
    assert!(s.apply(Lww::Set(1)).await.is_err());
    let before = db.snapshot().await.unwrap();
    assert!(
        tx.commit().await.is_err(),
        "the Type check must run before empty-Store cleanup"
    );
    assert_eq!(db.snapshot().await.unwrap(), before);
}

#[tokio::test]
async fn structured_replaced_staging_loses_its_encoder() {
    let (_instance, db) = database().await;
    for bytes in [dag_cbor::encode(&Lww::Set(2u64)).unwrap(), Vec::new()] {
        let tx = db.new_transaction().await.unwrap();
        let s = tx.get_store::<S>("subject").await.unwrap();
        s.apply(Lww::Set(1)).await.unwrap();
        tx.update_subtree("subject", bytes).await.unwrap();
        assert!(s.local_data().is_err());
        assert!(tx.commit().await.is_err());
    }
}

#[tokio::test]
async fn structured_clones_cannot_change_a_transaction_that_is_committing() {
    let (_instance, db) = database().await;
    let mut tx = db.new_transaction().await.unwrap();
    let s = tx.get_store::<S>("subject").await.unwrap();
    s.apply(Lww::Set(1)).await.unwrap();
    assert!(tx.freeze_structured().unwrap());
    assert!(s.apply(Lww::Set(2)).await.is_err());
    assert!(
        S::<B>::declare(s.transaction(), "subject", Doc::new())
            .await
            .is_err()
    );
    assert!(s.set_config(Doc::new()).await.is_err());
}

#[tokio::test]
async fn structured_warm_cache_cannot_replace_verified_history() {
    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject").await.unwrap();
    let declaration = tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<S>("subject")
        .await
        .unwrap()
        .apply(Lww::Set(7))
        .await
        .unwrap();
    let contribution = tx.commit().await.unwrap();
    let boundary = Snapshot::from([contribution.clone()]);
    let captured = db.new_transaction_at(&boundary).await.unwrap();
    let store = captured.get_store::<S>("subject").await.unwrap();
    assert_eq!(store.read().await.unwrap().into_value(), Lww::Set(7));
    let req = request::<A>(&db, &captured, "subject", &boundary);
    assert!(state::load_cached(db.ops(), &req).await.unwrap().is_some());
    db.ops()
        .local_engine()
        .unwrap()
        .update_verification_status(&declaration, VerificationStatus::Unverified)
        .await
        .unwrap();
    assert!(
        state::load_cached(db.ops(), &req).await.unwrap().is_some(),
        "the optional cache still exists"
    );
    let unavailable = |error: Error| {
        assert!(
            matches!(&error, Error::Store(e) if matches!(**e, StoreError::HistoryUnavailable { .. })),
            "{error}"
        );
    };
    unavailable(store.read().await.err().unwrap());
    // Incomplete proof is not reported as a Store that never had a Type.
    unavailable(
        captured
            .inspect_contribution("subject", &contribution)
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn structured_historical_gate_precedes_application_decode_and_staging() {
    static DECODE: AtomicUsize = AtomicUsize::new(0);
    #[derive(Clone, Default, Serialize)]
    #[serde(transparent)]
    struct Observed(Lww<u64>);
    impl<'de> Deserialize<'de> for Observed {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            DECODE.fetch_add(1, Ordering::SeqCst);
            Lww::deserialize(d).map(Self)
        }
    }
    impl CRDT for Observed {
        fn merge(&self, other: &Self) -> Result<Self> {
            self.0.merge(&other.0).map(Self)
        }
    }
    structured_type!(WatchA, "private:v0.1", Observed);
    structured_type!(WatchB, "private:v0.2", Observed);

    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    let b = tx.get_store::<S<B>>("subject").await.unwrap();
    b.apply(Lww::Set(7)).await.unwrap();
    let committed = tx.commit().await.unwrap();
    for warm in [false, true] {
        let tx = db.new_transaction().await.unwrap();
        let wrong = S::<WatchA>::load(&tx, "subject".into()).await.unwrap();
        assert!(wrong.read().await.is_err(), "warm {warm}");
        assert!(wrong.apply(Observed(Lww::Set(8))).await.is_err());
        assert!(wrong.local_data().unwrap().is_none());
        assert_eq!(
            tx.inspect_contribution("subject", &committed)
                .await
                .unwrap()
                .unwrap()
                .type_id,
            B::TYPE_ID
        );
        assert_eq!(DECODE.swap(0, Ordering::SeqCst), 0);
        let right = tx.get_store::<S<WatchB>>("subject").await.unwrap();
        assert_eq!(right.read().await.unwrap().into_value().0, Lww::Set(7));
        assert!(
            DECODE.swap(0, Ordering::SeqCst) > 0,
            "the matching Type does decode"
        );
    }
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
structured_type!(Bad, "private:v0.1", Malicious);
structured_type!(Good, "private:v0.1", Plain);

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
struct Plain(u64);
impl CRDT for Plain {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(other.clone())
    }
}

#[tokio::test]
async fn structured_encrypted_replay_and_cache_errors_are_redacted() {
    let (_instance, db) = database().await;
    let tx = db.new_transaction().await.unwrap();
    let mut wrapped = tx
        .get_store::<PasswordStore<S<Good>>>("secret")
        .await
        .unwrap();
    wrapped.initialize("pass", Doc::new()).await.unwrap();
    let inner = wrapped.inner().await.unwrap();
    inner.apply(Plain(1)).await.unwrap();
    let id = tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut wrapped = tx
        .get_store::<PasswordStore<S<Bad>>>("secret")
        .await
        .unwrap();
    wrapped.open("pass").unwrap();
    let s = wrapped.inner().await.unwrap();
    let check = |error: Error| {
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains("private"), "{text}");
        }
    };
    check(s.read().await.err().unwrap()); // cold replay
    // A direct load of another Type on the unlocked Store is gated and redacted.
    let other = S::<B>::load(&tx, "secret".into()).await.unwrap();
    check(other.read().await.err().unwrap());
    let req = request::<Bad>(&db, &tx, "secret", &Snapshot::from([id]));
    assert_eq!(
        req.projection.name,
        "eidetica/password/eidetica/structured-opaque"
    );
    let encrypted = tx
        .encrypt_if_needed(
            "secret",
            &StructuredState::<Good>::new(Plain(1))
                .encode_cached()
                .unwrap(),
        )
        .unwrap();
    seed(&db, req, encrypted).await;
    check(s.read().await.err().unwrap()); // exact cache
}
