//! Empty merge-base diagnostics must describe the queried ancestry, not the whole store.

use std::{
    io,
    sync::{Arc, Mutex},
};

use eidetica::entry::{Entry, ID};

use super::helpers::test_backend;
use crate::helpers::TestVerify;

const STORE: &str = "trace";

#[derive(Clone)]
struct EventWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for EventWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn assert_event(output: &str, count: usize, walked: usize, multiple: bool) {
    let event = output
        .lines()
        .find(|line| line.contains("merging from the empty base"))
        .expect("empty-base event was emitted");
    for field in [
        format!("common_ancestor_count={count}"),
        format!("walked_entry_count={walked}"),
        format!("multiple_roots={multiple}"),
    ] {
        assert!(event.contains(&field), "missing {field} in {event}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_empty_merge_base_events_describe_walked_ancestry() {
    let backend = test_backend().await;
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    backend.put_verified(root).await.unwrap();

    // A and B are independent roots in this store, despite sharing a tree root.
    // C reaches both; D reaches only A. A is common to C and D but C bypasses it.
    let mut ids = Vec::<ID>::new();
    for name in ["a", "b", "unrelated"] {
        let entry = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_subtree_data(STORE, name.as_bytes())
            .build()
            .unwrap();
        ids.push(entry.id());
        backend.put_verified(entry).await.unwrap();
    }
    let (a, b, unrelated) = (&ids[0], &ids[1], &ids[2]);
    let c = Entry::builder(tree.clone())
        .add_parent(a.clone())
        .add_parent(b.clone())
        .set_subtree_data(STORE, b"c")
        .add_subtree_parent(STORE, a.clone())
        .add_subtree_parent(STORE, b.clone())
        .build()
        .unwrap();
    let c_id = c.id();
    backend.put_verified(c).await.unwrap();
    let d = Entry::builder(tree.clone())
        .add_parent(a.clone())
        .set_subtree_data(STORE, b"d")
        .add_subtree_parent(STORE, a.clone())
        .build()
        .unwrap();
    let d_id = d.id();
    backend.put_verified(d).await.unwrap();

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = EventWriter(buffer.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // No common ancestor; a third unrelated root must not inflate either field.
    assert_eq!(
        backend
            .find_merge_base(&tree, STORE, &[a.clone(), b.clone()])
            .await
            .unwrap(),
        None
    );
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert_event(&output, 0, 2, true);

    buffer.lock().unwrap().clear();
    assert_eq!(
        backend
            .find_merge_base(&tree, STORE, &[c_id, d_id])
            .await
            .unwrap(),
        None
    );
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    // C,A,B and D,A: A counts once for each tip, unrelated counts zero.
    assert_event(&output, 1, 5, true);

    // Querying one independent tip and the unrelated tip still counts two roots.
    buffer.lock().unwrap().clear();
    assert_eq!(
        backend
            .find_merge_base(&tree, STORE, &[a.clone(), unrelated.clone()])
            .await
            .unwrap(),
        None
    );
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert_event(&output, 0, 2, true);
}
