//! Table facade wiring: row types and codecs never participate in state projection.

use std::rc::Rc;

use eidetica::{
    Database, Instance, NewUser, Registered, Store,
    crdt::{Codec, Doc, Lww},
    store::{PasswordStore, RawBytes, RawTable, RowCodec, SerdeJson, Table, TableData},
};
use serde::{Deserialize, Serialize};

use crate::helpers::{setup_tree, test_backend};

async fn database() -> (Instance, Database) {
    // test_backend() alone falls back to InMemory in service mode; use the
    // connected instance helper so these tests actually exercise daemon RPC.
    if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
        return setup_tree().await;
    }
    let (instance, mut user) =
        Instance::create_backend(test_backend().await, NewUser::passwordless("table-codecs"))
            .await
            .unwrap();
    let key = user.get_default_key().unwrap();
    let database = user.create_database(Doc::new(), &key).await.unwrap();
    (instance, database)
}

// Deliberately no Clone. Default Table must not narrow JSON integer ranges.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IntegerRow {
    signed64: i64,
    unsigned64: u64,
    signed128: i128,
    unsigned128: u128,
}

fn integer_rows() -> [IntegerRow; 2] {
    [
        IntegerRow {
            signed64: i64::MIN,
            unsigned64: 0,
            signed128: i128::MIN,
            unsigned128: 0,
        },
        IntegerRow {
            signed64: i64::MAX,
            unsigned64: u64::MAX,
            signed128: i128::MAX,
            unsigned128: u128::MAX,
        },
    ]
}

#[tokio::test]
async fn test_table_full_integer_ranges_staged_warm_and_cold() {
    let (instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let table = tx.get_store::<Table<IntegerRow>>("integers").await.unwrap();
    assert_eq!(Table::<IntegerRow>::type_id(), "table:v1");
    assert_eq!(Table::<IntegerRow>::state_model().descriptor().version, 1);
    for (index, row) in integer_rows().into_iter().enumerate() {
        let key = index.to_string();
        let bytes = SerdeJson::encode(&row).unwrap();
        table.clone().set(&key, row).await.unwrap();
        assert_eq!(table.get(&key).await.unwrap(), integer_rows()[index]);
        assert_eq!(
            table
                .local_data()
                .unwrap()
                .unwrap()
                .0
                .get(&key)
                .unwrap()
                .as_ref(),
            bytes
        );
    }
    let id = tx.commit().await.unwrap();
    let entry = database.backend().unwrap().get(&id).await.unwrap();
    let delta = TableData::decode(entry.data("integers").unwrap()).unwrap();
    for (index, row) in integer_rows().into_iter().enumerate() {
        assert_eq!(
            delta.0.get(&index.to_string()).unwrap().as_ref(),
            SerdeJson::encode(&row).unwrap()
        );
    }
    for cold in [false, true, false] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        let table = database
            .get_store_viewer::<Table<IntegerRow>>("integers")
            .await
            .unwrap();
        let first = table.scan_page(None, 1).await.unwrap();
        assert_eq!(
            first.rows,
            vec![("0".into(), integer_rows().into_iter().next().unwrap())]
        );
        let second = table.scan_page(first.next.as_ref(), 1).await.unwrap();
        assert_eq!(
            second.rows,
            vec![("1".into(), integer_rows().into_iter().nth(1).unwrap())]
        );
        assert!(second.next.is_none());
        assert_eq!(
            table
                .search(|row| row.unsigned128 == u128::MAX)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn test_raw_table_inspector_uses_unknown_codec_and_bounded_pages() {
    let (instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<LocalRow, LocalCodec>>("inspect")
        .await
        .unwrap();
    assert_eq!(
        table
            .get_config()
            .await
            .unwrap()
            .get("row_codec")
            .unwrap()
            .as_text(),
        Some(LocalCodec::FORMAT_ID)
    );
    let payloads = [vec![0xff, 0, 0x80], vec![], b" { \"n\" : 1.00 } ".to_vec()];
    for (i, bytes) in payloads.iter().enumerate() {
        table
            .set(i.to_string(), LocalRow(Rc::new(bytes.clone())))
            .await
            .unwrap();
    }
    let raw = tx.get_store::<RawTable>("inspect").await.unwrap();
    assert_eq!(raw.row_codec_id().await.unwrap(), LocalCodec::FORMAT_ID);
    assert_eq!(raw.get("0").await.unwrap(), payloads[0]);
    let first = raw.scan_page(None, 1).await.unwrap();
    assert_eq!(first.rows, vec![("0".into(), payloads[0].clone())]);
    table.set("1", LocalRow(Rc::new(vec![]))).await.unwrap();
    assert!(
        raw.scan_page(first.next.as_ref(), 1).await.is_err(),
        "overlay changes invalidate raw cursors too"
    );
    tx.commit().await.unwrap();
    for cold in [false, true] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        let raw = database
            .get_store_viewer::<RawTable>("inspect")
            .await
            .unwrap();
        let mut cursor = None;
        for (i, bytes) in payloads.iter().enumerate() {
            assert_eq!(raw.get(i.to_string()).await.unwrap(), *bytes);
            let page = raw.scan_page(cursor.as_ref(), 1).await.unwrap();
            assert_eq!(page.rows, vec![(i.to_string(), bytes.clone())]);
            cursor = page.next;
        }
        assert!(cursor.is_none());
    }
    let tx = database.new_transaction().await.unwrap();
    assert!(tx.get_store::<RawTable>("absent").await.is_err());
    assert!(
        tx.get_index()
            .await
            .unwrap()
            .get_entry("absent")
            .await
            .is_err(),
        "inspection must not register missing Stores"
    );
}

struct WrongCodec;
impl RowCodec<Vec<u8>> for WrongCodec {
    const FORMAT_ID: &'static str = "tests/wrong:v1";
    fn encode(_: &Vec<u8>) -> eidetica::Result<Vec<u8>> {
        panic!("mismatched encoder invoked")
    }
    fn decode(_: &[u8]) -> eidetica::Result<Vec<u8>> {
        panic!("mismatched decoder invoked")
    }
}

#[tokio::test]
async fn test_table_config_rejects_mismatched_and_stale_typed_handles() {
    let (_instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<Vec<u8>, RawBytes>>("identity")
        .await
        .unwrap();
    table.set("row", vec![0xff]).await.unwrap();
    assert!(
        tx.get_store::<Table<Vec<u8>, WrongCodec>>("identity")
            .await
            .is_err()
    );
    let mut changed = Doc::new();
    changed.set("row_codec", WrongCodec::FORMAT_ID);
    tx.get_index()
        .await
        .unwrap()
        .set_entry("identity", "table:v1", changed)
        .await
        .unwrap();
    assert!(table.get("row").await.is_err());
    assert!(table.set("row", vec![0]).await.is_err());
    assert!(table.scan_page(None, 1).await.is_err());
    assert!(table.delete("row").await.is_err());
}

#[tokio::test]
async fn test_table_absent_view_is_empty_but_missing_format_on_data_is_rejected() {
    let (_instance, database) = database().await;
    let empty = database
        .get_store_viewer::<Table<Vec<u8>, WrongCodec>>("absent")
        .await
        .unwrap();
    assert!(empty.get("row").await.is_err());
    assert!(empty.scan_page(None, 1).await.unwrap().rows.is_empty());
    assert!(
        empty
            .search(|_| panic!("absent row predicate invoked"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(empty.set("row", vec![]).await.is_err());

    let tx = database.new_transaction().await.unwrap();
    tx.get_index()
        .await
        .unwrap()
        .set_entry("malformed", "table:v1", Doc::new())
        .await
        .unwrap();
    assert!(
        tx.get_store::<Table<serde_json::Value>>("malformed")
            .await
            .is_err()
    );
    assert!(tx.get_store::<RawTable>("malformed").await.is_err());

    let table = tx
        .get_store::<Table<Vec<u8>, RawBytes>>("unbound")
        .await
        .unwrap();
    table.set("row", vec![0xff]).await.unwrap();
    let index = tx
        .get_store::<eidetica::store::DocStore>("_index")
        .await
        .unwrap();
    index.delete("unbound").await.unwrap();
    assert!(table.get("row").await.is_err());
    assert!(
        Table::<Vec<u8>, WrongCodec>::load(&tx, "unbound".into())
            .await
            .is_err()
    );
    assert_eq!(
        table
            .local_data()
            .unwrap()
            .unwrap()
            .0
            .get(&"row".into())
            .unwrap()
            .as_ref(),
        &[0xff]
    );
}

#[tokio::test]
async fn test_raw_table_inspector_unlocks_without_application_codec() {
    let (_instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<Table<LocalRow, LocalCodec>>>("inspect-secrets")
        .await
        .unwrap();
    encrypted
        .initialize("inspector-key", Doc::new())
        .await
        .unwrap();
    encrypted
        .inner()
        .await
        .unwrap()
        .set("row", LocalRow(Rc::new(vec![0xff, 0, 0x80])))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = database.new_transaction().await.unwrap();
    let mut raw = tx
        .get_store::<PasswordStore<RawTable>>("inspect-secrets")
        .await
        .unwrap();
    assert!(raw.inner().await.is_err());
    assert!(RawTable::load(&tx, "inspect-secrets".into()).await.is_err());
    assert!(raw.open("wrong-key").is_err());
    raw.open("inspector-key").unwrap();
    let inner = raw.inner().await.unwrap();
    assert_eq!(inner.row_codec_id().await.unwrap(), LocalCodec::FORMAT_ID);
    assert_eq!(inner.get("row").await.unwrap(), vec![0xff, 0, 0x80]);
    assert_eq!(
        inner.scan_page(None, 1).await.unwrap().rows,
        vec![("row".into(), vec![0xff, 0, 0x80])]
    );
}

// No Serde, Clone, Send or Sync on the row; no Clone on the stateless codec.
#[derive(Debug, PartialEq, Eq)]
struct LocalRow(Rc<Vec<u8>>);
struct LocalCodec;
impl RowCodec<LocalRow> for LocalCodec {
    const FORMAT_ID: &'static str = "tests/local-bytes:v1";

    fn encode(row: &LocalRow) -> eidetica::Result<Vec<u8>> {
        Ok(row.0.as_ref().clone())
    }

    fn decode(bytes: &[u8]) -> eidetica::Result<LocalRow> {
        Ok(LocalRow(Rc::new(bytes.to_vec())))
    }
}

#[tokio::test]
async fn test_table_custom_non_serde_non_clone_rows_project_exact_bytes() {
    let (instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<LocalRow, LocalCodec>>("custom")
        .await
        .unwrap();
    let payloads = [
        vec![0xff, 0, 0x80],
        b" { \"n\" : 1.00 } \n".to_vec(),
        vec![],
    ];
    for (index, bytes) in payloads.iter().enumerate() {
        let row = LocalRow(Rc::new(bytes.clone()));
        table.clone().set(index.to_string(), row).await.unwrap();
        assert_eq!(
            table.get(index.to_string()).await.unwrap().0.as_ref(),
            bytes
        );
    }
    let id = tx.commit().await.unwrap();
    let entry = database.backend().unwrap().get(&id).await.unwrap();
    let delta = TableData::decode(entry.data("custom").unwrap()).unwrap();
    for (index, bytes) in payloads.iter().enumerate() {
        assert_eq!(delta.0.get(&index.to_string()).unwrap().as_ref(), bytes);
    }
    for cold in [false, true, false] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        // The default daemon registration handles this non-JSON format without
        // registration of LocalCodec or executing any application row decoder.
        let state = database
            .get_store_state::<Table<LocalRow, LocalCodec>>("custom")
            .await
            .unwrap();
        for (index, bytes) in payloads.iter().enumerate() {
            assert_eq!(state.0.get(&index.to_string()).unwrap().as_ref(), bytes);
        }
        let table = database
            .get_store_viewer::<Table<LocalRow, LocalCodec>>("custom")
            .await
            .unwrap();
        for (index, bytes) in payloads.iter().enumerate() {
            assert_eq!(
                table
                    .clone()
                    .get(index.to_string())
                    .await
                    .unwrap()
                    .0
                    .as_ref(),
                bytes
            );
        }
        let page = table.scan_page(None, 3).await.unwrap();
        assert_eq!(page.rows.len(), 3);
        for ((key, row), bytes) in page.rows.iter().zip(&payloads) {
            assert_eq!(row.0.as_ref(), bytes, "key {key}");
        }
    }
    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<LocalRow, LocalCodec>>("custom")
        .await
        .unwrap();
    assert!(table.delete("0").await.unwrap());
    table
        .set("1", LocalRow(Rc::new(vec![7, 0xff])))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    instance
        .backend()
        .clear_derived_store_state()
        .await
        .unwrap();
    let state = database
        .get_store_state::<Table<LocalRow, LocalCodec>>("custom")
        .await
        .unwrap();
    assert_eq!(state.0.operation(&"0".into()), Some(&Lww::Delete));
    assert_eq!(state.0.get(&"1".into()).unwrap().as_ref(), &[7, 0xff]);
}

#[tokio::test]
async fn test_table_raw_bytes_encrypted_staged_warm_and_cold() {
    let (instance, database) = database().await;
    type EncryptedTable = PasswordStore<Table<Vec<u8>, RawBytes>>;
    let payloads = [vec![], vec![0xff, 0, 0x80], b" { \"x\" : 1e0 } ".to_vec()];
    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<EncryptedTable>("raw-secrets").await.unwrap();
    encrypted
        .initialize("opaque-password", Doc::new())
        .await
        .unwrap();
    let table = encrypted.inner().await.unwrap();
    for (index, bytes) in payloads.iter().enumerate() {
        table.set(index.to_string(), bytes.clone()).await.unwrap();
        assert_eq!(&table.get(index.to_string()).await.unwrap(), bytes);
    }
    let id = tx.commit().await.unwrap();
    let entry = database.backend().unwrap().get(&id).await.unwrap();
    assert!(TableData::decode(entry.data("raw-secrets").unwrap()).is_err());
    for cold in [false, true, false] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        let tx = database.new_transaction().await.unwrap();
        let mut encrypted = tx.get_store::<EncryptedTable>("raw-secrets").await.unwrap();
        assert!(encrypted.open("wrong-password").is_err());
        encrypted.open("opaque-password").unwrap();
        let table = encrypted.inner().await.unwrap();
        let state = encrypted.get_state().await.unwrap();
        for (index, bytes) in payloads.iter().enumerate() {
            assert_eq!(&table.get(index.to_string()).await.unwrap(), bytes);
            assert_eq!(state.0.get(&index.to_string()).unwrap().as_ref(), bytes);
        }
        let rows = table.search(|_| true).await.unwrap();
        assert_eq!(rows.len(), payloads.len());
        for (key, bytes) in rows {
            assert_eq!(bytes, payloads[key.parse::<usize>().unwrap()]);
        }
    }
}
