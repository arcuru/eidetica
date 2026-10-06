//! Authenticated Table-owned query/record paths, not legacy maintenance calls.
use super::sdk_cache::{Fault, observed_proxy, stop_proxy};
use super::*;
use eidetica::backend::RecordMutation;
use eidetica::service::protocol::DatabaseOp as Op;
use eidetica::store::{GetRow, RawTable, ScanRows, SearchRows, TableData};

fn no_legacy(ops: &[Op]) {
    assert!(
        !ops.iter().any(|op| matches!(
            op,
            Op::EnsureStoreStateGeneration { .. }
                | Op::EnsureRecordGeneration { .. }
                | Op::GetStoreEntries { .. }
                | Op::StoreStateRecordGet { .. }
                | Op::StoreStateRecordScan { .. }
                | Op::ResolveStoreState { .. }
        )),
        "normal Table query used legacy maintenance/history wire"
    );
}

#[tokio::test]
async fn table_query_read_only_encrypted_cold_warm_point_page_raw_and_repair() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    create_user_via_admin(&server, "reader").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let reader = server.login_user("reader", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<Table<String>>>("secret")
        .await
        .unwrap();
    encrypted.initialize("correct", Doc::new()).await.unwrap();
    let table = encrypted.inner().await.unwrap();
    for n in 0..260 {
        table
            .set(format!("key-{n:03}"), format!("private-row-{n}"))
            .await
            .unwrap();
    }
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &reader.get_default_key().unwrap(),
            eidetica::auth::types::AuthKey::active(None, Permission::Read),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "reader").await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let pinned = remote.new_transaction().await.unwrap();
    let source = pinned.query_source().unwrap();
    let mut encrypted = pinned
        .get_store::<PasswordStore<Table<String>>>("secret")
        .await
        .unwrap();
    assert!(encrypted.inner().await.is_err());
    assert!(encrypted.open("wrong").is_err());
    encrypted.open("correct").unwrap();
    let table = encrypted.inner().await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(
        table.query(GetRow("key-129")).await.unwrap(),
        "private-row-129"
    );
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.raw_count(), 1);
        assert_eq!(seen.begin_count(), 1);
        no_legacy(&seen.requests);
        let rows = seen
            .requests
            .iter()
            .filter_map(|op| {
                if let Op::PrivateAssistanceChunk { mutations, .. } = op {
                    Some(mutations)
                } else {
                    None
                }
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 260);
        assert!(rows.iter().all(|m| matches!(m, RecordMutation::Put { key, value } if key != b"key-129" && !value.windows(12).any(|w| w == b"private-row-"))));
    }
    // An already-published private point is exact-range: no raw history,
    // opaque hydration, fresh admission or full physical record scan.
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(table.get("key-129").await.unwrap(), "private-row-129");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.raw_count(), 0);
        assert_eq!(seen.begin_count(), 0);
        assert_eq!(seen.lookup_count(), 1);
        let Op::LookupPrivateMaterialization {
            source: used,
            range,
            after,
            ..
        } = seen
            .requests
            .iter()
            .find(|op| matches!(op, Op::LookupPrivateMaterialization { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(used.source, source);
        assert!(after.is_none());
        let start = range.start.as_ref().unwrap();
        let end = range.end.as_ref().unwrap();
        assert_eq!(end, &[start.as_slice(), &[0]].concat());
        no_legacy(&seen.requests);
    }
    seen.lock().unwrap().reset(Fault::None);
    let first = table
        .query(ScanRows {
            cursor: None,
            limit: 3,
        })
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 3);
    assert!(first.next.is_some());
    let second = table.scan_page(first.next.as_ref(), 3).await.unwrap();
    assert_eq!(second.rows.len(), 3);
    assert!(
        second
            .rows
            .iter()
            .all(|(key, _)| !first.rows.iter().any(|(old, _)| old == key))
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    assert_eq!(seen.lock().unwrap().lookup_count(), 2);
    let mut raw_encrypted = pinned
        .get_store::<PasswordStore<RawTable>>("secret")
        .await
        .unwrap();
    raw_encrypted.open("correct").unwrap();
    let raw = raw_encrypted.inner().await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(
        raw.query(GetRow("key-129")).await.unwrap(),
        b"\"private-row-129\""
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    assert_eq!(raw.row_codec_id().await.unwrap(), "json:v0");
    seen.lock().unwrap().reset(Fault::Corrupt);
    assert_eq!(raw.get("key-129").await.unwrap(), b"\"private-row-129\"");
    assert_eq!(seen.lock().unwrap().raw_count(), 1);
    for fault in [
        Fault::InvalidSource,
        Fault::InvalidToken,
        Fault::Denied,
        Fault::BadResponse,
        Fault::BadKey,
        Fault::BadShape,
        Fault::ChangedRefusal,
    ] {
        seen.lock().unwrap().reset(fault);
        assert!(raw.get("key-129").await.is_err());
        assert_eq!(seen.lock().unwrap().raw_count(), 0);
    }
    for fault in [Fault::BadRawSource, Fault::SourceUnavailable] {
        seen.lock().unwrap().reset(fault);
        assert!(raw.get("key-129").await.is_err());
        assert_eq!(seen.lock().unwrap().raw_count(), 1);
    }
    seen.lock().unwrap().reset(Fault::RepeatedExpiry);
    assert!(raw.get("key-129").await.is_err());
    assert_eq!(seen.lock().unwrap().raw_count(), 2);
    seen.lock().unwrap().reset(Fault::Quota);
    server.backend().clear_derived_store_state().await.unwrap();
    assert_eq!(raw.get("key-129").await.unwrap(), b"\"private-row-129\"");
    assert_eq!(seen.lock().unwrap().begin_count(), 1);
    // Publication is optional; source-pinned point stays old after a write.
    let later = db.new_transaction().await.unwrap();
    let mut enc = later
        .get_store::<PasswordStore<Table<String>>>("secret")
        .await
        .unwrap();
    enc.open("correct").unwrap();
    enc.inner()
        .await
        .unwrap()
        .set("key-129", "later".into())
        .await
        .unwrap();
    later.commit().await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(raw.get("key-129").await.unwrap(), b"\"private-row-129\"");
    assert!(
        raw.scan_page(None, 3).await.is_err(),
        "concurrent frontier invalidates scan, not point source"
    );
    // Revocation is actual current settings, not a fabricated cache error.
    let revoke = db.new_transaction().await.unwrap();
    revoke
        .get_settings()
        .unwrap()
        .revoke_auth_key(&reader.get_default_key().unwrap())
        .await
        .unwrap();
    revoke.commit().await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert!(raw.get("key-129").await.is_err());
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    drop(client);
    stop_proxy(task).await;
    drop(shutdown);
}

#[tokio::test]
async fn table_query_registered_opaque_rows_skip_without_repair_and_strict_messages() {
    let (socket, shutdown, server, dir) = start_test_server().await;
    let (_client, root, _) = setup_db(&server, &socket, "owner").await;
    let owner = server.login_user("owner", None).await.unwrap();
    let db = owner.open_database(&root).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<Table<String>>("rows")
        .await
        .unwrap()
        .set("good", "kept".into())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut delta = TableData::default();
    for n in 0..260 {
        delta.0.set(
            format!("bad-{n:03}"),
            serde_bytes::ByteBuf::from(vec![0xff, 0, 0x80]),
        );
    }
    insert_signed_store_payload(&db, &owner, "rows", delta.encode().unwrap()).await;
    let (proxy, seen, task) = observed_proxy(&socket, dir.path()).await;
    let client = login_client(&proxy, "owner").await;
    let remote = eidetica::Database::open(&client, &root).await.unwrap();
    let tx = remote.new_transaction().await.unwrap();
    let table = tx.get_store::<Table<String>>("rows").await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    let mut cursor = None;
    let mut empty = 0;
    let mut rows = vec![];
    loop {
        let page = table
            .query(ScanRows {
                cursor: cursor.as_ref(),
                limit: 128,
            })
            .await
            .unwrap();
        assert!(page.rows.len() <= 128);
        if page.rows.is_empty() {
            empty += 1;
            assert!(page.next.is_some());
        }
        rows.extend(page.rows);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
        assert!(empty <= 2, "unreadable-only pages must progress");
    }
    assert_eq!(empty, 2);
    assert_eq!(rows, vec![("good".into(), "kept".into())]);
    assert!(table.get("bad-000").await.unwrap_err().is_not_found());
    no_legacy(&seen.lock().unwrap().requests);
    let raw = tx.get_store::<RawTable>("rows").await.unwrap();
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(raw.get("bad-000").await.unwrap(), [0xff, 0, 0x80]);
    let threshold = std::rc::Rc::new(3usize);
    assert_eq!(
        table
            .query(SearchRows(|row: &String| row.len() > *threshold))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    assert_eq!(seen.lock().unwrap().lookup_count(), 0);
    no_legacy(&seen.lock().unwrap().requests);
    seen.lock().unwrap().reset(Fault::BadQueryResponse);
    assert!(table.get("good").await.is_err());
    assert_eq!(seen.lock().unwrap().raw_count(), 0);
    seen.lock().unwrap().reset(Fault::None);
    assert_eq!(table.get("good").await.unwrap(), "kept");
    assert!(
        tx.query_store("rows", "table:v0.1", b"malformed".to_vec())
            .await
            .is_err()
    );
    assert!(
        tx.query_store("rows", "table:v0", b"{}".to_vec())
            .await
            .is_err()
    );
    assert!(
        tx.query_store(
            "rows",
            "table:v0.1",
            br#"{"Page":{"after":null,"limit":129,"repair":true}}"#.to_vec()
        )
        .await
        .is_err()
    );
    drop(client);
    stop_proxy(task).await;
    drop(shutdown);
}
