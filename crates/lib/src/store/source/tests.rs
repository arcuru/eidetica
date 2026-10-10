use super::*;
use crate::{
    Instance, NewUser,
    backend::database::InMemory,
    store::{DocStore, Registered},
};

fn limits() -> Limits {
    Limits {
        source_bytes: 64 * 1024,
        page_bytes: 16 * 1024,
        nodes: 100,
        user_bytes: 256 * 1024,
        global_bytes: 512 * 1024,
        user_contexts: 2,
        global_contexts: 3,
        source_contexts: 1,
        ..Limits::default()
    }
}
async fn fixture() -> (
    Instance,
    Arc<dyn BackendImpl>,
    crate::Database,
    StoreQueryRequest,
) {
    let (instance, mut user) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewUser::passwordless("source-test"),
    )
    .await
    .unwrap();
    let db = user
        .create_database(Doc::new(), &user.get_default_key().unwrap())
        .await
        .unwrap();
    for i in 0..8 {
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("docs")
            .await
            .unwrap()
            .set("value", format!("{i}{}", "x".repeat(800)))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let request = StoreQueryRequest {
        store: "docs".into(),
        expected_type: DocStore::type_id().into(),
        source: QuerySource {
            main: db.snapshot().await.unwrap(),
            scope: ReadScope::Verified,
        },
        query: Vec::new(),
    };
    (
        instance.clone(),
        instance.require_local_engine().unwrap(),
        db,
        request,
    )
}
fn reader(user: &str, connection: u64) -> Reader {
    Reader {
        user: user.into(),
        principal: user.into(),
        connection,
    }
}
fn admission(error: crate::Error) {
    assert!(
        matches!(error, crate::Error::Backend(e) if matches!(*e, BackendError::SourceAdmissionRefused))
    );
}

#[tokio::test]
async fn source_small_limits_bound_walk_before_eager_allocation() {
    let (_instance, engine, db, request) = fixture().await;
    let mut small = limits();
    small.nodes = 2;
    let mut walk = Walk::new(engine.as_ref(), db.root_id(), &request.source, small);
    let error = walk
        .walk(request.source.main.tips(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, crate::Error::Backend(e) if matches!(*e, BackendError::SourceTooLarge))
    );
    assert_eq!(
        walk.entries.len(),
        2,
        "refuse before fetching/retaining a third Entry"
    );
    assert!(walk.bytes <= small.source_bytes);
    let mut small = limits();
    small.source_bytes = 1000;
    let mut walk = Walk::new(engine.as_ref(), db.root_id(), &request.source, small);
    assert!(walk.walk(request.source.main.tips(), None).await.is_err());
    assert!(walk.bytes <= 1000);
}

#[test]
fn source_small_limits_admit_user_global_source_work_and_release() {
    let mut small = limits();
    small.source_bytes = 100;
    small.user_bytes = 200;
    small.global_bytes = 300;
    let sources = Arc::new(Sources::new(small, Duration::from_secs(300)));
    let a = reader("a", 1);
    let b = reader("b", 2);
    let tree = ID::from_bytes(b"tree");
    let request = StoreQueryRequest {
        store: "first".into(),
        expected_type: "binary:v0".into(),
        source: QuerySource {
            main: Snapshot::from([tree.clone()]),
            scope: ReadScope::Verified,
        },
        query: Vec::new(),
    };
    let w1 = sources.admit(&a, &tree, &request).unwrap();
    admission(sources.admit(&b, &tree, &request).err().unwrap()); // global per-source work
    let r2 = StoreQueryRequest {
        store: "second".into(),
        ..request.clone()
    };
    let w2 = sources.admit(&a, &tree, &r2).unwrap();
    let r3 = StoreQueryRequest {
        store: "third".into(),
        ..request.clone()
    };
    admission(sources.admit(&a, &tree, &r3).err().unwrap()); // per-user count/bytes
    let w3 = sources.admit(&b, &tree, &r3).unwrap();
    let r4 = StoreQueryRequest {
        store: "fourth".into(),
        ..request.clone()
    };
    admission(sources.admit(&b, &tree, &r4).err().unwrap()); // global byte reservation
    drop(w1);
    drop(w2);
    drop(w3);
    assert!(sources.state.lock().unwrap().jobs.is_empty());
    assert!(sources.admit(&b, &tree, &r4).is_ok());
    // Separately exercise the global in-flight count, independent of byte cap.
    let mut small = limits();
    small.global_jobs = 1;
    let sources = Arc::new(Sources::new(small, Duration::from_secs(300)));
    let _w1 = sources.admit(&a, &tree, &request).unwrap();
    admission(sources.admit(&b, &tree, &r2).err().unwrap());
}

#[tokio::test]
async fn source_small_limits_cursor_retry_retarget_expiry_rebind_and_release() {
    let (_instance, engine, db, query) = fixture().await;
    let sources = Arc::new(Sources::new(limits(), Duration::from_secs(300)));
    let a = reader("a", 1);
    let bound = sources
        .resolve(engine.as_ref(), &a, db.root_id(), &query)
        .await
        .unwrap();
    let raw = RawStoreRequest {
        source: bound.source.clone(),
        cursor: None,
    };
    let _work = sources.admit(&a, db.root_id(), &query).unwrap();
    let first = sources.page(engine.as_ref(), &a, &raw).await.unwrap();
    first.validate(&raw).unwrap();
    assert!(first.next.is_some());
    assert_eq!(sources.state.lock().unwrap().contexts.len(), 1);
    // Per-source retained contexts refuse duplicate contexts.
    admission(sources.page(engine.as_ref(), &a, &raw).await.unwrap_err());
    let next = RawStoreRequest {
        cursor: first.next.clone(),
        ..raw.clone()
    };
    let second = sources.page(engine.as_ref(), &a, &next).await.unwrap();
    let retry = sources.page(engine.as_ref(), &a, &next).await.unwrap();
    assert_eq!(second.entries, retry.entries);
    assert_eq!(second.next, retry.next);
    for bad in [reader("b", 1), reader("a", 2)] {
        assert!(sources.page(engine.as_ref(), &bad, &next).await.is_err());
    }
    for mutation in 0..5 {
        let mut bad = next.clone();
        match mutation {
            0 => bad.source.store = "other".into(),
            1 => bad.source.type_id = "other:v0".into(),
            2 => bad.source.source.scope = ReadScope::AllowUnverified,
            3 => bad.source.snapshot = Snapshot::EMPTY,
            _ => bad.cursor.as_mut().unwrap().offset += 2,
        }
        assert!(sources.page(engine.as_ref(), &a, &bad).await.is_err());
    }
    assert!(
        sources
            .check_source(&a, &ID::default(), &raw.source)
            .is_err()
    );
    // Current posture applies on continuation, not just at source resolution.
    let ancestor = bound.posture[0].clone();
    engine
        .update_verification_status(&ancestor, VerificationStatus::Failed)
        .await
        .unwrap();
    assert!(sources.page(engine.as_ref(), &a, &next).await.is_err());
    engine
        .update_verification_status(&ancestor, VerificationStatus::Verified)
        .await
        .unwrap();
    sources
        .state
        .lock()
        .unwrap()
        .contexts
        .values_mut()
        .for_each(|c| c.last_used -= Duration::from_secs(301));
    assert!(
        matches!(sources.page(engine.as_ref(), &a, &next).await, Err(crate::Error::Backend(e)) if matches!(*e, BackendError::InvalidRawCursor))
    );
    // A write changes latest tips but never the sealed recovery descriptor.
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("value", "new")
        .await
        .unwrap();
    write.commit().await.unwrap();
    let rebound = sources.page(engine.as_ref(), &a, &raw).await.unwrap();
    assert_eq!(rebound.entries, first.entries);
    assert_eq!(rebound.source, first.source);
    let mut request = RawStoreRequest {
        cursor: rebound.next,
        ..raw.clone()
    };
    let mut all = rebound.entries;
    while request.cursor.is_some() {
        let page = sources.page(engine.as_ref(), &a, &request).await.unwrap();
        page.validate(&request).unwrap();
        all.extend(page.entries);
        request.cursor = page.next;
    }
    assert_eq!(all, bound.entries);
    assert!(sources.state.lock().unwrap().contexts.is_empty());
    let again = sources.page(engine.as_ref(), &a, &raw).await.unwrap();
    assert!(again.next.is_some());
    sources.disconnect(1);
    assert!(sources.state.lock().unwrap().contexts.is_empty());
}

#[tokio::test]
async fn source_small_limits_context_counts_and_retained_bytes_are_enforced() {
    let (_instance, engine, db, query) = fixture().await;
    let mut small = limits();
    small.source_contexts = 8;
    small.user_contexts = 1;
    let sources = Sources::new(small, Duration::from_secs(300));
    let a = reader("a", 1);
    let b = reader("b", 2);
    let raw_a = RawStoreRequest {
        source: sources
            .resolve(engine.as_ref(), &a, db.root_id(), &query)
            .await
            .unwrap()
            .source,
        cursor: None,
    };
    let raw_b = RawStoreRequest {
        source: sources
            .resolve(engine.as_ref(), &b, db.root_id(), &query)
            .await
            .unwrap()
            .source,
        cursor: None,
    };
    sources.page(engine.as_ref(), &a, &raw_a).await.unwrap();
    admission(sources.page(engine.as_ref(), &a, &raw_a).await.unwrap_err());
    sources.page(engine.as_ref(), &b, &raw_b).await.unwrap();
    assert_eq!(sources.state.lock().unwrap().contexts.len(), 2);
    sources.disconnect(1);
    sources.disconnect(2);
    let mut small = limits();
    small.global_contexts = 1;
    let sources = Sources::new(small, Duration::from_secs(300));
    let raw_a = RawStoreRequest {
        source: sources
            .resolve(engine.as_ref(), &a, db.root_id(), &query)
            .await
            .unwrap()
            .source,
        cursor: None,
    };
    let raw_b = RawStoreRequest {
        source: sources
            .resolve(engine.as_ref(), &b, db.root_id(), &query)
            .await
            .unwrap()
            .source,
        cursor: None,
    };
    sources.page(engine.as_ref(), &a, &raw_a).await.unwrap();
    admission(sources.page(engine.as_ref(), &b, &raw_b).await.unwrap_err());
    let mut small = limits();
    small.user_bytes = 1;
    let sources = Sources::new(small, Duration::from_secs(300));
    let raw = RawStoreRequest {
        source: sources
            .resolve(engine.as_ref(), &a, db.root_id(), &query)
            .await
            .unwrap()
            .source,
        cursor: None,
    };
    admission(sources.page(engine.as_ref(), &a, &raw).await.unwrap_err());
    assert!(sources.state.lock().unwrap().contexts.is_empty());
}

#[tokio::test]
async fn source_small_limits_oversize_entry_and_envelope_are_not_empty_pages() {
    let (_instance, engine, db, query) = fixture().await;
    let a = reader("a", 1);
    let largest = engine
        .get_tree_from_tips(db.root_id(), query.source.main.tips())
        .await
        .unwrap()
        .iter()
        .map(|e| encoded_size(e, usize::MAX).unwrap())
        .max()
        .unwrap();
    let mut small = limits();
    small.page_bytes = largest + 32;
    let sources = Sources::new(small, Duration::from_secs(300));
    let raw = RawStoreRequest {
        source: sources
            .resolve(engine.as_ref(), &a, db.root_id(), &query)
            .await
            .unwrap()
            .source,
        cursor: None,
    };
    assert!(
        matches!(sources.page(engine.as_ref(), &a, &raw).await, Err(crate::Error::Backend(e)) if matches!(*e, BackendError::SourceTooLarge))
    );
    assert!(sources.state.lock().unwrap().contexts.is_empty());
    let mut small = limits();
    small.page_bytes = 100;
    let sources = Sources::new(small, Duration::from_secs(300));
    assert!(
        matches!(sources.resolve(engine.as_ref(), &a, db.root_id(), &query).await, Err(crate::Error::Backend(e)) if matches!(*e, BackendError::SourceTooLarge))
    );
}

#[test]
fn source_malformed_pages_are_rejected_before_data_use() {
    let root = Entry::root_builder()
        .set_subtree_data("s", vec![255; 10])
        .build()
        .unwrap();
    let source = StoreSource {
        database: root.id(),
        store: "s".into(),
        type_id: "unknown:v0".into(),
        source: QuerySource {
            main: Snapshot::from([root.id()]),
            scope: ReadScope::Verified,
        },
        snapshot: Snapshot::from([root.id()]),
        index_snapshot: Snapshot::EMPTY,
        registration: Vec::new(),
        seal: "seal".into(),
    };
    let request = RawStoreRequest {
        source: source.clone(),
        cursor: None,
    };
    let page = RawStorePage {
        source,
        offset: 0,
        entries: vec![root],
        next: Some(RawCursor {
            context: "context".into(),
            offset: 1,
        }),
    };
    page.validate(&request).unwrap();
    for mutation in 0..5 {
        let mut bad = page.clone();
        match mutation {
            0 => bad.offset = 1,
            1 => bad.next.as_mut().unwrap().offset = 2,
            2 => bad.entries.push(bad.entries[0].clone()),
            3 => bad.source.store = "other".into(),
            _ => bad.entries.clear(),
        }
        assert!(bad.validate(&request).is_err());
    }
    assert!(encoded_size(&vec![255u8; 10], 39).is_err()); // JSON expansion, not raw len
    assert_eq!(encoded_size(&vec![255u8; 10], 41).unwrap(), 41);
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn source_page_count_uses_actual_server_frame_shape() {
    let root = Entry::root_builder()
        .set_subtree_data("s", vec![255; 10])
        .build()
        .unwrap();
    let source = StoreSource {
        database: root.id(),
        store: "s".into(),
        type_id: "unknown:v0".into(),
        source: QuerySource {
            main: Snapshot::from([root.id()]),
            scope: ReadScope::Verified,
        },
        snapshot: Snapshot::from([root.id()]),
        index_snapshot: Snapshot::EMPTY,
        registration: Vec::new(),
        seal: "seal".into(),
    };
    let page = RawStorePage {
        source,
        offset: 0,
        entries: vec![root],
        next: None,
    };
    let frame = crate::service::protocol::ServerFrame::Response(Box::new(
        crate::service::protocol::ServiceResponse::RawStore(page.clone()),
    ));
    let real = serde_json::to_vec(&frame).unwrap().len();
    assert_eq!(page_size(&page, usize::MAX).unwrap(), real);
    assert!(page_size(&page, real - 1).is_err());
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn source_sqlite_bounded_current_historical_and_oversize_entry() {
    let (instance, mut owner) = Instance::create_backend(
        Box::new(
            crate::backend::database::sql::SqlxBackend::sqlite_in_memory()
                .await
                .unwrap(),
        ),
        NewUser::passwordless("sql-source"),
    )
    .await
    .unwrap();
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key().unwrap())
        .await
        .unwrap();
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "old")
        .await
        .unwrap();
    write.commit().await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    let raw = tx
        .raw_store_source("docs", DocStore::type_id())
        .await
        .unwrap();
    let engine = instance.require_local_engine().unwrap();
    assert!(
        engine
            .current_source_frontiers(db.root_id(), &raw.source.main, &[INDEX, "docs"])
            .await
            .unwrap()
            .is_some()
    );
    let write = db.new_transaction().await.unwrap();
    write
        .get_store::<DocStore>("docs")
        .await
        .unwrap()
        .set("key", "new")
        .await
        .unwrap();
    write.commit().await.unwrap();
    assert!(
        engine
            .current_source_frontiers(db.root_id(), &raw.source.main, &[INDEX, "docs"])
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tx.fold_raw_source::<Doc>(&raw)
            .await
            .unwrap()
            .get("key")
            .unwrap()
            .as_text(),
        Some("old")
    );
    assert_eq!(
        tx.raw_store_source("docs", DocStore::type_id())
            .await
            .unwrap()
            .snapshot,
        raw.snapshot
    );
    for size in [1_100_000, 4 * 1024 * 1024 + 1] {
        let entry = Entry::builder(db.root_id().clone())
            .set_parents(raw.source.main.tips().to_vec())
            .set_height(100)
            .set_subtree_data("docs", vec![255; size])
            .build()
            .unwrap();
        let id = entry.id();
        engine.put(entry).await.unwrap();
        assert!(
            matches!(engine.get_source_entry(&id).await, Err(crate::Error::Backend(e)) if matches!(*e, BackendError::SourceTooLarge))
        );
    }
}

#[tokio::test]
async fn legacy_collection_enforces_walk_and_response_bounds() {
    let (_instance, engine, db, request) = fixture().await;
    let tips = engine
        .store_snapshot_at(db.root_id(), "docs", &request.source.main)
        .await
        .unwrap();
    let mut sufficient = limits();
    sufficient.page_bytes = 64 * 1024;
    sufficient.source_bytes = 128 * 1024;
    let entries = collect_entries_with_limits(
        engine.as_ref(),
        db.root_id(),
        "docs",
        tips.tips(),
        ReadScope::Verified,
        sufficient,
    )
    .await
    .unwrap();
    assert_eq!(entries.len(), 8);
    let mut small = limits();
    small.nodes = 2;
    assert!(
        matches!(collect_entries_with_limits(engine.as_ref(), db.root_id(), "docs", tips.tips(), ReadScope::Verified, small).await, Err(crate::Error::Backend(e)) if matches!(*e, BackendError::SourceTooLarge))
    );
    let mut small = limits();
    small.page_bytes = 12 * 1024;
    assert!(
        matches!(collect_entries_with_limits(engine.as_ref(), db.root_id(), "docs", tips.tips(), ReadScope::Verified, small).await, Err(crate::Error::Backend(e)) if matches!(*e, BackendError::SourceTooLarge))
    );
}

#[tokio::test]
async fn reserved_metadata_is_fixed_not_unregistered_doc_fallback() {
    let (_instance, engine, db, request) = fixture().await;
    let sources = Sources::default();
    for store in [INDEX, SETTINGS] {
        let query = StoreQueryRequest {
            store: store.into(),
            ..request.clone()
        };
        let bound = sources
            .resolve(engine.as_ref(), &Reader::local(), db.root_id(), &query)
            .await
            .unwrap();
        assert_eq!(bound.source.type_id, DocStore::type_id());
        sources
            .validate_binding(engine.as_ref(), &bound.source)
            .await
            .unwrap();
        let query = StoreQueryRequest {
            expected_type: "unknown:v0".into(),
            ..query
        };
        assert!(
            sources
                .resolve(engine.as_ref(), &Reader::local(), db.root_id(), &query)
                .await
                .is_err()
        );
    }
    let query = StoreQueryRequest {
        store: "unregistered".into(),
        ..request
    };
    assert!(
        sources
            .resolve(engine.as_ref(), &Reader::local(), db.root_id(), &query)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn higher_view_selects_ancestors_without_retargeting_pinned_store_snapshots() -> Result<()> {
    let (_instance, engine, db, request) = fixture().await;
    let sources = Sources::default();
    let reader = Reader::local();
    let initial = sources
        .resolve(engine.as_ref(), &reader, db.root_id(), &request)
        .await?;
    let tip = initial.source.snapshot.tips()[0].clone();
    // Advance main without changing this Store. The loose main boundary is
    // explicitly selected here, above source resolution and the value cache.
    let write = db.new_transaction().await?;
    write.get_settings()?.set_name("unrelated").await?;
    write.commit().await?;
    let loose = StoreQueryRequest {
        source: QuerySource {
            main: db.clone().allow_unverified().snapshot().await?,
            scope: ReadScope::AllowUnverified,
        },
        ..request.clone()
    };
    let pinned = sources
        .resolve(engine.as_ref(), &reader, db.root_id(), &loose)
        .await?;
    engine
        .update_verification_status(&tip, VerificationStatus::Failed)
        .await?;
    assert!(
        sources
            .recover(engine.as_ref(), &reader, &initial.source)
            .await
            .is_err()
    );
    let same = sources
        .resolve(engine.as_ref(), &reader, db.root_id(), &loose)
        .await?;
    assert_eq!(same.source.snapshot, pinned.source.snapshot);
    assert_eq!(
        same.entries, pinned.entries,
        "loose paging cannot filter/retarget a Store Snapshot"
    );
    sources
        .recover(engine.as_ref(), &reader, &pinned.source)
        .await?;
    // Existing strict higher-layer selection may choose an ancestor boundary.
    let selected = StoreQueryRequest {
        source: QuerySource {
            main: db.snapshot().await?,
            scope: ReadScope::Verified,
        },
        ..request
    };
    let earlier = sources
        .resolve(engine.as_ref(), &reader, db.root_id(), &selected)
        .await?;
    assert_ne!(earlier.source.source.main, pinned.source.source.main);
    assert_eq!(
        earlier.source.snapshot,
        Snapshot::from(engine.get(&tip).await?.subtree_parents("docs")?)
    );
    assert!(earlier.entries.iter().all(|entry| entry.id_ref() != &tip));
    assert!(
        sources
            .validate_binding(engine.as_ref(), &initial.source)
            .await
            .is_err(),
        "fresh selection must not make the old pin recoverable at different tips"
    );
    Ok(())
}

#[tokio::test]
async fn source_serialization_bounds_pending_callers_and_releases_on_cancellation() -> Result<()> {
    let mut small = limits();
    small.global_contexts = 2;
    let sources = Arc::new(Sources::new(small, Duration::from_secs(1)));
    let reader = Reader::local();
    let tree = ID::from_bytes(b"tree");
    let request = StoreQueryRequest {
        store: "docs".into(),
        expected_type: DocStore::type_id().into(),
        source: QuerySource {
            main: Snapshot::from([tree.clone()]),
            scope: ReadScope::Verified,
        },
        query: Vec::new(),
    };
    let first = sources.admit_serialized(&reader, &tree, &request).await?;
    let pending = {
        let (sources, reader, tree, request) = (
            sources.clone(),
            reader.clone(),
            tree.clone(),
            request.clone(),
        );
        tokio::spawn(async move { sources.admit_serialized(&reader, &tree, &request).await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while sources.callers.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    admission(
        sources
            .admit_serialized(&reader, &tree, &request)
            .await
            .err()
            .unwrap(),
    );
    pending.abort();
    match pending.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("pending caller was not cancelled"),
    }
    assert_eq!(sources.callers.available_permits(), 1);
    // A held source cannot be waited on indefinitely either.
    admission(
        sources
            .admit_serialized(&reader, &tree, &request)
            .await
            .err()
            .unwrap(),
    );
    drop(first);
    assert_eq!(sources.callers.available_permits(), 2);
    let last = sources.admit_serialized(&reader, &tree, &request).await?;
    drop(last);
    assert!(sources.state.lock().unwrap().jobs.is_empty());
    Ok(())
}
