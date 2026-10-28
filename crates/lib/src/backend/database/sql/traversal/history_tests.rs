//! Actual native-engine conformance for the bounded metadata walk.
use super::*;
use crate::{
    Snapshot,
    backend::{
        BackendImpl, HistoryPageLimits,
        history_page::{HistoryQuery, TopK, Visits, Workspace},
    },
};
use futures_util::TryStreamExt;
use sqlx::Row;
use std::{sync::Arc, time::Duration};

fn limits() -> HistoryPageLimits {
    HistoryPageLimits {
        max_entries: 4,
        max_encoded_bytes: 1024 * 1024,
        allow_large_singleton: true,
    }
}
async fn backend() -> Arc<SqlxBackend> {
    concurrent_snapshot_test_backend().await
}
async fn graph(b: &SqlxBackend) -> (ID, Snapshot) {
    graph_length(b, 12).await
}
async fn graph_length(b: &SqlxBackend, count: u64) -> (ID, Snapshot) {
    let root = Entry::root_builder()
        .set_subtree_data("h", "root")
        .build()
        .unwrap();
    let tree = root.id();
    b.put(root).await.unwrap();
    let mut parent = tree.clone();
    for n in 1..=count {
        let entry = Entry::builder(tree.clone())
            .add_parent(parent.clone())
            .set_subtree_data("h", n.to_string())
            .add_subtree_parent("h", parent)
            .set_subtree_height("h", Some(n))
            .build()
            .unwrap();
        parent = entry.id();
        b.put(entry).await.unwrap();
    }
    (tree, Snapshot::new(vec![parent]))
}
async fn all(b: &SqlxBackend, tree: &ID, tips: &Snapshot) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut after = None;
    loop {
        let page = b
            .store_history_page(tree, "h", tips, after.as_ref(), limits())
            .await
            .unwrap();
        entries.extend(page.entries);
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => return entries,
        }
    }
}

#[tokio::test]
async fn native_history_metadata_capacity_corruption_and_missing_edges() {
    let b = backend().await;
    let (tree, tips) = graph(&b).await;
    let legacy = b.store_at(&tree, "h", &tips).await.unwrap();
    assert_eq!(all(&b, &tree, &tips).await, legacy);
    let q = HistoryQuery::new(&tree, "h", &tips, None).unwrap();
    let mut overhead = Workspace::new();
    overhead
        .charge("h".len() * 4 + HISTORY_DRIVER_SLACK)
        .unwrap();
    let _ = TopK::new(&q, &limits(), &mut overhead).unwrap();
    let exact = overhead.used + legacy.len() * Visits::<ID>::COST;
    for budget in [exact - 1, exact, exact + 1] {
        let result = history_page_until(
            &b,
            &q,
            &limits(),
            tokio::time::Instant::now() + Duration::from_secs(30),
            Workspace::with_limit(budget),
        )
        .await;
        assert_eq!(
            result.is_ok(),
            budget >= exact,
            "budget {budget}: {result:?}"
        );
    }
    // Starts require tree membership. Following edges must NOT impose that
    // predicate again, nor stop at a missing intermediary's retained edges.
    let foreign = legacy[4].id();
    sqlx::query("UPDATE entries SET tree_id=$1 WHERE id=$2")
        .bind(ID::from_bytes("foreign metadata tree").to_string())
        .bind(foreign.to_string())
        .execute(b.pool())
        .await
        .unwrap();
    assert_eq!(
        all(&b, &tree, &tips).await,
        legacy,
        "SQL final selection must not add the start's tree predicate"
    );
    let foreign_start = Snapshot::new(vec![foreign.clone()]);
    assert!(
        all(&b, &tree, &foreign_start).await.is_empty(),
        "SQL starts must keep their tree predicate"
    );
    sqlx::query("UPDATE entries SET tree_id=$1 WHERE id=$2")
        .bind(tree.to_string())
        .bind(foreign.to_string())
        .execute(b.pool())
        .await
        .unwrap();
    let missing = legacy[5].id();
    sqlx::query("DELETE FROM entries WHERE id = $1")
        .bind(missing.to_string())
        .execute(b.pool())
        .await
        .unwrap();
    let legacy_missing = b.store_at(&tree, "h", &tips).await.unwrap();
    assert_eq!(legacy_missing.len(), legacy.len() - 1);
    assert_eq!(all(&b, &tree, &tips).await, legacy_missing);
    // Add a cycle in metadata: visited dedup terminates, without output pruning.
    sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
        .bind("h")
        .bind(tree.to_string())
        .bind(tips.tips()[0].to_string())
        .execute(b.pool())
        .await
        .unwrap();
    assert_eq!(all(&b, &tree, &tips).await, legacy_missing);
    for bad in [
        "!not-a-cid".to_string(),
        "x".repeat(HISTORY_ID_TEXT_BYTES + 1),
        String::new(),
    ] {
        sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
            .bind("h")
            .bind(tips.tips()[0].to_string())
            .bind(&bad)
            .execute(b.pool())
            .await
            .unwrap();
        assert!(
            b.store_history_page(&tree, "h", &tips, None, limits())
                .await
                .is_err(),
            "bad ID accepted"
        );
        sqlx::query("DELETE FROM store_parents WHERE store_name=$1 AND parent_id=$2")
            .bind("h")
            .bind(&bad)
            .execute(b.pool())
            .await
            .unwrap();
    }
    let replacement = Entry::root_builder()
        .set_subtree_data("h", "different immutable body")
        .build()
        .unwrap();
    sqlx::query("UPDATE entries SET entry_cbor=$1 WHERE id=$2")
        .bind(serde_ipld_dagcbor::to_vec(&replacement).unwrap())
        .bind(tree.to_string())
        .execute(b.pool())
        .await
        .unwrap();
    let mismatch = b
        .store_history_page(&tree, "h", &tips, None, limits())
        .await
        .unwrap_err();
    assert!(
        matches!(mismatch,crate::Error::Backend(ref e) if matches!(**e,BackendError::EntryValidationFailed{..})),
        "{mismatch:?}"
    );
    // A corrupt immutable body can never be returned as a successful page.
    sqlx::query("UPDATE entries SET entry_cbor=$1 WHERE id=$2")
        .bind(vec![255u8])
        .bind(tree.to_string())
        .execute(b.pool())
        .await
        .unwrap();
    assert!(
        b.store_history_page(&tree, "h", &tips, None, limits())
            .await
            .is_err()
    );
    eprintln!(
        "NATIVE HISTORY {:?}: exact workspace={exact}; minus-one refusal, duplicate/cycle/missing traversal and corrupt ID/body controls passed",
        b.kind
    );
}

#[tokio::test]
async fn native_history_simple_plans_and_raw_id_guard() {
    let b = backend().await;
    let (tree, _) = graph(&b).await;
    let parent_sql = if b.kind == super::super::DbKind::Sqlite {
        HISTORY_PARENTS_SQLITE
    } else {
        HISTORY_PARENTS_POSTGRES
    };
    if b.kind == super::super::DbKind::Sqlite {
        let sql = format!("EXPLAIN QUERY PLAN {parent_sql}");
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind("h")
            .bind(tree.to_string())
            .bind(256i64)
            .bind(100i64)
            .fetch_all(b.pool())
            .await
            .unwrap();
        let details: Vec<String> = rows.iter().map(|row| row.get(3)).collect();
        assert!(
            details
                .iter()
                .any(|s| s.contains("idx_store_parents_child")),
            "{details:?}"
        );
        let vm = format!("EXPLAIN {parent_sql}");
        let rows = sqlx::query(sqlx::AssertSqlSafe(vm))
            .bind("h")
            .bind(tree.to_string())
            .bind(256i64)
            .bind(100i64)
            .fetch_all(b.pool())
            .await
            .unwrap();
        let ops: Vec<String> = rows.iter().map(|row| row.get(1)).collect();
        assert!(
            !ops.iter()
                .any(|op| ["OpenEphemeral", "SorterOpen", "Sort", "SorterInsert"]
                    .contains(&op.as_str())),
            "{ops:?}"
        );
        eprintln!("SQLITE HISTORY PLAN {details:?}; VM {ops:?}");
    } else {
        let mut tx = b.pool().begin().await.unwrap();
        sqlx::query("SET LOCAL enable_bitmapscan=off")
            .execute(&mut *tx)
            .await
            .unwrap();
        let sql = format!("EXPLAIN {parent_sql}");
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind("h")
            .bind(tree.to_string())
            .bind(256i64)
            .bind(100i64)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        let details: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
        assert!(
            !details.iter().any(
                |line| ["Recursive", "Hash", "Sort", "Materialize", "Bitmap"]
                    .iter()
                    .any(|op| line.contains(op))
            ),
            "{details:?}"
        );
        eprintln!("POSTGRES HISTORY PLAN {details:?}");
        tx.rollback().await.unwrap();
    }
    let raw = sqlx::query(parent_sql)
        .bind("h")
        .bind(tree.to_string())
        .bind(256i64)
        .bind(100i64)
        .fetch_all(b.pool())
        .await
        .unwrap();
    // Account both native and Any wrappers, two fixed result columns, raw
    // and converted bounded text, container slack and the 50-row channel.
    // Column metadata is shared; include conservative fixed ownership too.
    let row_headers = std::mem::size_of::<sqlx::any::AnyRow>()
        + 4 * std::mem::size_of::<<sqlx::Any as sqlx::Database>::Column>()
        + 4 * std::mem::size_of::<sqlx::any::AnyValue>();
    #[cfg(feature = "sqlite")]
    let row_headers = row_headers
        + std::mem::size_of::<sqlx::sqlite::SqliteRow>()
        + 4 * std::mem::size_of::<sqlx::sqlite::SqliteValue>();
    #[cfg(feature = "postgres")]
    let row_headers = row_headers + std::mem::size_of::<sqlx::postgres::PgRow>();
    let conservative = 64 * (row_headers + 4 * HISTORY_ID_TEXT_BYTES + 512) * 2;
    assert!(
        HISTORY_DRIVER_SLACK >= conservative,
        "driver slack {HISTORY_DRIVER_SLACK} < {conservative}"
    );
    assert!(
        raw.iter()
            .all(|r| r.columns.len() == 2 && r.values.len() == 2)
    );
    eprintln!(
        "NATIVE ROW ACCOUNTING: headers={row_headers}, 64-slot conservative={conservative}, charged={HISTORY_DRIVER_SLACK}"
    );
    let (length,): (i64,) = sqlx::query_as("SELECT CAST(octet_length($1) AS BIGINT)")
        .bind("é".repeat(128))
        .fetch_one(b.pool())
        .await
        .unwrap();
    assert_eq!(length, 256, "guard must use bytes, not characters");
    // Raw guarded text cannot allocate the oversized input in a driver row.
    sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
        .bind("raw")
        .bind(tree.to_string())
        .bind("é".repeat(129))
        .execute(b.pool())
        .await
        .unwrap();
    let row: (Option<String>, Option<i64>) = sqlx::query_as(parent_sql)
        .bind("raw")
        .bind(tree.to_string())
        .bind(256i64)
        .bind(100i64)
        .fetch_one(b.pool())
        .await
        .unwrap();
    assert_eq!(row, (None, Some(258)));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn native_history_sqlite_stream_cancel_preserves_inmemory_pool() {
    let b = SqlxBackend::sqlite_in_memory().await.unwrap();
    let (tree, _) = graph(&b).await;
    // Enough rows to fill the bounded 50-row driver channel, all synthetic.
    for n in 0u32..256 {
        sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
            .bind("fan")
            .bind(tree.to_string())
            .bind(ID::from_bytes(n.to_le_bytes()).to_string())
            .execute(b.pool())
            .await
            .unwrap();
    }
    let mut tx = b.pool().begin().await.unwrap();
    let mut rows = sqlx::query_as::<_, (Option<String>, Option<i64>)>(HISTORY_PARENTS_SQLITE)
        .bind("fan")
        .bind(tree.to_string())
        .bind(256i64)
        .bind(1000i64)
        .fetch(&mut *tx);
    assert!(rows.try_next().await.unwrap().is_some());
    drop(rows);
    drop(tx);
    let (count,): (i64,) = tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::query_as("SELECT COUNT(*) FROM entries").fetch_one(b.pool()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(count, 13, "last in-memory connection was closed");
    let (busy,): (i64,) = sqlx::query_as("PRAGMA busy_timeout")
        .fetch_one(b.pool())
        .await
        .unwrap();
    assert_eq!(busy, 5000);
    assert_eq!(b.pool().size(), 1);
    eprintln!(
        "SQLITE NATIVE CANCEL: active indexed stream dropped; one-slot pool reused, all 13 Entries retained, busy_timeout=5000"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn native_history_postgres_timeout_cancel_and_local_cleanup() {
    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let b = backend().await;
    let (superuser,): (bool,) =
        sqlx::query_as("SELECT rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(b.pool())
            .await
            .unwrap();
    if std::env::var_os("HISTORY_REQUIRE_UNPRIVILEGED").is_some() {
        assert!(
            !superuser,
            "native controls must run without privileged role"
        );
    }
    eprintln!("POSTGRES fixture superuser={superuser}");
    let mut tx = b
        .pool()
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await
        .unwrap();
    sqlx::query("SET LOCAL work_mem='64kB'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SET LOCAL enable_bitmapscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    history_statement_deadline(
        &mut tx,
        b.kind,
        tokio::time::Instant::now() + Duration::from_millis(100),
    )
    .await
    .unwrap();
    let row: (String,String,String) = sqlx::query_as("SELECT current_setting('work_mem'),current_setting('enable_bitmapscan'),current_setting('statement_timeout')").fetch_one(&mut *tx).await.unwrap();
    assert_eq!((&*row.0, &*row.1), ("64kB", "off"));
    assert_ne!(row.2, "0");
    let error = sqlx::query("SELECT pg_sleep(2)")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("57014")
    );
    tx.rollback().await.unwrap();
    let mut held = Vec::new();
    for _ in 1..b.pool().options().get_max_connections() {
        held.push(b.pool().acquire().await.unwrap());
    }
    let mut tx = b.pool().begin().await.unwrap();
    let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    history_statement_deadline(
        &mut tx,
        b.kind,
        tokio::time::Instant::now() + Duration::from_millis(250),
    )
    .await
    .unwrap();
    let mut sleep = Box::pin(sqlx::query("SELECT pg_sleep(2)").execute(&mut *tx));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut sleep)
            .await
            .is_err()
    );
    drop(sleep);
    drop(tx);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), b.pool().acquire())
            .await
            .is_err(),
        "pool replenished ahead of still-running native work"
    );
    let mut reused = tokio::time::timeout(Duration::from_secs(2), b.pool().acquire())
        .await
        .unwrap()
        .unwrap();
    let (active,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM pg_stat_activity WHERE pid=$1 AND state='active'")
            .bind(pid)
            .fetch_one(&mut *reused)
            .await
            .unwrap();
    assert_eq!(active, 0);
    let row: (String,String,String) = sqlx::query_as("SELECT current_setting('work_mem'),current_setting('enable_bitmapscan'),current_setting('statement_timeout')").fetch_one(&mut *reused).await.unwrap();
    assert_eq!(row.2, "0");
    assert_ne!(row.0, "64kB");
    assert_eq!(row.1, "on");
    assert!(
        history_statement_deadline(&mut reused, b.kind, tokio::time::Instant::now())
            .await
            .is_err(),
        "expired native deadline must never bind zero"
    );
    eprintln!(
        "POSTGRES NATIVE CANCEL: local settings={row:?}; SQLSTATE57014; old pid={pid} idle-or-gone; one remaining pool slot withheld then safely reused"
    );
}

#[tokio::test]
async fn native_history_tiny_capture_beside_unrelated_history_and_parent_guard() {
    let b = backend().await;
    let root = Entry::root_builder()
        .set_subtree_data("h", "tiny")
        .build()
        .unwrap();
    let tree = root.id();
    b.put(root.clone()).await.unwrap();
    // A different root with more nodes than the tiny captured query's entire
    // supported workspace. No global row-count preflight may reject the tiny read.
    let other = ID::from_bytes("unrelated");
    for n in 0u32..256 {
        sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
            .bind("elsewhere")
            .bind(other.to_string())
            .bind(ID::from_bytes(n.to_le_bytes()).to_string())
            .execute(b.pool())
            .await
            .unwrap();
    }
    let tips = Snapshot::new(vec![tree.clone()]);
    let q = HistoryQuery::new(&tree, "h", &tips, None).unwrap();
    let mut overhead = Workspace::new();
    overhead.charge(HISTORY_DRIVER_SLACK + 4).unwrap();
    let _ = TopK::new(&q, &limits(), &mut overhead).unwrap();
    let budget = overhead.used + Visits::<ID>::COST;
    let page = history_page_until(
        &b,
        &q,
        &limits(),
        tokio::time::Instant::now() + Duration::from_secs(30),
        Workspace::with_limit(budget),
    )
    .await
    .unwrap();
    assert_eq!(page.entries, vec![root]);
    assert!(page.next.is_none());
    // The very same indexed query limits rows to V+1, not remaining slots+1.
    let sql = if b.kind == super::super::DbKind::Sqlite {
        HISTORY_PARENTS_SQLITE
    } else {
        HISTORY_PARENTS_POSTGRES
    };
    let rows: Vec<(Option<String>, Option<i64>)> = sqlx::query_as(sql)
        .bind("elsewhere")
        .bind(other.to_string())
        .bind(256i64)
        .bind(2i64)
        .fetch_all(b.pool())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "V+1 parent guard must remain SQL-side");
    sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
        .bind("h")
        .bind(tree.to_string())
        .bind(other.to_string())
        .execute(b.pool())
        .await
        .unwrap();
    assert!(
        history_page_until(
            &b,
            &q,
            &limits(),
            tokio::time::Instant::now() + Duration::from_secs(30),
            Workspace::with_limit(budget)
        )
        .await
        .is_err(),
        "resource exhaustion returned a partial successful page"
    );
}

#[tokio::test]
async fn native_history_multi_lookup_transaction_snapshot() {
    #[cfg(feature = "sqlite")]
    let dir = tempfile::tempdir().unwrap();
    let b = if std::env::var("TEST_BACKEND").as_deref() == Ok("postgres") {
        backend().await
    } else {
        #[cfg(feature = "sqlite")]
        {
            Arc::new(
                SqlxBackend::open_sqlite(dir.path().join("snapshot.db"))
                    .await
                    .unwrap(),
            )
        }
        #[cfg(not(feature = "sqlite"))]
        {
            return;
        }
    };
    let (tree, tips) = graph(&b).await;
    let expected = b.store_at(&tree, "h", &tips).await.unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = tokio::sync::oneshot::channel();
    *HISTORY_PAUSE.get_or_init(Default::default).lock().unwrap() =
        Some((tree.clone(), (entered, resume_rx)));
    let reader = b.clone();
    let rt = tree.clone();
    let rs = tips.clone();
    let task = tokio::spawn(async move {
        reader
            .store_history_page(&rt, "h", &rs, None, limits())
            .await
            .unwrap()
    });
    entered_rx.await.unwrap();
    // Destroy the newest tip's edge after the first metadata lookup established
    // the read snapshot. First page must see the old edge, later RPCs are fresh.
    sqlx::query("DELETE FROM store_parents WHERE store_name=$1 AND child_id=$2")
        .bind("h")
        .bind(tips.tips()[0].to_string())
        .execute(b.pool())
        .await
        .unwrap();
    resume.send(()).unwrap();
    let result = task.await.unwrap();
    assert_eq!(result.entries, expected[..limits().max_entries]);
    assert!(
        b.store_history_page(&tree, "h", &tips, result.next.as_ref(), limits())
            .await
            .is_err(),
        "removed metadata must not invent continuation success"
    );
    // We deliberately make no cross-page snapshot promise for corrupt/missing
    // metadata. A direct next RPC sees the new state, not a lifetime lease.
    let fresh = b
        .store_history_page(&tree, "h", &tips, None, limits())
        .await
        .unwrap();
    assert_eq!(fresh.entries.len(), 1);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn native_history_sqlite_busy_cancel_retains_pool_ownership() {
    use sqlx::Connection;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("locked.db");
    let b = SqlxBackend::open_sqlite(&file).await.unwrap();
    let (tree, _) = graph(&b).await;
    let mut held = Vec::new();
    for _ in 1..b.pool().options().get_max_connections() {
        held.push(b.pool().acquire().await.unwrap());
    }
    let mut lock = sqlx::AnyConnection::connect(&format!("sqlite://{}", file.display()))
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut lock)
        .await
        .unwrap();
    let mut connection = b.pool().acquire().await.unwrap();
    let start = std::time::Instant::now();
    // WAL reads need not wait for a writer. A bounded native write is the
    // positive control for this pool's unchanged busy_timeout; active history
    // stream cancellation is checked separately without changing journal mode.
    let mut query = Box::pin(
        sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
            .bind("lock-control")
            .bind(tree.to_string())
            .bind(tree.to_string())
            .execute(&mut *connection),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut query)
            .await
            .is_err()
    );
    drop(query);
    drop(connection);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), b.pool().acquire())
            .await
            .is_err(),
        "busy native work released its pool slot early"
    );
    // Keep the lock until the real 5000ms native busy refusal drains.
    let mut reused = tokio::time::timeout(Duration::from_secs(7), b.pool().acquire())
        .await
        .unwrap()
        .unwrap();
    assert!(start.elapsed() >= Duration::from_secs(4));
    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM entries")
        .fetch_one(&mut *reused)
        .await
        .unwrap();
    assert_eq!(count, 13);
    let (busy,): (i64,) = sqlx::query_as("PRAGMA busy_timeout")
        .fetch_one(&mut *reused)
        .await
        .unwrap();
    assert_eq!(busy, 5000);
    eprintln!(
        "SQLITE LOCK CANCEL: native busy_timeout=5000 withheld one available pool slot for {:?}; data retained",
        start.elapsed()
    );
}

#[tokio::test]
async fn native_history_raw_body_guard_precedes_driver_copy() {
    use crate::backend::history_page::MAX_ENTRY_RAW_BYTES;
    let b = backend().await;
    let entry = Entry::root_builder()
        .set_subtree_data("h", "guard")
        .build()
        .unwrap();
    let tree = entry.id();
    b.put(entry).await.unwrap();
    let oversized = (MAX_ENTRY_RAW_BYTES + 1) as i64;
    if b.kind == super::super::DbKind::Sqlite {
        sqlx::query("UPDATE entries SET entry_cbor=zeroblob($1) WHERE id=$2")
            .bind(oversized)
            .bind(tree.to_string())
            .execute(b.pool())
            .await
            .unwrap();
    } else {
        sqlx::query("UPDATE entries SET entry_cbor=decode(repeat('00',CAST($1 AS INTEGER)), 'hex') WHERE id=$2").bind(oversized).bind(tree.to_string()).execute(b.pool()).await.unwrap();
    }
    let tips = Snapshot::new(vec![tree.clone()]);
    let error = b
        .store_history_page(&tree, "h", &tips, None, limits())
        .await
        .unwrap_err();
    assert!(
        matches!(error,crate::Error::Backend(ref e) if matches!(**e,BackendError::EntryTooLarge{..})),
        "{error:?}"
    );
    let row:(Option<Vec<u8>>,i64)=sqlx::query_as("SELECT CASE WHEN length(entry_cbor)<=$2 THEN entry_cbor ELSE NULL END,CAST(length(entry_cbor) AS BIGINT) FROM entries WHERE id=$1").bind(tree.to_string()).bind(MAX_ENTRY_RAW_BYTES as i64).fetch_one(b.pool()).await.unwrap();
    assert!(row.0.is_none());
    assert_eq!(row.1, oversized);
}

#[tokio::test]
async fn native_history_high_fanout_missing_plans_and_v_plus_one_refusal() {
    let b = backend().await;
    let (tree, tips) = graph(&b).await;
    for n in 0u32..1024 {
        sqlx::query("INSERT INTO store_parents(store_name,child_id,parent_id) VALUES($1,$2,$3)")
            .bind("h")
            .bind(tips.tips()[0].to_string())
            .bind(ID::from_bytes(n.to_le_bytes()).to_string())
            .execute(b.pool())
            .await
            .unwrap();
    }
    let q = HistoryQuery::new(&tree, "h", &tips, None).unwrap();
    let mut workspace = Workspace::new();
    workspace.charge(HISTORY_DRIVER_SLACK + 4).unwrap();
    let _ = TopK::new(&q, &limits(), &mut workspace).unwrap();
    let cap = workspace.used + 3 * Visits::<ID>::COST;
    assert!(
        history_page_until(
            &b,
            &q,
            &limits(),
            tokio::time::Instant::now() + Duration::from_secs(30),
            Workspace::with_limit(cap)
        )
        .await
        .is_err(),
        "V+1 distinct parents returned a partial successful page"
    );
    let sql = if b.kind == super::super::DbKind::Sqlite {
        HISTORY_PARENTS_SQLITE
    } else {
        HISTORY_PARENTS_POSTGRES
    };
    let mut tx = b.pool().begin().await.unwrap();
    if b.kind == super::super::DbKind::Postgres {
        sqlx::query("SET LOCAL enable_bitmapscan=off")
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    for child in [tips.tips()[0].clone(), ID::from_bytes("missing key")] {
        let prefix = if b.kind == super::super::DbKind::Sqlite {
            "EXPLAIN QUERY PLAN"
        } else {
            "EXPLAIN"
        };
        let plan = format!("{prefix} {sql}");
        let rows = sqlx::query(sqlx::AssertSqlSafe(plan))
            .bind("h")
            .bind(child.to_string())
            .bind(256i64)
            .bind(4i64)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        let column = if b.kind == super::super::DbKind::Sqlite {
            3
        } else {
            0
        };
        let plan: Vec<String> = rows.iter().map(|r| r.get(column)).collect();
        assert!(
            !plan.iter().any(|s| [
                "Recursive",
                "Hash",
                "Sort",
                "Materialize",
                "Bitmap",
                "TEMP B-TREE"
            ]
            .iter()
            .any(|op| s.contains(op))),
            "{plan:?}"
        );
        if b.kind == super::super::DbKind::Sqlite {
            assert!(plan.iter().any(|s| s.contains("idx_store_parents_child")));
        }
        eprintln!("NATIVE FANOUT/MISSING {:?}: {plan:?}", b.kind);
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn native_history_delta_capacity_strictness_deadline_and_cleanup() {
    use crate::backend::history_delta::DeltaQuery;
    let b = backend().await;
    let (tree, target) = graph(&b).await;
    let legacy = b.store_at(&tree, "h", &target).await.unwrap();
    let known = Snapshot::from([legacy[6].id()]);
    let q = DeltaQuery::new(&tree, "h", &target, &known).unwrap();
    let run = |budget| {
        history_delta_until(
            &b,
            &q,
            limits(),
            tokio::time::Instant::now() + Duration::from_secs(30),
            Workspace::with_limit(budget),
        )
    };
    let mut low = 0;
    let mut high = crate::backend::history_page::MAX_HISTORY_WORKSPACE_BYTES;
    while low < high {
        let mid = (low + high) / 2;
        if run(mid).await.is_ok() {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    assert!(run(low - 1).await.is_err());
    let page = run(low).await.unwrap();
    assert_eq!(page.entries, legacy[7..11]);
    let mut v1 = Workspace::new();
    v1.charge(4 + HISTORY_DRIVER_SLACK).unwrap();
    let hq = HistoryQuery::new(&tree, "h", &target, None).unwrap();
    let _ = TopK::new(&hq, &limits(), &mut v1).unwrap();
    let v1_min = v1.used + legacy.len() * Visits::<ID>::COST;
    eprintln!(
        "NATIVE DELTA CAPACITY: target_nodes={} known_nodes=7 v1_accounted_bytes={} v2_min_accounted_bytes={} added_bytes={} backend={:?}",
        legacy.len(),
        v1_min,
        low,
        low - v1_min,
        b.kind
    );
    assert!(
        history_delta_until(
            &b,
            &q,
            limits(),
            tokio::time::Instant::now(),
            Workspace::new()
        )
        .await
        .is_err()
    );
    assert!(
        b.store_history_delta(&tree, "h", &target, &known, limits())
            .await
            .is_ok()
    );
    // Strict v2 rejects observable metadata scope corruption that v1 permits.
    sqlx::query("UPDATE entries SET tree_id=$1 WHERE id=$2")
        .bind(ID::from_bytes("foreign").to_string())
        .bind(legacy[3].id().to_string())
        .execute(b.pool())
        .await
        .unwrap();
    assert!(
        b.store_history_delta(&tree, "h", &target, &known, limits())
            .await
            .is_err()
    );
    assert!(
        b.store_history_page(&tree, "h", &target, None, limits())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn native_history_delta_captured_transaction_and_cancel_reuse() {
    #[cfg(feature = "sqlite")]
    let dir = tempfile::tempdir().unwrap();
    let b = if std::env::var("TEST_BACKEND").as_deref() == Ok("postgres") {
        backend().await
    } else {
        #[cfg(feature = "sqlite")]
        {
            Arc::new(
                SqlxBackend::open_sqlite(dir.path().join("delta-snapshot.db"))
                    .await
                    .unwrap(),
            )
        }
        #[cfg(not(feature = "sqlite"))]
        {
            return;
        }
    };
    let (tree, target) = graph(&b).await;
    let expected = b.store_at(&tree, "h", &target).await.unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = tokio::sync::oneshot::channel();
    *HISTORY_PAUSE.get_or_init(Default::default).lock().unwrap() =
        Some((tree.clone(), (entered, resume_rx)));
    let reader = b.clone();
    let rt = tree.clone();
    let rs = target.clone();
    let task = tokio::spawn(async move {
        reader
            .store_history_delta(&rt, "h", &rs, &Snapshot::EMPTY, limits())
            .await
            .unwrap()
    });
    entered_rx.await.unwrap();
    sqlx::query("DELETE FROM store_parents WHERE store_name=$1 AND child_id=$2")
        .bind("h")
        .bind(target.tips()[0].to_string())
        .execute(b.pool())
        .await
        .unwrap();
    resume.send(()).unwrap();
    assert_eq!(task.await.unwrap().entries, expected[..4]);
    // Fresh page sees new metadata and body inconsistency, not a lifetime lease.
    assert!(
        b.store_history_delta(&tree, "h", &target, &Snapshot::EMPTY, limits())
            .await
            .is_err()
    );
    // Hold all but one pool slot and cancel a real v2 operation after its read.
    let mut held = Vec::new();
    for _ in 1..b.pool().options().get_max_connections() {
        held.push(b.pool().acquire().await.unwrap());
    }
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (_resume, resume_rx) = tokio::sync::oneshot::channel();
    *HISTORY_PAUSE.get_or_init(Default::default).lock().unwrap() =
        Some((tree.clone(), (entered, resume_rx)));
    let reader = b.clone();
    let rt = tree.clone();
    let rs = target.clone();
    let task = tokio::spawn(async move {
        reader
            .store_history_delta(&rt, "h", &rs, &Snapshot::EMPTY, limits())
            .await
    });
    entered_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), b.pool().acquire())
            .await
            .is_err()
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let mut reused = tokio::time::timeout(Duration::from_secs(5), b.pool().acquire())
        .await
        .unwrap()
        .unwrap();
    let (one,): (i64,) = sqlx::query_as("SELECT CAST(1 AS BIGINT)")
        .fetch_one(&mut *reused)
        .await
        .unwrap();
    assert_eq!(one, 1);
    if b.kind == super::super::DbKind::Postgres {
        let (timeout,): (String,) = sqlx::query_as("SELECT current_setting('statement_timeout')")
            .fetch_one(&mut *reused)
            .await
            .unwrap();
        assert_eq!(timeout, "0");
    }
}

#[tokio::test]
async fn native_history_delta_representative_metadata_cost() {
    use crate::backend::history_delta::DeltaQuery;
    let b = backend().await;
    // Same node count as the signed many-sub-1MiB Entry transfer fixture.
    // Body sizes do not affect metadata admission; no RSS claim is made.
    let (tree, target) = graph_length(&b, 95).await;
    let entries = b.store_at(&tree, "h", &target).await.unwrap();
    let known = Snapshot::from([entries[94].id()]);
    let q = DeltaQuery::new(&tree, "h", &target, &known).unwrap();
    let mut low = 0;
    let mut high = crate::backend::history_page::MAX_HISTORY_WORKSPACE_BYTES;
    while low < high {
        let mid = (low + high) / 2;
        let result = history_delta_until(
            &b,
            &q,
            limits(),
            tokio::time::Instant::now() + Duration::from_secs(30),
            Workspace::with_limit(mid),
        )
        .await;
        if result.is_ok() {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    let page = history_delta_until(
        &b,
        &q,
        limits(),
        tokio::time::Instant::now() + Duration::from_secs(30),
        Workspace::with_limit(low),
    )
    .await
    .unwrap();
    assert!(page.complete);
    assert_eq!(page.entries, entries[95..]);
    let mut v1 = Workspace::new();
    v1.charge(4 + HISTORY_DRIVER_SLACK).unwrap();
    let hq = HistoryQuery::new(&tree, "h", &target, None).unwrap();
    let _ = TopK::new(&hq, &limits(), &mut v1).unwrap();
    let v1_min = v1.used + entries.len() * Visits::<ID>::COST;
    assert!(low > v1_min && low < crate::backend::history_page::MAX_HISTORY_WORKSPACE_BYTES);
    eprintln!(
        "REPRESENTATIVE METADATA target_nodes=96 known_nodes=95 v1_accounted_bytes={v1_min} v2_min_accounted_bytes={low} extra_accounted_bytes={} (not RSS)",
        low - v1_min
    );
}

#[tokio::test]
async fn native_history_delta_eight_nonqueued_operations_release_on_cancel() {
    let b = backend().await;
    let (tree, target) = graph(&b).await;
    let empty = Snapshot::EMPTY;
    let mut held = Vec::new();
    for _ in 0..b.pool().options().get_max_connections() {
        held.push(b.pool().acquire().await.unwrap());
    }
    let mut calls = Vec::new();
    for _ in 0..8 {
        let mut call = Box::pin(b.store_history_delta(&tree, "h", &target, &empty, limits()));
        assert!(futures_util::poll!(call.as_mut()).is_pending());
        calls.push(call);
    }
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        b.store_history_delta(&tree, "h", &target, &empty, limits()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        matches!(error,crate::Error::Backend(e) if matches!(*e,BackendError::HistoryResourceLimit{..}))
    );
    drop(calls.pop());
    let mut replacement = Box::pin(b.store_history_delta(&tree, "h", &target, &empty, limits()));
    assert!(futures_util::poll!(replacement.as_mut()).is_pending());
    drop(replacement);
    drop(calls);
    drop(held);
    assert!(
        tokio::time::timeout(
            Duration::from_secs(5),
            b.store_history_delta(&tree, "h", &target, &empty, limits())
        )
        .await
        .unwrap()
        .is_ok()
    );
}
