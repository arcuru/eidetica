//! Canonical bounded history pages must equal `store_at`, page by page.

use eidetica::{
    Error,
    backend::{BackendError, BackendImpl, HistoryCursor, HistoryPage, HistoryPageLimits},
    entry::{Entry, ID},
    snapshot::Snapshot,
};

use crate::helpers::{TestVerify, test_backend};

const STORE: &str = "data";

fn limits(max_entries: usize, max_encoded_bytes: usize, singleton: bool) -> HistoryPageLimits {
    HistoryPageLimits {
        max_entries,
        max_encoded_bytes,
        allow_large_singleton: singleton,
    }
}

fn backend_error(error: &Error) -> &BackendError {
    match error {
        Error::Backend(error) => error,
        other => panic!("expected backend error, got {other:?}"),
    }
}

async fn put(backend: &dyn BackendImpl, entry: &Entry) -> ID {
    backend.put_verified(entry.clone()).await.unwrap();
    entry.id()
}

/// Layered DAG with forks, merges, tree-only entries, a foreign Store and a
/// mid-history Snapshot. Returns (root, tips of the final layer, a mid entry).
async fn build_dag(
    backend: &dyn BackendImpl,
    payload: impl Fn(usize) -> String,
) -> (ID, Vec<ID>, ID) {
    let root = Entry::root_builder()
        .set_subtree_data(STORE, payload(0))
        .build()
        .unwrap();
    let root_id = put(backend, &root).await;
    let mut layer = vec![root_id.clone()];
    let mut mid = root_id.clone();
    let mut n = 1;
    for depth in 1..=6u64 {
        let mut next = Vec::new();
        for i in 0..4 {
            let first = layer[i % layer.len()].clone();
            let second = layer[(i + 1) % layer.len()].clone();
            let mut builder = Entry::builder(root_id.clone())
                .set_height(depth)
                .add_parent(first.clone());
            if i % 2 == 1 && first != second {
                builder = builder.add_parent(second.clone());
            }
            if i == 3 {
                // Tree-only Entry: never part of this Store's history.
                builder = builder.set_subtree_data("other", "{}");
            } else {
                builder = builder
                    .set_subtree_data(STORE, payload(n))
                    .set_subtree_height(STORE, Some(depth))
                    .add_subtree_parent(STORE, first);
                if i % 2 == 1 && layer.len() > 1 {
                    builder = builder.add_subtree_parent(STORE, second);
                }
            }
            let entry = builder.build().unwrap();
            next.push(put(backend, &entry).await);
            n += 1;
        }
        if depth == 3 {
            mid = next[1].clone();
        }
        layer = next;
    }
    (root_id, layer, mid)
}

/// Read every page, checking per-page bounds, and return the emitted IDs.
async fn read_all(
    backend: &dyn BackendImpl,
    tree: &ID,
    snapshot: &Snapshot,
    page_limits: HistoryPageLimits,
) -> Vec<ID> {
    let mut ids = Vec::new();
    let mut cursor: Option<HistoryCursor> = None;
    for _ in 0..1000 {
        let page = backend
            .store_history_page(tree, STORE, snapshot, cursor.as_ref(), page_limits)
            .await
            .unwrap();
        let encoded = serde_json::to_vec(&page).unwrap().len();
        assert!(encoded <= page_limits.max_encoded_bytes, "{encoded} bytes");
        assert!(page.entries.len() <= page_limits.max_entries.max(1));
        ids.extend(page.entries.iter().map(Entry::id));
        match page.next {
            Some(next) => {
                assert!(!page.entries.is_empty(), "non-final page must progress");
                cursor = Some(next);
            }
            None => return ids,
        }
    }
    panic!("history paging did not terminate");
}

#[tokio::test]
async fn history_pages_concatenate_to_store_at() {
    let backend = test_backend().await;
    let (root, tips, mid) = build_dag(&*backend, |n| format!(r#"{{"n":{n}}}"#)).await;
    for snapshot in [
        Snapshot::new(tips.clone()),
        Snapshot::new(vec![mid.clone()]),
        // Tips outside the Store are ignored, exactly as store_at does.
        Snapshot::new(vec![tips[0].clone(), tips[3].clone()]),
    ] {
        let entries = backend.store_at(&root, STORE, &snapshot).await.unwrap();
        let expected: Vec<ID> = entries.iter().map(Entry::id).collect();
        let whole = read_all(&*backend, &root, &snapshot, limits(1024, 1 << 20, false)).await;
        assert_eq!(whole, expected);
        let one = backend
            .store_history_page(&root, STORE, &snapshot, None, limits(1, 1 << 20, false))
            .await
            .unwrap();
        // Largest single-Entry page (cursor heights vary by a few digits).
        let entry_size = entries
            .iter()
            .map(|entry| {
                let page = HistoryPage {
                    entries: vec![entry.clone()],
                    next: one.next.clone(),
                };
                serde_json::to_vec(&page).unwrap().len() + 20
            })
            .max()
            .unwrap_or(1 << 10);
        for (count, bytes) in [
            (1, 1 << 20),
            (2, 1 << 20),
            (3, 1 << 20),
            (7, 1 << 20),
            (1024, entry_size),
            (1024, entry_size * 3),
        ] {
            let paged = read_all(&*backend, &root, &snapshot, limits(count, bytes, false)).await;
            assert_eq!(paged, expected, "count {count} bytes {bytes}");
        }
    }
    let empty = backend
        .store_history_page(
            &root,
            STORE,
            &Snapshot::new(vec![]),
            None,
            limits(8, 1 << 20, false),
        )
        .await
        .unwrap();
    assert_eq!(empty, HistoryPage::default());
}

#[tokio::test]
async fn history_page_exact_byte_boundary_and_metadata_floor() {
    let backend = test_backend().await;
    let (root, tips, _) = build_dag(&*backend, |n| format!(r#"{{"n":{n}}}"#)).await;
    let snapshot = Snapshot::new(tips);
    let two = backend
        .store_history_page(&root, STORE, &snapshot, None, limits(2, 1 << 20, false))
        .await
        .unwrap();
    assert_eq!(two.entries.len(), 2);
    let size = serde_json::to_vec(&two).unwrap().len();
    let exact = backend
        .store_history_page(&root, STORE, &snapshot, None, limits(2, size, false))
        .await
        .unwrap();
    assert_eq!(exact, two);
    let short = backend
        .store_history_page(&root, STORE, &snapshot, None, limits(2, size - 1, false))
        .await
        .unwrap();
    assert_eq!(short.entries, two.entries[..1]);
    assert!(short.next.is_some());
    assert!(serde_json::to_vec(&short).unwrap().len() < size);

    let floor = serde_json::to_vec(&HistoryPage::default()).unwrap().len();
    let error = backend
        .store_history_page(&root, STORE, &snapshot, None, limits(2, floor - 1, false))
        .await
        .unwrap_err();
    assert!(
        matches!(backend_error(&error), BackendError::RecordTooLarge { .. }),
        "{error:?}"
    );
    // A count of zero is clamped to one; a page never fakes progress.
    let page = backend
        .store_history_page(&root, STORE, &snapshot, None, limits(0, 1 << 20, false))
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
}

#[tokio::test]
async fn history_cursor_is_bound_to_query() {
    let backend = test_backend().await;
    let (root, tips, mid) = build_dag(&*backend, |n| format!(r#"{{"n":{n}}}"#)).await;
    let snapshot = Snapshot::new(tips);
    let page = backend
        .store_history_page(&root, STORE, &snapshot, None, limits(1, 1 << 20, false))
        .await
        .unwrap();
    let cursor = page.next.unwrap();
    let other = Snapshot::new(vec![mid]);
    for (store, snapshot) in [("other", &snapshot), (STORE, &other)] {
        let error = backend
            .store_history_page(
                &root,
                store,
                snapshot,
                Some(&cursor),
                limits(1, 1 << 20, false),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(backend_error(&error), BackendError::InvalidHistoryCursor),
            "{error:?}"
        );
    }
    // Opaque but validated: a forged cursor body is rejected, not reinterpreted.
    let mut forged = serde_json::to_value(&cursor).unwrap();
    forged["binding"][0] = serde_json::json!((forged["binding"][0].as_u64().unwrap() + 1) % 256);
    let forged: HistoryCursor = serde_json::from_value(forged).unwrap();
    let error = backend
        .store_history_page(
            &root,
            STORE,
            &snapshot,
            Some(&forged),
            limits(1, 1 << 20, false),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        backend_error(&error),
        BackendError::InvalidHistoryCursor
    ));
}

#[tokio::test]
async fn history_page_oversized_entry_is_explicit() {
    let backend = test_backend().await;
    let big = "x".repeat(96 * 1024);
    let (root, tips, _) = build_dag(&*backend, |n| {
        if n == 21 {
            format!(r#"{{"big":"{big}"}}"#)
        } else {
            format!(r#"{{"n":{n}}}"#)
        }
    })
    .await;
    let snapshot = Snapshot::new(tips);
    let expected: Vec<Entry> = backend.store_at(&root, STORE, &snapshot).await.unwrap();
    let position = expected
        .iter()
        .position(|entry| entry.data(STORE).is_ok_and(|data| data.len() > 90 * 1024))
        .expect("oversized fixture Entry");
    assert!(position > 0);
    let budget = 64 * 1024;

    // Earlier fitting Entries are returned; the cursor names the last emitted one.
    let mut cursor = None;
    let mut seen = Vec::new();
    loop {
        match backend
            .store_history_page(
                &root,
                STORE,
                &snapshot,
                cursor.as_ref(),
                limits(1024, budget, false),
            )
            .await
        {
            Ok(page) => {
                assert!(!page.entries.is_empty());
                seen.extend(page.entries.iter().map(Entry::id));
                cursor = page.next;
                assert!(cursor.is_some(), "the oversized Entry is still ahead");
            }
            Err(error) => {
                assert!(
                    matches!(backend_error(&error), BackendError::EntryTooLarge { id, .. } if *id == expected[position].id()),
                    "{error:?}"
                );
                break;
            }
        }
    }
    let ids: Vec<ID> = expected.iter().map(Entry::id).collect();
    assert_eq!(seen, ids[..position]);

    // An explicit singleton carries it whole, then paging continues.
    let page = backend
        .store_history_page(
            &root,
            STORE,
            &snapshot,
            cursor.as_ref(),
            limits(1024, budget, true),
        )
        .await
        .unwrap();
    assert_eq!(page.entries, expected[position..=position]);
    assert!(serde_json::to_vec(&page).unwrap().len() > budget);
    let whole = read_all(&*backend, &root, &snapshot, limits(1024, 1 << 20, false)).await;
    assert_eq!(whole, ids);
}

/// Same DAG, same pages on both reference backends (canonical CID order).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn history_pages_match_across_backends() {
    use eidetica::backend::database::{InMemory, Sqlite};

    let mem_backend = InMemory::new();
    let sql_backend = Sqlite::in_memory().await.unwrap();
    let (mem, sql): (&dyn BackendImpl, &dyn BackendImpl) = (&mem_backend, &sql_backend);
    let root = Entry::root_builder()
        .set_subtree_data(STORE, "{}")
        .build()
        .unwrap();
    let root_id = root.id();
    mem.put_verified(root.clone()).await.unwrap();
    sql.put_verified(root).await.unwrap();
    let mut tips = Vec::new();
    for i in 0..40 {
        let child = Entry::builder(root_id.clone())
            .set_height(1)
            .add_parent(root_id.clone())
            .set_subtree_data(STORE, format!(r#"{{"n":{i}}}"#))
            .add_subtree_parent(STORE, root_id.clone())
            .build()
            .unwrap();
        tips.push(child.id());
        mem.put_verified(child.clone()).await.unwrap();
        sql.put_verified(child).await.unwrap();
    }
    let snapshot = Snapshot::new(tips);
    for count in [1, 3, 1024] {
        let a = read_all(mem, &root_id, &snapshot, limits(count, 1 << 20, false)).await;
        let b = read_all(sql, &root_id, &snapshot, limits(count, 1 << 20, false)).await;
        assert_eq!(a, b);
        let mut sorted = a.clone();
        sorted[1..].sort();
        assert_eq!(a, sorted, "same-height ties follow ID Ord");
    }
}

#[tokio::test]
async fn history_pages_decreasing_heights_and_tampered_position() {
    let backend = test_backend().await;
    let root = Entry::root_builder()
        .set_subtree_data(STORE, "root")
        .build()
        .unwrap();
    let tree = root.id();
    backend.put(root).await.unwrap();
    let parent = Entry::builder(tree.clone())
        .add_parent(tree.clone())
        .set_subtree_data(STORE, "late-sorting parent")
        .add_subtree_parent(STORE, tree.clone())
        .set_subtree_height(STORE, Some(50))
        .build()
        .unwrap();
    let parent_id = parent.id();
    backend.put(parent).await.unwrap();
    let child = Entry::builder(tree.clone())
        .add_parent(parent_id.clone())
        .set_subtree_data(STORE, "early-sorting descendant")
        .add_subtree_parent(STORE, parent_id)
        .set_subtree_height(STORE, Some(1))
        .build()
        .unwrap();
    let tips = Snapshot::new(vec![child.id()]);
    backend.put(child).await.unwrap();
    let expected = backend.store_at(&tree, STORE, &tips).await.unwrap();
    assert_eq!(
        read_all(&*backend, &tree, &tips, limits(1, 1 << 20, false)).await,
        expected.iter().map(Entry::id).collect::<Vec<_>>()
    );
    let page = backend
        .store_history_page(&tree, STORE, &tips, None, limits(1, 1 << 20, false))
        .await
        .unwrap();
    let mut forged = serde_json::to_value(page.next.unwrap()).unwrap();
    forged["height"] = serde_json::json!(9999);
    let cursor: HistoryCursor = serde_json::from_value(forged).unwrap();
    assert!(
        backend
            .store_history_page(
                &tree,
                STORE,
                &tips,
                Some(&cursor),
                limits(1, 1 << 20, false)
            )
            .await
            .is_err(),
        "tampered canonical position was silently accepted"
    );
}
