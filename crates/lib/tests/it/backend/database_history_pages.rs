//! Peer producers must walk the whole main DAG, not one Store or a prebuilt Vec.

use std::time::Duration;

use eidetica::{
    Error,
    backend::{
        BackendError, BackendImpl, HistoryCursor, HistoryPageLimits, MAX_DATABASE_PAGE_ENCODING,
    },
    entry::{Entry, ID},
    snapshot::Snapshot,
};
use tokio::time::Instant;

use crate::helpers::test_backend;

fn limits(count: usize, bytes: usize) -> HistoryPageLimits {
    HistoryPageLimits {
        max_entries: count,
        max_encoded_bytes: bytes,
        allow_large_singleton: false,
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn is_cursor(error: Error) -> bool {
    matches!(error, Error::Backend(e) if matches!(*e, BackendError::InvalidHistoryCursor))
}

async fn graph(b: &dyn BackendImpl, payload: usize) -> (ID, Snapshot, Vec<Entry>) {
    let root = Entry::root_builder()
        .set_subtree_data("_settings", "synthetic root")
        .build()
        .unwrap();
    let tree = root.id();
    let mut all = vec![root];
    for n in 1..=18u64 {
        let parent = all.last().unwrap().id();
        let mut builder = Entry::builder(tree.clone())
            .set_height(n)
            .add_parent(parent)
            .set_subtree_data(
                if n % 2 == 0 { "data" } else { "_settings" },
                vec![255u8; payload],
            );
        // A merge edge that does not occur in a Store-specific parent set.
        if n == 9 {
            builder = builder.add_parent(all[3].id());
        }
        all.push(builder.build().unwrap());
    }
    for entry in &all {
        b.put(entry.clone()).await.unwrap();
    }
    let target = Snapshot::from([all.last().unwrap().id()]);
    (tree, target, all)
}

async fn collect(
    b: &dyn BackendImpl,
    tree: &ID,
    target: &Snapshot,
    known: &Snapshot,
    limits: HistoryPageLimits,
) -> (Vec<Entry>, usize) {
    let mut entries = Vec::new();
    let mut after: Option<HistoryCursor> = None;
    let end = deadline();
    for pages in 1..=1000 {
        let page = b
            .database_history_page(tree, target, known, after.as_ref(), limits, end)
            .await
            .unwrap();
        assert!(
            serde_json::to_vec(&page).unwrap().len()
                <= limits.max_encoded_bytes.min(MAX_DATABASE_PAGE_ENCODING)
        );
        assert!(page.entries.len() <= limits.max_entries.clamp(1, 1024));
        if let Some(next) = &page.next {
            assert!(!page.entries.is_empty());
            assert_ne!(Some(next), after.as_ref());
        }
        entries.extend(page.entries);
        match page.next {
            Some(next) => after = Some(next),
            None => return (entries, pages),
        }
    }
    panic!("Database pages did not terminate");
}

#[tokio::test]
async fn database_history_pages_include_root_all_stores_and_large_aggregate() {
    let b = test_backend().await;
    let (tree, target, expected) = graph(&*b, 20_000).await;
    assert!(serde_json::to_vec(&expected).unwrap().len() > 1024 * 1024);
    for bounds in [
        limits(1, 150_000),
        limits(4, 180_000),
        limits(1024, MAX_DATABASE_PAGE_ENCODING),
    ] {
        let (actual, pages) = collect(&*b, &tree, &target, &Snapshot::EMPTY, bounds).await;
        assert_eq!(actual, expected);
        assert!(pages > 1);
        assert_eq!(actual[0].id(), tree);
    }
}

#[tokio::test]
async fn database_history_pages_preserve_exact_stop_boundary_and_frozen_target() {
    let b = test_backend().await;
    let (tree, target, all) = graph(&*b, 1).await;
    let known = Snapshot::from([all[8].id(), ID::from_bytes("unrelated absent peer tip")]);
    let (actual, _) = collect(&*b, &tree, &target, &known, limits(2, 10_000)).await;
    // The merge at 9 has a second path to the root, so exact-boundary peer
    // selection still sends 0..=3. It must NOT subtract all ancestors of 8.
    let expected: Vec<_> = all[..=3].iter().chain(&all[9..]).cloned().collect();
    assert_eq!(actual, expected);
    let frozen = Snapshot::from([all[4].id()]);
    assert_eq!(
        collect(&*b, &tree, &frozen, &Snapshot::EMPTY, limits(2, 10_000))
            .await
            .0,
        all[..=4]
    );
    assert!(
        collect(&*b, &tree, &target, &target, limits(2, 10_000))
            .await
            .0
            .is_empty()
    );
}

#[tokio::test]
async fn database_history_pages_cursors_bind_scope_target_boundary_and_position() {
    let b = test_backend().await;
    let (tree, target, all) = graph(&*b, 1).await;
    let first = b
        .database_history_page(
            &tree,
            &target,
            &Snapshot::EMPTY,
            None,
            limits(1, 10_000),
            deadline(),
        )
        .await
        .unwrap();
    let cursor = first.next.unwrap();
    for (scope, target, known) in [
        (tree.clone(), Snapshot::from([all[9].id()]), Snapshot::EMPTY),
        (tree.clone(), target.clone(), Snapshot::from([all[3].id()])),
        (
            ID::from_bytes("wrong Database"),
            target.clone(),
            Snapshot::EMPTY,
        ),
    ] {
        assert!(is_cursor(
            b.database_history_page(
                &scope,
                &target,
                &known,
                Some(&cursor),
                limits(2, 10_000),
                deadline()
            )
            .await
            .unwrap_err()
        ));
    }
    assert!(is_cursor(
        b.store_history_page(
            &tree,
            "_settings",
            &target,
            Some(&cursor),
            limits(2, 10_000)
        )
        .await
        .unwrap_err()
    ));
    for (field, value) in [
        ("v", serde_json::json!(99)),
        ("height", serde_json::json!(999)),
        (
            "id",
            serde_json::json!(ID::from_bytes("not a reachable cursor")),
        ),
    ] {
        let mut wire = serde_json::to_value(&cursor).unwrap();
        wire[field] = value;
        let bad: HistoryCursor = serde_json::from_value(wire).unwrap();
        assert!(is_cursor(
            b.database_history_page(
                &tree,
                &target,
                &Snapshot::EMPTY,
                Some(&bad),
                limits(2, 10_000),
                deadline()
            )
            .await
            .unwrap_err()
        ));
    }
    // The cursor is an exclusive position, not authority or a spent token:
    // repeating the same bound page query is deterministic.
    let a = b
        .database_history_page(
            &tree,
            &target,
            &Snapshot::EMPTY,
            Some(&cursor),
            limits(2, 10_000),
            deadline(),
        )
        .await
        .unwrap();
    let c = b
        .database_history_page(
            &tree,
            &target,
            &Snapshot::EMPTY,
            Some(&cursor),
            limits(2, 10_000),
            deadline(),
        )
        .await
        .unwrap();
    assert_eq!(a, c);
}

#[tokio::test]
async fn database_history_pages_reject_missing_foreign_and_nonforward_ancestry() {
    for invalid in [0, 1, 2] {
        let b = test_backend().await;
        let (tree, _, all) = graph(&*b, 1).await;
        let foreign = Entry::root_builder()
            .set_subtree_data("data", "foreign")
            .build()
            .unwrap();
        b.put(foreign.clone()).await.unwrap();
        let parent = match invalid {
            0 => ID::from_bytes("missing"),
            1 => foreign.id(),
            _ => all[18].id(),
        };
        let entry = Entry::builder(tree.clone())
            .set_height(1)
            .add_parent(parent)
            .set_subtree_data("data", "invalid")
            .build()
            .unwrap();
        let target = Snapshot::from([entry.id()]);
        b.put(entry).await.unwrap();
        assert!(
            b.database_history_page(
                &tree,
                &target,
                &Snapshot::EMPTY,
                None,
                limits(2, 10_000),
                deadline()
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn database_history_pages_deadline_and_singleton_are_not_extended() {
    let b = test_backend().await;
    let (tree, target, _) = graph(&*b, 1).await;
    let error = b
        .database_history_page(
            &tree,
            &target,
            &Snapshot::EMPTY,
            None,
            limits(2, 10_000),
            Instant::now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Backend(e) if matches!(*e, BackendError::HistoryResourceLimit { .. }))
    );
    let root = Entry::root_builder()
        .set_subtree_data("data", vec![255u8; 300_000])
        .build()
        .unwrap();
    let id = root.id();
    assert!(serde_json::to_vec(&root).unwrap().len() > MAX_DATABASE_PAGE_ENCODING);
    b.put(root).await.unwrap();
    let mut large = limits(1024, 63 * 1024 * 1024);
    large.allow_large_singleton = true;
    let error = b
        .database_history_page(
            &id,
            &Snapshot::from([id.clone()]),
            &Snapshot::EMPTY,
            None,
            large,
            deadline(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Backend(e) if matches!(*e, BackendError::EntryTooLarge { id: ref too_large, encoded_bytes } if too_large == &id && encoded_bytes == MAX_DATABASE_PAGE_ENCODING + 1))
    );
}

#[tokio::test]
async fn database_history_pages_forks_ties_and_exact_encoded_boundaries() {
    let b = test_backend().await;
    let root = Entry::root_builder()
        .set_subtree_data("escaped\"\\\n\tstore", vec![255u8; 27])
        .build()
        .unwrap();
    let tree = root.id();
    b.put(root.clone()).await.unwrap();
    let target = Snapshot::from([tree.clone()]);
    let page = b
        .database_history_page(
            &tree,
            &target,
            &Snapshot::EMPTY,
            None,
            limits(1, 10_000),
            deadline(),
        )
        .await
        .unwrap();
    let exact = serde_json::to_vec(&page).unwrap().len();
    for bytes in [exact - 1, exact, exact + 1] {
        let result = b
            .database_history_page(
                &tree,
                &target,
                &Snapshot::EMPTY,
                None,
                limits(1, bytes),
                deadline(),
            )
            .await;
        assert_eq!(result.is_ok(), bytes >= exact);
    }
    let mut all = vec![root];
    for n in (0..5).rev() {
        let entry = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_height(1)
            .set_subtree_data(format!("Store{n}"), format!("fork{n}"))
            .build()
            .unwrap();
        b.put(entry.clone()).await.unwrap();
        all.push(entry);
    }
    let target = Snapshot::from(all[1..].iter().map(Entry::id).collect::<Vec<_>>());
    all.sort_by_key(|e| (e.height(), e.id()));
    for count in [1, 2, 5, 1024] {
        assert_eq!(
            collect(&*b, &tree, &target, &Snapshot::EMPTY, limits(count, 10_000))
                .await
                .0,
            all
        );
    }
}

#[tokio::test]
async fn database_history_pages_refuse_oversized_input_frontiers() {
    let b = test_backend().await;
    let tree = ID::from_bytes("scope");
    let wide = Snapshot::from(
        (0u32..4097)
            .map(|n| ID::from_bytes(n.to_le_bytes()))
            .collect::<Vec<_>>(),
    );
    for (target, known) in [(&wide, &Snapshot::EMPTY), (&Snapshot::EMPTY, &wide)] {
        let error = b
            .database_history_page(&tree, target, known, None, limits(1, 10_000), deadline())
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::Backend(e) if matches!(*e, BackendError::HistoryResourceLimit { .. }))
        );
    }
}
