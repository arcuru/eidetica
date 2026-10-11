//! Persistence compatibility and independence of the encoding/merge contracts.

use eidetica::{
    Error, Result,
    constants::SETTINGS,
    crdt::{
        CRDT, Codec, Doc,
        doc::{List, Value, list::Position},
    },
};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::helpers::TestContext;

fn assert_fixture<T>(value: &T, bytes: &[u8])
where
    T: Codec + Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    // The pre-Codec persistence boundary used these exact Serde operations.
    assert_eq!(serde_json::to_vec(value).unwrap(), bytes);
    assert_eq!(value.encode().unwrap(), bytes);
    assert_eq!(&T::decode(bytes).unwrap(), value);
    assert_eq!(&serde_json::from_slice::<T>(bytes).unwrap(), value);
}

#[test]
fn test_codec_doc_legacy_bytes_and_merge_state() {
    assert_fixture(&Doc::new(), br#"{"children":{}}"#);
    let mut atomic = Doc::atomic();
    atomic.set("gone", Value::Deleted);
    let bytes = br#"{"_a":true,"children":{"gone":"Deleted"}}"#;
    assert_fixture(&atomic, bytes);
    let mut previous = Doc::new();
    previous.set("old", "value");
    assert_eq!(
        previous.merge(&Doc::decode(bytes).unwrap()).unwrap(),
        atomic
    );

    // Legacy v0 documents without a version remain readable.
    let explicit_v0 = br#"{"_v":0,"children":{}}"#;
    assert_eq!(Doc::decode(explicit_v0).unwrap(), Doc::new());
}

#[test]
fn test_codec_value_and_list_legacy_bytes() {
    for (value, bytes) in [
        (Value::Null, br#""Null""#.as_slice()),
        (Value::Bool(true), br#"{"Bool":true}"#),
        (Value::Int(-7), br#"{"Int":-7}"#),
        (Value::Text("a\n\"b".into()), br#"{"Text":"a\n\"b"}"#),
        (Value::Deleted, br#""Deleted""#),
        (Value::Doc(Doc::new()), br#"{"Doc":{"children":{}}}"#),
        (Value::List(List::new()), br#"{"List":[]}"#),
    ] {
        assert_fixture(&value, bytes);
    }
    let mut list = List::new();
    list.insert_at_position(
        Position {
            numerator: 3,
            denominator: 2,
            unique_id: Uuid::nil(),
        },
        Value::Deleted,
    );
    let bytes = br#"[[{"numerator":3,"denominator":2,"unique_id":"00000000-0000-0000-0000-000000000000"},"Deleted"]]"#;
    assert_fixture(&list, bytes);
    let decoded = List::decode(bytes).unwrap();
    assert_eq!(decoded.iter_with_positions().next().unwrap().0.numerator, 3);
    assert_eq!(
        decoded.iter_with_positions().next().unwrap().1,
        &Value::Deleted
    );
}

fn assert_json_error_unchanged<T: Codec + DeserializeOwned>(bytes: &[u8]) {
    let legacy = serde_json::from_slice::<T>(bytes).err().unwrap();
    match T::decode(bytes).err().unwrap() {
        Error::Serialize(error) => assert_eq!(error.to_string(), legacy.to_string()),
        error => panic!("changed error variant: {error}"),
    }
}

#[test]
fn test_codec_json_decode_errors_unchanged() {
    for bytes in [
        b"".as_slice(),
        b" ",
        b"not JSON",
        b"{\"children\":{}} trailing",
        b"{\"_v\":1,\"children\":{}}",
    ] {
        assert_json_error_unchanged::<Doc>(bytes);
    }
    assert_json_error_unchanged::<Value>(b"\"Deleted\" false");
    assert_json_error_unchanged::<List>(b"[] []");
    assert_eq!(Doc::decode(b" {\"children\":{}} \n").unwrap(), Doc::new());
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_codec_yrs_legacy_json_wrapper() {
    use eidetica::store::YrsBinary;
    for (update, bytes) in [
        (vec![], br#"{"data":[]}"#.as_slice()),
        // A valid empty Yjs v1 update; persistence retains its JSON wrapper.
        (vec![0, 0], br#"{"data":[0,0]}"#),
    ] {
        let binary = YrsBinary::new(update.clone());
        assert_eq!(serde_json::to_vec(&binary).unwrap(), bytes);
        assert_eq!(binary.encode().unwrap(), bytes);
        assert_eq!(YrsBinary::decode(bytes).unwrap().as_bytes(), update);
        assert_eq!(
            serde_json::from_slice::<YrsBinary>(bytes)
                .unwrap()
                .as_bytes(),
            update
        );
    }
    assert_json_error_unchanged::<YrsBinary>(b"{\"data\":[256]}");
    assert_json_error_unchanged::<YrsBinary>(b"{\"data\":[]} []");
}

struct MergeOnly(u8);
impl CRDT for MergeOnly {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.max(other.0)))
    }
}

#[test]
fn test_codec_merge_only_without_encoding_clone_or_default() {
    assert_eq!(MergeOnly(3).merge(&MergeOnly(7)).unwrap().0, 7);
}

#[tokio::test]
async fn test_codec_settings_persisted_bytes() {
    let ctx = TestContext::new().with_database().await;
    let tx = ctx.database().new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_name("codec-fixture")
        .await
        .unwrap();
    let bytes = br#"{"children":{"name":{"Text":"codec-fixture"}}}"#;
    let staged = tx.get_local_data::<Doc>(SETTINGS).unwrap().unwrap();
    assert_fixture(&staged, bytes);
    let id = tx.commit().await.unwrap();
    let entry = ctx.database().get_entry(id).await.unwrap();
    assert_eq!(entry.data(SETTINGS).unwrap(), bytes);
    let reader = ctx.database().new_transaction().await.unwrap();
    assert_eq!(
        reader.get_settings().unwrap().get_name().await.unwrap(),
        "codec-fixture"
    );
}
