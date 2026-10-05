//! LWW configuration selects interpretation without rejecting mixed histories.

use eidetica::{
    Database,
    crdt::Doc,
    store::{DocStore, RawBytes, RawTable, StoreError, Table},
};

use super::table_codecs::database;

#[derive(Clone)]
struct WarningWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for WarningWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct WarningCodec;

impl eidetica::store::RowCodec<Vec<u8>> for WarningCodec {
    const FORMAT_ID: &'static str = "warning-test:v1";

    fn encode(value: &Vec<u8>) -> eidetica::Result<Vec<u8>> {
        Ok(value.clone())
    }

    fn decode(_: &[u8]) -> eidetica::Result<Vec<u8>> {
        Err(StoreError::DeserializationFailed {
            store: "codec".into(),
            reason: "private-error-string".into(),
        }
        .into())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_table_unreadable_row_warning_does_not_leak_content() {
    let (_instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let table = tx
        .get_store::<Table<Vec<u8>, WarningCodec>>("warning-rows")
        .await
        .unwrap();
    table
        .set("private-key", b"private-payload".to_vec())
        .await
        .unwrap();
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer = WarningWriter(buffer.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    assert!(matches!(table.get("private-key").await,
        Err(eidetica::Error::Store(error)) if matches!(*error, StoreError::KeyNotFound { .. })));
    let page = table.scan_page(None, 1).await.unwrap();
    assert!(page.rows.is_empty());
    assert!(page.next.is_none());
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert_eq!(output.matches("Skipping Table row").count(), 2);
    assert!(output.contains("warning-test:v1"));
    for secret in ["private-key", "private-payload", "private-error-string"] {
        assert!(!output.contains(secret), "warning leaked {secret}");
    }
}

async fn select_codec(database: &Database, codec: &str) {
    let tx = database.new_transaction().await.unwrap();
    let mut changed = Doc::new();
    changed.set("row_codec", codec);
    tx.get_index()
        .await
        .unwrap()
        .set_entry("rows", "table:v1", changed)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn test_table_unreadable_encrypted_rows_keep_raw_bytes_and_password_errors() {
    use eidetica::store::PasswordStore;

    let (instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<Table<serde_json::Value>>>("rows")
        .await
        .unwrap();
    encrypted
        .initialize("row-skip-password", Doc::new())
        .await
        .unwrap();
    let writer = encrypted.inner().await.unwrap();
    writer
        .set("bad", serde_json::json!({"new_schema": true}))
        .await
        .unwrap();
    writer.set("good", serde_json::json!(7)).await.unwrap();
    tx.commit().await.unwrap();

    for cold in [false, true, false] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        let tx = database.new_transaction().await.unwrap();
        let mut encrypted = tx
            .get_store::<PasswordStore<Table<u64>>>("rows")
            .await
            .unwrap();
        assert!(encrypted.inner().await.is_err());
        assert!(encrypted.open("wrong-password").is_err());
        encrypted.open("row-skip-password").unwrap();
        let reader = encrypted.inner().await.unwrap();
        assert!(matches!(reader.get("bad").await,
            Err(eidetica::Error::Store(error)) if matches!(*error, StoreError::KeyNotFound { .. })));
        assert_eq!(reader.get("good").await.unwrap(), 7);
        let mut cursor = None;
        let mut rows = Vec::new();
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(
                pages <= 2,
                "skipped rows must not stall the physical cursor"
            );
            let page = reader.scan_page(cursor.as_ref(), 1).await.unwrap();
            rows.extend(page.rows);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(rows, vec![("good".into(), 7)]);
        let mut encrypted_raw = tx
            .get_store::<PasswordStore<RawTable>>("rows")
            .await
            .unwrap();
        encrypted_raw.open("row-skip-password").unwrap();
        let raw = encrypted_raw.inner().await.unwrap();
        assert_eq!(raw.get("bad").await.unwrap(), br#"{"new_schema":true}"#);
    }
}

#[tokio::test]
async fn test_table_history_current_codec_selects_rows_without_rewriting_bytes() {
    let (instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    let rows = tx
        .get_store::<Table<Vec<u8>, RawBytes>>("rows")
        .await
        .unwrap();
    rows.set("bad", vec![0xff]).await.unwrap();
    rows.set("good", b"[3]".to_vec()).await.unwrap();
    let original = tx.commit().await.unwrap();
    let original_entry = database.backend().unwrap().get(&original).await.unwrap();
    let original_bytes = original_entry.data("rows").unwrap().clone();
    // Warm the opaque projection under the original codec.
    assert_eq!(
        database
            .get_store_viewer::<RawTable>("rows")
            .await
            .unwrap()
            .get("bad")
            .await
            .unwrap(),
        vec![0xff]
    );

    select_codec(&database, "json:v1").await;
    for cold in [false, true, false] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        assert!(
            database
                .get_store_viewer::<Table<Vec<u8>, RawBytes>>("rows")
                .await
                .is_err()
        );
        let rows = database
            .get_store_viewer::<Table<Vec<u8>>>("rows")
            .await
            .unwrap();
        assert!(matches!(rows.get("bad").await,
            Err(eidetica::Error::Store(error)) if matches!(*error, StoreError::KeyNotFound { .. })));
        let first = rows.scan_page(None, 1).await.unwrap();
        assert!(first.rows.is_empty());
        assert!(
            first.next.is_some(),
            "filtered empty page must retain its cursor"
        );
        let next = rows.scan_page(first.next.as_ref(), 1).await.unwrap();
        assert_eq!(next.rows, vec![("good".into(), vec![3])]);
        assert!(next.next.is_none());
        assert_eq!(
            rows.search(|_| true).await.unwrap(),
            vec![("good".into(), vec![3])]
        );
        let raw = database.get_store_viewer::<RawTable>("rows").await.unwrap();
        assert_eq!(raw.get("bad").await.unwrap(), vec![0xff]);
        assert_eq!(raw.get("good").await.unwrap(), b"[3]");
    }
    // A -> B -> A is allowed: configuration changes do not rewrite source data.
    select_codec(&database, "raw:v1").await;
    assert_eq!(
        database
            .get_store_viewer::<Table<Vec<u8>, RawBytes>>("rows")
            .await
            .unwrap()
            .get("bad")
            .await
            .unwrap(),
        vec![0xff]
    );
    assert_eq!(
        database
            .backend()
            .unwrap()
            .get(&original)
            .await
            .unwrap()
            .data("rows")
            .unwrap(),
        &original_bytes
    );
}

#[tokio::test]
async fn test_table_history_missing_configuration_errors_without_erasing_rows() {
    let (_instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    tx.get_store::<Table<Vec<u8>, RawBytes>>("rows")
        .await
        .unwrap()
        .set("row", vec![0xff])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = database.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("_index")
        .await
        .unwrap()
        .delete("rows")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(database.get_store_viewer::<RawTable>("rows").await.is_err());
    select_codec(&database, "raw:v1").await;
    assert_eq!(
        database
            .get_store_viewer::<RawTable>("rows")
            .await
            .unwrap()
            .get("row")
            .await
            .unwrap(),
        vec![0xff]
    );
}

#[tokio::test]
async fn test_table_history_conflicting_registrations_follow_lww_configuration() {
    let (_instance, database) = database().await;
    let base = database.snapshot().await.unwrap();
    let left = database.new_transaction_at(&base).await.unwrap();
    let right = database.new_transaction_at(&base).await.unwrap();
    left.get_store::<Table<Vec<u8>, RawBytes>>("rows")
        .await
        .unwrap()
        .set("left", vec![0xff])
        .await
        .unwrap();
    right
        .get_store::<Table<Vec<u8>>>("rows")
        .await
        .unwrap()
        .set("right", vec![0])
        .await
        .unwrap();
    left.commit().await.unwrap();
    right.commit().await.unwrap();
    let tx = database.new_transaction().await.unwrap();
    let required = tx
        .get_index()
        .await
        .unwrap()
        .get_entry("rows")
        .await
        .unwrap()
        .config
        .get("row_codec")
        .unwrap()
        .as_text()
        .unwrap()
        .to_owned();
    let raw = tx.get_store::<RawTable>("rows").await.unwrap();
    assert_eq!(raw.row_codec_id().await.unwrap(), required);
    assert_eq!(
        raw.scan_page(None, 10).await.unwrap().rows,
        vec![
            ("left".into(), vec![0xff]),
            ("right".into(), b"[0]".to_vec())
        ]
    );
    match required.as_str() {
        "raw:v1" => {
            assert!(tx.get_store::<Table<Vec<u8>>>("rows").await.is_err());
            let rows = tx
                .get_store::<Table<Vec<u8>, RawBytes>>("rows")
                .await
                .unwrap();
            assert_eq!(rows.get("left").await.unwrap(), vec![0xff]);
            assert_eq!(rows.get("right").await.unwrap(), b"[0]");
        }
        "json:v1" => {
            assert!(
                tx.get_store::<Table<Vec<u8>, RawBytes>>("rows")
                    .await
                    .is_err()
            );
            let rows = tx.get_store::<Table<Vec<u8>>>("rows").await.unwrap();
            assert_eq!(
                rows.search(|_| true).await.unwrap(),
                vec![("right".into(), vec![0])]
            );
        }
        _ => panic!("unexpected LWW codec: {required}"),
    }
}

#[tokio::test]
async fn test_table_history_matching_first_registrations_merge() {
    let (_instance, database) = database().await;
    let base = database.snapshot().await.unwrap();
    let left = database.new_transaction_at(&base).await.unwrap();
    let right = database.new_transaction_at(&base).await.unwrap();
    left.get_store::<Table<Vec<u8>, RawBytes>>("rows")
        .await
        .unwrap()
        .set("left", vec![0xff])
        .await
        .unwrap();
    right
        .get_store::<Table<Vec<u8>, RawBytes>>("rows")
        .await
        .unwrap()
        .set("right", vec![0])
        .await
        .unwrap();
    left.commit().await.unwrap();
    right.commit().await.unwrap();
    let raw = database.get_store_viewer::<RawTable>("rows").await.unwrap();
    assert_eq!(raw.row_codec_id().await.unwrap(), "raw:v1");
    assert_eq!(
        raw.scan_page(None, 10).await.unwrap().rows,
        vec![("left".into(), vec![0xff]), ("right".into(), vec![0])]
    );
}

#[tokio::test]
async fn test_table_unreadable_winning_row_does_not_resurrect_older_value() {
    let (instance, database) = database().await;
    let tx = database.new_transaction().await.unwrap();
    tx.get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap()
        .set("row", serde_json::json!(3))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = database.new_transaction().await.unwrap();
    let writer = tx
        .get_store::<Table<serde_json::Value>>("rows")
        .await
        .unwrap();
    writer
        .set("row", serde_json::json!({"new_schema": true}))
        .await
        .unwrap();
    let reader = tx.get_store::<Table<u64>>("rows").await.unwrap();
    assert!(matches!(reader.get("row").await,
        Err(eidetica::Error::Store(error)) if matches!(*error, StoreError::KeyNotFound { .. })));
    assert!(reader.scan_page(None, 5).await.unwrap().rows.is_empty());
    tx.commit().await.unwrap();
    for cold in [false, true] {
        if cold {
            instance
                .backend()
                .clear_derived_store_state()
                .await
                .unwrap();
        }
        let reader = database
            .get_store_viewer::<Table<u64>>("rows")
            .await
            .unwrap();
        assert!(matches!(reader.get("row").await,
            Err(eidetica::Error::Store(error)) if matches!(*error, StoreError::KeyNotFound { .. })));
        assert!(reader.search(|_| true).await.unwrap().is_empty());
        let raw = database.get_store_viewer::<RawTable>("rows").await.unwrap();
        assert_eq!(raw.get("row").await.unwrap(), br#"{"new_schema":true}"#);
    }
}
