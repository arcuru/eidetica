//! Real signed many-small-Entry history, observed at the physical frame boundary.
use super::*;
use eidetica::service::protocol::MAX_FRAME_SIZE;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWrite};

#[derive(Default)]
struct Observed {
    pages: AtomicUsize,
    whole: AtomicUsize,
    prefixes: Mutex<Vec<u32>>,
}

async fn forward<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    requests: bool,
    stats: Arc<Observed>,
) -> std::io::Result<()> {
    loop {
        let mut prefix = [0; 4];
        if reader.read(&mut prefix[..1]).await? == 0 {
            return Ok(());
        }
        reader.read_exact(&mut prefix[1..]).await?;
        let size = u32::from_be_bytes(prefix);
        assert!(size <= MAX_FRAME_SIZE, "physical prefix {size}");
        let mut body = vec![0; size as usize];
        reader.read_exact(&mut body).await?;
        if requests {
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if let Some(op) = request.get("AuthenticatedDb").and_then(|r| r.get("op")) {
                if op.get("GetStoreHistoryPage").is_some() {
                    stats.pages.fetch_add(1, Ordering::Relaxed);
                }
                if op.get("GetStoreEntries").is_some() {
                    stats.whole.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else {
            stats.prefixes.lock().unwrap().push(size);
        }
        writer.write_all(&prefix).await?;
        writer.write_all(&body).await?;
    }
}

#[tokio::test]
async fn ordinary_sdk_many_small_entries_exceed_legacy_frame_via_pages() {
    let (path, _shutdown, server, dir) = start_test_server().await;
    let mut admin = server.login_account("admin", None).await.unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    // A stable overwritten logical value avoids requiring an ever-growing
    // materialized store. Raw signed history still retains each immutable write.
    for n in 0..96 {
        db.with_transaction(|tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store
                .set("bulk", format!("{n:03}{}", "x".repeat(192 * 1024)))
                .await?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let tips = db.snapshot().await.unwrap();
    let expected = server
        .backend()
        .store_at(db.root_id(), "data", &tips)
        .await
        .unwrap();
    assert_eq!(expected.len(), 96);
    assert!(
        expected
            .iter()
            .all(|entry| entry.auth().signature.is_some()),
        "fixture Entries must be signed"
    );
    let max_entry = expected
        .iter()
        .map(|e| serde_json::to_vec(e).unwrap().len())
        .max()
        .unwrap();
    assert!(
        max_entry < 1024 * 1024,
        "fixture must have only sub-1MiB Entries: {max_entry}"
    );
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(
        &mut count,
        &ServerFrame::Response(Box::new(ServiceResponse::Entries(expected.clone()))),
    )
    .unwrap();
    assert!(
        count.0 > MAX_FRAME_SIZE as usize,
        "legacy fixture only {} bytes",
        count.0
    );
    let proxy_path = dir.path().join("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
    let stats = Arc::new(Observed::default());
    let observed = stats.clone();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let upstream = UnixStream::connect(path).await.unwrap();
        let (cr, cw) = client.into_split();
        let (sr, sw) = upstream.into_split();
        tokio::select! { _ = forward(cr, sw, true, observed.clone()) => {}, _ = forward(sr, cw, false, observed) => {} }
    });
    let client = Instance::connect(format!("unix://{}", proxy_path.display()))
        .await
        .unwrap();
    let _session = client.login_account("admin", None).await.unwrap();
    let remote = eidetica::Database::open(&client, db.root_id())
        .await
        .unwrap();
    stats.pages.store(0, Ordering::Relaxed);
    stats.whole.store(0, Ordering::Relaxed);
    stats.prefixes.lock().unwrap().clear();
    eprintln!(
        "AGGREGATE FIXTURE signed_entries={} max_individual={} legacy_frame={}",
        expected.len(),
        max_entry,
        count.0
    );
    let entries = tokio::time::timeout(
        Duration::from_secs(120),
        remote.get_store_entries("data", tips.tips(), ReadScope::Verified),
    )
    .await
    .expect("history call hung")
    .expect("ordinary SDK history failed");
    assert_eq!(entries, expected);
    assert!(
        remote_conn(&client)
            .get_instance_metadata()
            .await
            .unwrap()
            .is_some()
    );
    let pages = stats.pages.load(Ordering::Relaxed);
    let whole = stats.whole.load(Ordering::Relaxed);
    let prefixes = stats.prefixes.lock().unwrap().clone();
    assert!(pages > 1, "collector sent {pages} page calls");
    assert_eq!(
        whole, 0,
        "negotiated collector used legacy whole-history RPC"
    );
    assert!(prefixes.iter().all(|size| *size <= MAX_FRAME_SIZE));
    eprintln!(
        "AGGREGATE PROOF entries={} max_individual={} legacy_frame={} page_calls={} legacy_calls={} physical_prefixes={prefixes:?}",
        entries.len(),
        max_entry,
        count.0,
        pages,
        whole
    );
    proxy.abort();
    let _ = proxy.await;
}

#[tokio::test]
async fn oversized_response_retires_only_affected_handler() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let mut admin = server.login_account("admin", None).await.unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("data")
            .await?
            .set("small", "first-page")
            .await?;
        Ok(())
    })
    .await
    .unwrap();
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("data")
            .await?
            .set("large", "x".repeat(17 * 1024 * 1024))
            .await?;
        Ok(())
    })
    .await
    .unwrap();
    let client = Instance::connect(format!("unix://{}", path.display()))
        .await
        .unwrap();
    let _session = client.login_account("admin", None).await.unwrap();
    let conn = remote_conn(&client);
    let tips = db.snapshot().await.unwrap();
    let result = conn
        .get_store_entries(
            db.root_id().clone(),
            SigKey::from_pubkey(&key),
            "data".into(),
            tips.tips().to_vec(),
            ReadScope::Verified,
        )
        .await;
    assert!(
        result.is_err(),
        "hard-oversized Entry after progress returned a successful prefix"
    );
    assert!(result.unwrap_err().to_string().contains("too large"));
    assert!(
        conn.get_instance_metadata().await.unwrap().is_some(),
        "explicit page error broke FIFO/connection"
    );
    let id = db.snapshot().await.unwrap().tips()[0].clone();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(30),
            conn.db_get_entry(db.root_id().clone(), SigKey::from_pubkey(&key), id)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(conn.get_instance_metadata().await.is_err());
    let another = Instance::connect(format!("unix://{}", path.display()))
        .await
        .unwrap();
    assert_eq!(another.id(), client.id());
}

#[tokio::test]
async fn fixed_frame_shutdown_interrupts_jointly_owned_server_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("joint.sock");
    let (instance, mut admin) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let key = admin.get_default_key().unwrap();
    let private = admin.get_signing_key(&key).unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("data")
            .await?
            .set("fitting", "x".repeat(8 * 1024 * 1024))
            .await?;
        Ok(())
    })
    .await
    .unwrap();
    let id = db.snapshot().await.unwrap().tips()[0].clone();
    let server = ServiceServer::bind(instance, &path).await.unwrap();
    let (shutdown, rx) = watch::channel(());
    let task = tokio::spawn(server.run(rx));
    let mut reader = request_large_entry(
        &path,
        true,
        db.root_id().clone(),
        SigKey::from_pubkey(&key),
        id,
        &private,
    )
    .await;
    let mut prefix = [0; 4];
    tokio::time::timeout(Duration::from_secs(30), reader.read_exact(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    let expected = u32::from_be_bytes(prefix) as usize;
    assert!(expected > 16 * 1024 * 1024 && expected <= MAX_FRAME_SIZE as usize);
    drop(shutdown);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut partial = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut partial))
        .await
        .expect("orphaned server writer kept socket open")
        .unwrap();
    assert!(
        partial.len() < expected,
        "detached writer completed a response after handler shutdown"
    );
}

use eidetica::{
    entry::ID,
    service::protocol::{AuthenticatedDbRequest, DatabaseOp},
};
async fn request_large_entry(
    path: &std::path::Path,
    capable: bool,
    root: ID,
    identity: SigKey,
    id: ID,
    private: &PrivateKey,
) -> ReadHalf<UnixStream> {
    let stream = UnixStream::connect(path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_frame(
        &mut writer,
        &Handshake {
            protocol_version: 0,
            features: if capable {
                vec![eidetica::service::protocol::FEATURE_HISTORY_PAGES.into()]
            } else {
                Vec::new()
            },
        },
    )
    .await
    .unwrap();
    let ack: HandshakeAck = read_frame(&mut reader).await.unwrap().unwrap();
    assert_eq!(!ack.features.is_empty(), capable);
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginAccount {
            username: "admin".into(),
        },
    )
    .await
    .unwrap();
    let challenge = match read_response(&mut reader).await {
        ServiceResponse::TrustedLoginChallenge { challenge, .. } => challenge,
        other => panic!("unexpected login response: {other:?}"),
    };
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: create_challenge_response(&challenge, private),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_response(&mut reader).await,
        ServiceResponse::TrustedLoginOk
    ));
    write_frame(
        &mut writer,
        &ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
            root_id: root,
            identity,
            op: DatabaseOp::GetEntry { id },
        })),
    )
    .await
    .unwrap();
    reader
}

/// Separate from the sub-1MiB incident regression: a >65MiB logical Table with
/// individually fitting Entries, cold cache-disabled rebuild and one-row update.
#[tokio::test]
async fn native_history_service_large_table_rebuild_and_one_row_update() {
    use eidetica::backend::DerivedCachePolicy;
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    use eidetica::backend::database::SqlxBackend;
    let backend = crate::helpers::test_backend().await;
    let policy = DerivedCachePolicy {
        max_bytes: 0,
        max_namespaces: 1024,
    };
    if let Some(memory) = backend.as_any().downcast_ref::<InMemory>() {
        memory.testing_set_derived_cache_policy(policy);
    }
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    if let Some(sql) = backend.as_any().downcast_ref::<SqlxBackend>() {
        sql.testing_set_derived_cache_policy(policy);
    }
    let (instance, mut admin) =
        Instance::create_backend(backend, NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    const ROW_BYTES: usize = 512 * 1024;
    const ROWS: usize = 136;
    for batch in 0..8 {
        db.with_transaction(|tx| async move {
            let rows = tx.get_store::<Table<String>>("large-rows").await?;
            for offset in 0..17 {
                rows.set(
                    format!("row{:03}", batch * 17 + offset),
                    "x".repeat(ROW_BYTES),
                )
                .await?;
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("table.sock");
    let server = ServiceServer::bind(instance.clone(), &socket)
        .await
        .unwrap();
    let (shutdown, rx) = watch::channel(());
    let task = tokio::spawn(server.run(rx));
    let client = Instance::connect(format!("unix://{}", socket.display()))
        .await
        .unwrap();
    let user = client.login_account("admin", None).await.unwrap();
    let remote = user.open_database(db.root_id()).await.unwrap();
    let table = remote
        .get_store_viewer::<Table<String>>("large-rows")
        .await
        .unwrap();
    // One complete logical scan verifies every row without needlessly
    // rebuilding a cache-disabled whole history once per tiny output page.
    let page = table.scan_page(None, ROWS).await.unwrap();
    assert!(page.next.is_none());
    let count = page.rows.len();
    let bytes = page.rows.iter().map(|(_, v)| v.len()).sum::<usize>();
    assert_eq!((count, bytes), (ROWS, ROWS * ROW_BYTES));
    assert!(
        bytes > 65 * 1024 * 1024,
        "measured Table payload did not exceed65MiB"
    );
    eprintln!("LARGE TABLE cold rebuild complete: rows={count} logical_bytes={bytes}");
    drop(page);
    drop(table);
    remote
        .with_transaction(|tx| async move {
            tx.get_store::<Table<String>>("large-rows")
                .await?
                .set("row000", "updated".into())
                .await?;
            Ok(())
        })
        .await
        .unwrap();
    let updated = remote
        .get_store_viewer::<Table<String>>("large-rows")
        .await
        .unwrap();
    let page = updated.scan_page(None, ROWS).await.unwrap();
    assert!(page.next.is_none());
    assert_eq!(page.rows.len(), ROWS);
    assert_eq!(page.rows[0], ("row000".into(), "updated".into()));
    assert!(page.rows[1..].iter().all(|(_, v)| v.len() == ROW_BYTES));
    eprintln!(
        "LARGE TABLE PROOF rows={count} logical_bytes={bytes} cold_cache_budget=0 one_row_update=verified backend={}",
        std::env::var("TEST_BACKEND").unwrap_or_else(|_| "inmemory".into())
    );
    drop(shutdown);
    task.await.unwrap().unwrap();
}
