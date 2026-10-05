//! Table-only tolerance at source Entry replay boundaries.

use super::*;
use crate::store::{PasswordStore, RawBytes, StoreStateModel, Table, TableData};
use serde_bytes::ByteBuf;
use std::sync::{Arc, Mutex};

type BytesTable = Table<Vec<u8>, RawBytes>;
const STORE: &str = "payloads";

fn projection() -> Arc<dyn RecordProjection<TableData>> {
    let StoreStateModel::Records(projection) = BytesTable::state_model() else {
        panic!("Table must use records");
    };
    projection
}

fn rows(values: &[(&str, &[u8])], deletes: &[&str]) -> TableData {
    let mut data = TableData::default();
    for (key, bytes) in values {
        data.0.set((*key).into(), ByteBuf::from(*bytes));
    }
    for key in deletes {
        data.0.delete((*key).into());
    }
    data
}

async fn database(instance: &Instance) -> Database {
    let mut admin = instance.login_user("admin", None).await.unwrap();
    let key = match admin.get_default_key() {
        Ok(key) => key,
        Err(_) => admin.add_private_key(Some("payloads")).await.unwrap(),
    };
    admin.create_database(Doc::new(), &key).await.unwrap()
}

// Deliberately bypass the typed writer to model an incompatible historical
// writer. The canonical Entry still goes through normal signing/verification.
async fn append(db: &Database, bytes: Vec<u8>, encrypted: bool) -> ID {
    let tx = db.new_transaction().await.unwrap();
    if encrypted {
        let mut handle = tx
            .get_store::<PasswordStore<BytesTable>>(STORE)
            .await
            .unwrap();
        if handle.is_initialized() {
            handle.open("correct").unwrap();
        } else {
            handle.initialize("correct", Doc::new()).await.unwrap();
        }
    } else {
        tx.get_store::<BytesTable>(STORE).await.unwrap();
    }
    tx.update_subtree(STORE, bytes).await.unwrap();
    tx.commit().await.unwrap()
}

async fn check_view(db: &Database, encrypted: bool, expected: &TableData) {
    let tx = db.new_transaction().await.unwrap();
    let projection = projection();
    let (actual, records) = if encrypted {
        let mut handle = tx
            .get_store::<PasswordStore<BytesTable>>(STORE)
            .await
            .unwrap();
        assert!(handle.open("wrong").is_err());
        assert!(handle.get_state().await.is_err());
        handle.open("correct").unwrap();
        let actual = handle.get_state().await.unwrap();
        let (page, end) = handle
            .projected_scan_page(projection.as_ref(), None, 512)
            .await
            .unwrap();
        assert!(end.is_none());
        for (key, value) in &page.records {
            assert_eq!(
                handle
                    .projected_get(projection.as_ref(), key)
                    .await
                    .unwrap()
                    .as_ref(),
                Some(value)
            );
        }
        (actual, page.records)
    } else {
        let table = tx.get_store::<BytesTable>(STORE).await.unwrap();
        let mut records = Vec::new();
        let mut cursor = None;
        loop {
            let page = table.scan_page(cursor.as_ref(), 37).await.unwrap();
            records.extend(
                page.rows
                    .into_iter()
                    .map(|(key, value)| (key.into_bytes(), value)),
            );
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        for (key, value) in &records {
            assert_eq!(
                table.get(std::str::from_utf8(key).unwrap()).await.unwrap(),
                *value
            );
        }
        (
            db.get_store_state::<BytesTable>(STORE).await.unwrap(),
            records,
        )
    };
    assert_eq!(actual, *expected);
    let mut expected_records = expected
        .0
        .iter()
        .map(|(key, value)| (key.as_bytes().to_vec(), value.to_vec()))
        .collect::<Vec<_>>();
    let mut records = records;
    expected_records.sort();
    records.sort();
    assert_eq!(records, expected_records);
}

async fn mixed_history(encrypted: bool) {
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    let mut before = rows(
        &[
            ("update", b"old"),
            ("delete", b"older"),
            ("opaque", b"\xff\x80\0"),
        ],
        &[],
    );
    // Cross the 128-mutation chunk boundary; opaque rows need no application decoder.
    for i in 0..140 {
        before
            .0
            .set(format!("row-{i:03}"), ByteBuf::from(vec![0, 0xff, i as u8]));
    }
    let first = append(&db, before.encode().unwrap(), encrypted).await;
    let bad_delta = rows(
        &[("update", b"secret-update"), ("partial", b"secret-partial")],
        &["delete"],
    );
    let mut bad_bytes = bad_delta.encode().unwrap();
    bad_bytes.push(0xff);
    assert!(TableData::decode(&bad_bytes).is_err());
    let bad = append(&db, bad_bytes.clone(), encrypted).await;
    let after = rows(
        &[("after", b"\0\xff"), ("row-139", b"replacement")],
        &["row-000"],
    );
    let last = append(&db, after.encode().unwrap(), encrypted).await;
    let expected = before.merge(&after).unwrap();
    let backend = db.backend().unwrap();
    let original = [first, bad, last];
    let mut sources = Vec::new();
    for id in &original {
        sources.push(backend.get(id).await.unwrap());
    }
    if !encrypted {
        assert_eq!(sources[1].data(STORE).unwrap(), &bad_bytes);
    } else {
        assert_ne!(sources[1].data(STORE).unwrap(), &bad_bytes);
    }
    for cold in [true, false, true, false] {
        if cold {
            backend.clear_derived_store_state().await.unwrap();
            backend.clear_derived_store_state().await.unwrap();
        }
        check_view(&db, encrypted, &expected).await;
        // Native recordless fallback must not rely on a published generation.
        let recordless = db
            .clone()
            .with_test_ops(Arc::new(RecordlessOps(backend.clone())));
        check_view(&recordless, encrypted, &expected).await;
    }
    for source in sources {
        assert_eq!(
            backend
                .get(&source.id())
                .await
                .unwrap()
                .to_dagcbor()
                .unwrap(),
            source.to_dagcbor().unwrap()
        );
    }
    #[cfg(all(unix, feature = "service"))]
    if let Some(conn) = instance.remote_connection() {
        // Explicit registered maintenance returns a strict encoded whole state;
        // PasswordStore exercises the authorized local-decryption fallback above.
        if !encrypted {
            assert_eq!(
                conn.get_store_state::<BytesTable>(
                    db.root_id().clone(),
                    SigKey::default(),
                    STORE.into()
                )
                .await
                .unwrap(),
                expected
            );
        }
    }
}

#[tokio::test]
async fn table_payload_native_and_service_cold_warm_ordered_replay() {
    mixed_history(false).await;
}

#[tokio::test]
async fn table_payload_password_cold_warm_and_authorized_fallback() {
    mixed_history(true).await;
}

#[tokio::test]
async fn table_payload_all_unreadable_publishes_empty_default() {
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    for bytes in [
        b"legacy JSON payload".to_vec(),
        vec![0x98, 0x00],
        vec![0xff],
    ] {
        assert!(TableData::decode(&bytes).is_err());
        append(&db, bytes, false).await;
    }
    for _ in 0..2 {
        check_view(&db, false, &TableData::default()).await;
    }
    let tx = db.new_transaction().await.unwrap();
    let view = tx.record_view(STORE, projection().as_ref()).await.unwrap();
    assert!(
        db.ops()
            .store_state_record_scan(&view, &RecordRange::default(), None, 1)
            .await
            .unwrap()
            .records
            .is_empty()
    );
}

#[derive(Clone, Default)]
struct WarningWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for WarningWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for WarningWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn table_payload_warning_is_observable_and_omits_content() {
    use tracing::instrument::WithSubscriber;
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    let good = rows(&[("key", b"old")], &[]).encode().unwrap();
    append(&db, good, false).await;
    let mut bad = rows(&[("secret-key", b"secret-row-bytes")], &["key"])
        .encode()
        .unwrap();
    bad.push(0xff);
    let id = append(&db, bad, false).await;
    let writer = WarningWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(writer.clone())
        .finish();
    let tx = db.new_transaction().await.unwrap();
    let state: TableData = tx
        .get_full_state_with_descriptor(STORE, BytesTable::state_model().descriptor())
        .with_subscriber(subscriber)
        .await
        .unwrap();
    assert_eq!(state, rows(&[("key", b"old")], &[]));
    let warning = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
    assert!(warning.contains("WARN"));
    assert!(warning.contains("Skipping unreadable table:v1 Entry payload"));
    assert!(warning.contains(&id.to_string()));
    assert!(warning.contains(STORE));
    for secret in [
        "secret-key",
        "secret-row-bytes",
        "DeserializationFailed",
        "trailing",
    ] {
        assert!(!warning.contains(secret), "payload/decoder content leaked");
    }
}

#[tokio::test]
async fn table_payload_merge_paths_preserve_strict_missing_source() {
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    let before = rows(&[("key", b"old")], &[]);
    let base = append(&db, before.encode().unwrap(), false).await;
    let bad = append(&db, vec![0xff], false).await;
    let after = rows(&[("after", b"new")], &["key"]);
    let last = append(&db, after.encode().unwrap(), false).await;
    let tx = db.new_transaction().await.unwrap();
    assert_eq!(
        tx.merge_path_entries(
            STORE,
            before.clone(),
            &[bad.clone(), last.clone()],
            &BytesTable::state_model().descriptor()
        )
        .await
        .unwrap(),
        before.merge(&after).unwrap()
    );
    let state: TableData = tx
        .compute_subtree_state_merge_based(
            STORE,
            &[base, bad, last],
            &BytesTable::state_model().descriptor(),
        )
        .await
        .unwrap();
    assert_eq!(state, before.merge(&after).unwrap());
    assert!(
        tx.merge_path_entries(
            STORE,
            before,
            &[ID::from_bytes(b"missing-source")],
            &BytesTable::state_model().descriptor()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn table_payload_cache_staged_data_and_non_table_codecs_stay_strict() {
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    let id = append(&db, rows(&[("key", b"old")], &[]).encode().unwrap(), false).await;
    let backend = db.backend().unwrap();
    let request = state::opaque_request(
        db.root_id(),
        STORE,
        BytesTable::state_model().descriptor(),
        id.to_string().into_bytes(),
        CacheScope::Shared,
    );
    state::publish_opaque(backend.as_ref(), request, vec![0xff])
        .await
        .unwrap();
    assert!(matches!(
        db.get_store_state::<BytesTable>(STORE).await,
        Err(crate::Error::CRDT(_))
    ));
    let tx = db.new_transaction().await.unwrap();
    tx.update_subtree(STORE, vec![0xff]).await.unwrap();
    assert!(matches!(
        tx.get_local_data::<TableData>(STORE),
        Err(crate::Error::Transaction(_))
    ));
    let entry = backend.get(&id).await.unwrap();
    assert!(matches!(
        state::decode_source::<MaxCounter>(
            STORE,
            &entry,
            b"bad",
            &BytesTable::state_model().descriptor()
        ),
        Err(crate::Error::CRDT(_))
    ));
    let malformed_id = tx.commit().await.unwrap();
    let malformed = backend.get(&malformed_id).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    assert!(matches!(
        tx.fold_store_entries::<StagedRows>(
            STORE,
            std::slice::from_ref(&malformed),
            &BytesTable::state_model().descriptor()
        ),
        Err(crate::Error::CRDT(_))
    ));
    // Reusing TableData is not enough to opt a custom non-Table projection
    // into Table's source policy.
    assert!(matches!(
        tx.fold_store_entries::<TableData>(
            STORE,
            &[malformed],
            &StoreStateModel::<TableData>::opaque("custom/strict", 0).descriptor()
        ),
        Err(crate::Error::CRDT(_))
    ));
}

#[tokio::test]
async fn table_payload_decryption_failure_never_becomes_skip() {
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    append(&db, rows(&[("key", b"old")], &[]).encode().unwrap(), true).await;
    let tx = db.new_transaction().await.unwrap();
    // Authenticated decryption fails even when the bytes themselves are a valid
    // plaintext TableData value: no ciphertext-as-plaintext fallback is allowed.
    tx.update_subtree(STORE, TableData::default().encode().unwrap())
        .await
        .unwrap();
    let id = tx.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut handle = tx
        .get_store::<PasswordStore<BytesTable>>(STORE)
        .await
        .unwrap();
    handle.open("correct").unwrap();
    assert!(handle.get_state().await.is_err());
    assert!(
        handle
            .projected_get(projection().as_ref(), b"key")
            .await
            .is_err()
    );
    assert!(
        handle
            .projected_scan_page(projection().as_ref(), None, 1)
            .await
            .is_err()
    );
    assert_eq!(
        db.ops().get(&id).await.unwrap().data(STORE).unwrap(),
        &vec![0x80]
    );
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn table_payload_wire_decode_and_authorization_errors_remain_hard() {
    use crate::service::client::RemoteConnection;
    use crate::service::protocol::{
        DatabaseOp, Handshake, HandshakeAck, PROTOCOL_VERSION, ServerFrame, ServiceRequest,
        ServiceResponse, read_frame, write_frame,
    };
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("table-wire.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
        write_frame(
            &mut stream,
            &HandshakeAck {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        for response in [
            ServiceResponse::StoreState(TableData::default().encode().unwrap()),
            ServiceResponse::StoreState(vec![0xff]),
            ServiceResponse::Error(crate::service::error::ServiceError {
                module: "auth".into(),
                kind: "PermissionDenied".into(),
                message: "refused".into(),
            }),
        ] {
            let request: ServiceRequest = read_frame(&mut stream).await.unwrap().unwrap();
            assert!(
                matches!(request, ServiceRequest::AuthenticatedDb(ref request) if matches!(request.op, DatabaseOp::EnsureStoreStateGeneration { .. }))
            );
            write_frame(&mut stream, &ServerFrame::Response(Box::new(response)))
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                read_frame::<_, ServiceRequest>(&mut stream)
            )
            .await
            .is_err(),
            "wire/auth errors triggered history fallback"
        );
    });
    let conn = RemoteConnection::connect(&socket).await.unwrap();
    assert_eq!(
        conn.get_store_state::<BytesTable>(ID::default(), SigKey::default(), STORE.into())
            .await
            .unwrap(),
        TableData::default()
    );
    assert!(matches!(
        conn.get_store_state::<BytesTable>(ID::default(), SigKey::default(), STORE.into())
            .await,
        Err(crate::Error::CRDT(_))
    ));
    let denied = conn
        .get_store_state::<BytesTable>(ID::default(), SigKey::default(), STORE.into())
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("PermissionDenied"));
    tokio::time::timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn table_payload_staging_and_size_errors_remain_hard() {
    let (instance, _daemon) = projection_backend().await;
    let db = database(&instance).await;
    let id = append(&db, rows(&[("key", b"old")], &[]).encode().unwrap(), false).await;
    let entry = db.ops().get(&id).await.unwrap();
    let projection = projection();
    let request = state::records_request(
        db.root_id(),
        STORE,
        projection.descriptor(),
        b"staging-error".to_vec(),
        CacheScope::Shared,
    );
    let refused = RecordlessOps(db.backend().unwrap());
    let error = state::publish_records(
        &refused,
        request.clone(),
        [&entry].into_iter(),
        projection.as_ref(),
    )
    .await
    .unwrap_err();
    assert!(error.is_unsupported_store_state());
    assert!(
        db.ops()
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_none()
    );

    let large = rows(&[("large", &vec![0xff; state::CHUNK_BYTES])], &[])
        .encode()
        .unwrap();
    let large_id = append(&db, large, false).await;
    let large_entry = db.ops().get(&large_id).await.unwrap();
    let request = state::records_request(
        db.root_id(),
        STORE,
        projection.descriptor(),
        b"oversize".to_vec(),
        CacheScope::Shared,
    );
    let error = state::publish_records(
        db.ops(),
        request.clone(),
        [&large_entry].into_iter(),
        projection.as_ref(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, crate::Error::Backend(ref error) if matches!(**error, crate::backend::BackendError::RecordTooLarge { .. }))
    );
    assert!(
        db.ops()
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_none()
    );
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn table_payload_read_only_maintenance_and_encrypted_fallback() {
    use crate::auth::types::{AuthKey, Permission};
    use std::time::Duration;
    let (server, admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    admin
        .admin()
        .await
        .unwrap()
        .create_user(crate::NewUser::passwordless("reader"))
        .await
        .unwrap();
    let expected = rows(&[("key", b"old"), ("after", b"\xff\0")], &[]);
    let mut databases = Vec::new();
    for encrypted in [false, true] {
        let db = database(&server).await;
        append(
            &db,
            rows(&[("key", b"old")], &[]).encode().unwrap(),
            encrypted,
        )
        .await;
        let mut bad = rows(&[], &["key"]).encode().unwrap();
        bad.push(0xff);
        append(&db, bad, encrypted).await;
        let last = append(
            &db,
            rows(&[("after", b"\xff\0")], &[]).encode().unwrap(),
            encrypted,
        )
        .await;
        let tx = db.new_transaction().await.unwrap();
        tx.get_settings()
            .unwrap()
            .set_global_auth_key(AuthKey::active(None, Permission::Read))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        databases.push((db, last, encrypted));
    }
    server.backend().clear_derived_store_state().await.unwrap();
    server.backend().clear_derived_store_state().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("read-table.sock");
    let daemon = crate::service::ServiceServer::bind(server.clone(), &socket)
        .await
        .unwrap();
    let (stop, rx) = tokio::sync::watch::channel(());
    let task = tokio::spawn(daemon.run(rx));
    let remote = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let reader = remote.login_user("reader", None).await.unwrap();
    let signing_key = reader
        .get_signing_key(&reader.get_default_key().unwrap())
        .unwrap();
    for (db, last, encrypted) in databases {
        let read_db = Database::open(&remote, db.root_id())
            .await
            .unwrap()
            .with_key(crate::database::DatabaseKey::global(signing_key.clone()));
        assert_eq!(
            read_db.current_permission().await.unwrap(),
            Permission::Read
        );
        let descriptor = if encrypted {
            PasswordStore::<BytesTable>::state_model().descriptor()
        } else {
            BytesTable::state_model().descriptor()
        };
        let request = state::opaque_request(
            db.root_id(),
            STORE,
            descriptor,
            last.to_string().into_bytes(),
            CacheScope::Shared,
        );
        for _ in 0..2 {
            check_view(&read_db, encrypted, &expected).await;
        }
        assert!(
            read_db
                .ops()
                .begin_store_state_staging(request.clone())
                .await
                .is_err()
        );
        if encrypted {
            // Read-only fallback must not publish a shared opaque generation.
            assert!(
                db.ops()
                    .resolve_store_state(&request)
                    .await
                    .unwrap()
                    .is_none()
            );
        } else {
            let conn = remote.remote_connection().unwrap();
            assert_eq!(
                conn.get_store_state::<BytesTable>(
                    db.root_id().clone(),
                    SigKey::default(),
                    STORE.into()
                )
                .await
                .unwrap(),
                expected
            );
            let request = state::records_request(
                db.root_id(),
                STORE,
                BytesTable::state_model().descriptor(),
                create_merge_cache_id(&[last]).to_string().into_bytes(),
                CacheScope::Shared,
            );
            assert!(
                db.ops()
                    .resolve_store_state(&request)
                    .await
                    .unwrap()
                    .is_some(),
                "read-scoped daemon maintenance must publish real rows"
            );
        }
        let tx = read_db.new_transaction().await.unwrap();
        assert!(
            tx.commit().await.is_err(),
            "Read does not grant commit authority"
        );
    }
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
