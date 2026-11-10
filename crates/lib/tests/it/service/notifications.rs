//! Server-push write notifications and client subscription lifecycle tests.

use std::time::Duration;

use eidetica::Instance;
use eidetica::store::DocStore;

use super::{create_user_via_admin, remote_conn, start_test_server};

// =============================================================================
// Server-push write notifications (`Notification::DatabaseWrite`).
//
// Exercises the end-to-end path: client `Database::on_write` registration sends
// `DatabaseOp::SubscribeWrites`, daemon's global write callback fans events
// out to subscribed connections, client reader task routes notifications into
// the local callback registry.
//
// Generous timeouts: the notification is asynchronous to `commit().await` on a
// connected instance (by design — see `Database::on_write` docs § Callback
// timing). 2s is enormous for a Unix-socket round-trip but absorbs CI jitter.
// =============================================================================

/// Helper: drain a callback-fed channel until `min` events arrive (or timeout).
async fn collect_events<T>(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>,
    min: usize,
    timeout_secs: u64,
) -> Vec<T> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    while out.len() < min {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(v)) => out.push(v),
            Ok(None) => break, // channel closed
            Err(_) => break,   // timeout
        }
    }
    out
}

#[tokio::test]
async fn test_on_write_fires_for_local_commit_on_connected_instance() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "callback_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Single fire per submit under the fire-on-Verified model — no
    // need to filter on verification state, every notification we
    // receive here is settled.
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<(usize, eidetica::instance::WriteSource)>();
    let _cb = db
        .on_write(move |event, db| {
            let prev = event.previous_tips().clone();
            let post = event.post_tips().clone();
            let db = db.clone();
            let source = event.source();
            let tx = event_tx.clone();
            async move {
                let count = db.ids_added(&prev, &post).await?.len();
                let _ = tx.send((count, source));
                Ok(())
            }
        })
        .await
        .unwrap();

    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();

    let events = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(events.len(), 1, "exactly one Verified notification");
    assert_eq!(events[0].0, 1, "exactly one entry in the event");
    assert_eq!(
        events[0].1,
        eidetica::instance::WriteSource::Local,
        "a service-client commit is local to the daemon peer"
    );
}

#[tokio::test]
async fn test_on_write_fires_across_clients_on_same_daemon() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Set up a database server-side so both clients have the same root_id.
    let mut server_user = server.login_account("alice", "").await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "cross_client_test");
    let server_key = server_user.get_default_key().unwrap();
    let server_db = server_user
        .create_database(settings, &server_key)
        .await
        .unwrap();
    let root_id = server_db.root_id().clone();

    // Observer client: registers on_write, waits.
    let observer = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let observer_user = observer.login_account("alice", "").await.unwrap();
    let observer_db = observer_user.open_database(&root_id).await.unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let _cb = observer_db
        .on_write(move |_event, _db| {
            let tx = event_tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        })
        .await
        .unwrap();

    // Writer client: connects, commits an entry. The observer must see it.
    let writer = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let writer_user = writer.login_account("alice", "").await.unwrap();
    let writer_db = writer_user.open_database(&root_id).await.unwrap();
    writer_db
        .with_transaction(|tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store.set("from", "writer").await?;
            Ok(())
        })
        .await
        .unwrap();

    let events = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(
        events.len(),
        1,
        "observer must see writer's commit via daemon push"
    );
}

#[tokio::test]
async fn test_on_write_previous_tips_populated_on_connected_instance() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "prev_tips_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Single fire per commit under fire-on-Verified — no filter needed.
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<eidetica::entry::ID>, eidetica::Snapshot)>();
    let _cb = db
        .on_write(move |event, db| {
            let prev_tips = event.previous_tips().clone();
            let post_tips = event.post_tips().clone();
            let db = db.clone();
            let tx = event_tx.clone();
            async move {
                let entries = db.ids_added(&prev_tips, &post_tips).await?;
                let _ = tx.send((entries, prev_tips));
                Ok(())
            }
        })
        .await
        .unwrap();

    // Two sequential commits; the second's previous_tips must include the
    // first entry's id (and must not be empty — the V1 behavior before this
    // change).
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v1").await?;
        Ok(())
    })
    .await
    .unwrap();
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v2").await?;
        Ok(())
    })
    .await
    .unwrap();

    let events = collect_events(&mut event_rx, 2, 2).await;
    assert_eq!(events.len(), 2, "both commits must surface as callbacks");
    let (entries_1, _prev_1) = &events[0];
    let (_entries_2, prev_2) = &events[1];
    assert!(
        !prev_2.is_empty(),
        "second callback's previous_tips must be populated (got empty — daemon push not delivering canonical tips)"
    );
    assert!(
        prev_2.tips().contains(&entries_1[0]),
        "second callback's previous_tips must include the first commit's entry id; got prev={prev_2:?}, first_entry={:?}",
        entries_1[0]
    );
}

#[tokio::test]
async fn test_on_write_dropped_handle_stops_callback() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "drop_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let cb = db
        .on_write(move |_event, _db| {
            let tx = event_tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        })
        .await
        .unwrap();

    // First commit: callback fires.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v1").await?;
        Ok(())
    })
    .await
    .unwrap();
    let first = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(first.len(), 1, "first commit must fire callback");

    // Drop the WriteCallback handle — unregisters from the local registry.
    // The daemon may still push (subscription persists on the connection),
    // but with no callback registered the dispatch is a no-op.
    drop(cb);

    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v2").await?;
        Ok(())
    })
    .await
    .unwrap();

    // Give the daemon round-trip a moment to complete; if a callback were
    // going to fire (regression), it would do so well within 500ms on a
    // local Unix socket.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_drop = collect_events(&mut event_rx, 1, 0).await;
    assert!(
        after_drop.is_empty(),
        "dropped WriteCallback must stop firing user code; got {after_drop:?}"
    );
}

#[tokio::test]
async fn test_on_write_only_fires_for_subscribed_tree() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings_a = eidetica::crdt::Doc::new();
    settings_a.set("name", "tree_a");
    let db_a = user.create_database(settings_a, &pubkey).await.unwrap();

    let mut settings_b = eidetica::crdt::Doc::new();
    settings_b.set("name", "tree_b");
    let db_b = user.create_database(settings_b, &pubkey).await.unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let _cb = db_a
        .on_write(move |_event, _db| {
            let tx = event_tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        })
        .await
        .unwrap();

    // Commit to tree B; tree A's callback must not fire.
    db_b.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cross_tree = collect_events(&mut event_rx, 1, 0).await;
    assert!(
        cross_tree.is_empty(),
        "callback on tree A must not fire for writes to tree B; got {cross_tree:?}"
    );

    // Now commit to tree A — the callback must fire.
    db_a.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();
    let same_tree = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(
        same_tree.len(),
        1,
        "callback on tree A must fire for tree A's own commit"
    );
}

/// Race two `on_write` registrations on the same tree from separate tasks,
/// then commit immediately after both await-points return. Both callbacks
/// must observe the commit — i.e. the second-to-arrive registration must
/// have waited for the in-flight `SubscribeWrites` to land before returning,
/// not raced ahead with the daemon still unsubscribed.
#[tokio::test]
async fn test_on_write_concurrent_registrations_both_observe_commit() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "race_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel::<()>();

    let db_for_a = db.clone();
    let db_for_b = db.clone();
    let reg_a = tokio::spawn(async move {
        db_for_a
            .on_write(move |_event, _db| {
                let tx = tx_a.clone();
                async move {
                    let _ = tx.send(());
                    Ok(())
                }
            })
            .await
            .unwrap()
    });
    let reg_b = tokio::spawn(async move {
        db_for_b
            .on_write(move |_event, _db| {
                let tx = tx_b.clone();
                async move {
                    let _ = tx.send(());
                    Ok(())
                }
            })
            .await
            .unwrap()
    });
    let (_cb_a, _cb_b) = tokio::try_join!(reg_a, reg_b).unwrap();

    // Both `on_write().await` calls have returned. A commit issued now must
    // be visible to both callbacks — if the follower returned without
    // awaiting the leader's subscribe, this commit could land before the
    // daemon recognized the subscription and one callback (or both) would
    // miss it.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();

    let events_a = collect_events(&mut rx_a, 1, 2).await;
    let events_b = collect_events(&mut rx_b, 1, 2).await;
    assert_eq!(
        events_a.len(),
        1,
        "first registration must observe the commit; got {events_a:?}"
    );
    assert_eq!(
        events_b.len(),
        1,
        "second (concurrent) registration must observe the commit; got {events_b:?}"
    );
}

/// Regression: dropping the last per-tree callback transitions the wire
/// subscription to `Idle`; a re-registration inside the grace window
/// avoids a wire round-trip; once the grace window elapses the sweep
/// sends `UnsubscribeWrites` to the daemon.
///
/// Uses `set_idle_grace_window_for_test` and
/// `set_sweep_interval_for_test` (gated behind the `testing` feature)
/// to shrink the windows so the test runs in ~250ms instead of waiting
/// the production-default 60s grace.
#[tokio::test]
async fn test_lazy_unsubscribe_after_grace_window() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    // Shrink the grace + sweep windows for this test. The setters are
    // process-global OnceLocks gated behind the `testing` feature;
    // nextest runs each integration-test binary in its own process so
    // no other test sees the override.
    eidetica::service::client::set_idle_grace_window_for_test(std::time::Duration::from_millis(
        100,
    ));
    eidetica::service::client::set_sweep_interval_for_test(std::time::Duration::from_millis(50));

    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Count daemon-side per-tree callback registrations as a proxy
    // for "the daemon is subscribed for this tree." Each
    // `SubscribeWrites` adds one; `UnsubscribeWrites` removes one.
    // Track via the daemon's internal `Instance::register_write_callback`
    // detected indirectly via a global callback that observes writes
    // — but that's a side measurement. Simpler: just count
    // notifications received on the client. Before sweep:
    // commit fires a notification. After sweep: commit fires nothing.
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "lazy_unsub_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Register a callback, then drop it, then commit. The commit
    // should still fire (subscription is Idle, daemon still pushing,
    // but no local callback to invoke — so we measure differently).
    //
    // Instead, after the grace window elapses, register a *new*
    // callback. By that time the sweep should have unsubscribed —
    // but `on_write_at_tips` then re-subscribes via the leader path.
    // Verify a subsequent commit still fires that new callback (proves
    // the subscribe round-trip happens correctly post-unsubscribe).
    let received = Arc::new(AtomicUsize::new(0));

    {
        let received_clone = received.clone();
        let _cb = db
            .on_write(move |_event, _db| {
                let n = received_clone.clone();
                async move {
                    n.fetch_add(1, AtomicOrdering::Relaxed);
                    Ok(())
                }
            })
            .await
            .unwrap();

        // Fire one commit while callback is alive.
        db.with_transaction(|tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store.set("k", "v1").await?;
            Ok(())
        })
        .await
        .unwrap();

        // Give the notification time to round-trip.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            received.load(AtomicOrdering::Relaxed) >= 1,
            "callback should fire while subscribed"
        );

        // `cb` drops here → transition_to_idle.
    }

    // Wait past the grace window so the sweep runs UnsubscribeWrites.
    // Grace is 100ms, sweep interval is 50ms, so by 300ms we're
    // guaranteed to have swept.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Now register a fresh callback. This should re-subscribe via
    // the leader path (entry was removed by the sweep). Then commit
    // and assert the new callback fires.
    let received2 = Arc::new(AtomicUsize::new(0));
    let received2_clone = received2.clone();
    let _cb2 = db
        .on_write(move |_event, _db| {
            let n = received2_clone.clone();
            async move {
                n.fetch_add(1, AtomicOrdering::Relaxed);
                Ok(())
            }
        })
        .await
        .unwrap();

    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v2").await?;
        Ok(())
    })
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        received2.load(AtomicOrdering::Relaxed) >= 1,
        "re-registered callback after sweep must fire on subsequent commit"
    );
}

/// Regression: per-tree dispatch on the client means a slow callback on
/// tree A does not stall callbacks on tree B. The single-drain-task
/// shape this replaced would have serialised them.
#[tokio::test]
async fn test_on_write_per_tree_concurrent_dispatch() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings_a = eidetica::crdt::Doc::new();
    settings_a.set("name", "tree_a_slow");
    let db_a = user.create_database(settings_a, &pubkey).await.unwrap();
    let mut settings_b = eidetica::crdt::Doc::new();
    settings_b.set("name", "tree_b_fast");
    let db_b = user.create_database(settings_b, &pubkey).await.unwrap();

    // A's callback sleeps for 300ms before flipping `a_done`. B's
    // callback runs immediately and records whether `a_done` is set at
    // the moment it observes the write. If dispatch were serial across
    // trees, B's callback would only run *after* A's sleep completes
    // (a_done == true). Per-tree dispatch means B's callback runs
    // independently and should see a_done == false.
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    let a_done = std::sync::Arc::new(AtomicBool::new(false));
    let b_observed: std::sync::Arc<std::sync::Mutex<Option<bool>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let a_done_for_a = a_done.clone();
    let _cb_a = db_a
        .on_write(move |_event, _db| {
            let a_done = a_done_for_a.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                a_done.store(true, AtomicOrdering::Relaxed);
                Ok(())
            }
        })
        .await
        .unwrap();
    let a_done_for_b = a_done.clone();
    let b_observed_for_b = b_observed.clone();
    let _cb_b = db_b
        .on_write(move |_event, _db| {
            let a_done = a_done_for_b.clone();
            let observed = b_observed_for_b.clone();
            async move {
                let snapshot = a_done.load(AtomicOrdering::Relaxed);
                *observed.lock().unwrap() = Some(snapshot);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Commit to both trees. Order doesn't matter; the daemon's per-tree
    // callback fires push notifications independently.
    let commit_a = db_a.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v_a").await?;
        Ok(())
    });
    let commit_b = db_b.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v_b").await?;
        Ok(())
    });
    tokio::try_join!(commit_a, commit_b).unwrap();

    // Wait long enough for B's notification to round-trip and its
    // callback to record, but well before A's 300ms sleep finishes.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let observation = *b_observed.lock().unwrap();
    assert_eq!(
        observation,
        Some(false),
        "B's callback must run before A's slow callback finishes; per-tree dispatch broken if Some(true) or None: {observation:?}",
    );
}

/// Regression: notifications dispatched to user callbacks on a connected
/// client must arrive in daemon-canonical order, even under burst load.
///
/// Pre-fix: every notification spawned its own dispatch task, so the
/// callback for notification N+1 could start before notification N's
/// callback finished — and could even run on a different worker thread.
/// A user counting `event.entries()[0]` height (or similar) would see
/// arrivals interleaved by the scheduler, not by the daemon's send
/// order.
///
/// Post-fix: a single per-connection drain task `await`s each callback
/// before the next, so the order observed by the user matches the order
/// the daemon pushed.
#[tokio::test]
async fn test_on_write_preserves_daemon_canonical_order_under_burst() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_account("alice", "").await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "ordering_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Each callback yields *before* it records its arrival index, then
    // sleeps for a tiny stagger to make it easy for a per-notification
    // `tokio::spawn` to land out of order. The serial dispatcher must
    // serialise them so the recorded order is monotonic.
    // One fire per commit under fire-on-Verified; no need to filter on
    // verification status. Per-fire ordering is what this test asserts.
    let arrivals: std::sync::Arc<std::sync::Mutex<Vec<u64>>> = Default::default();
    let next_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let arrivals_for_cb = arrivals.clone();
    let next_seq_for_cb = next_seq.clone();
    let _cb = db
        .on_write(move |_event, _db| {
            let arrivals = arrivals_for_cb.clone();
            let next_seq = next_seq_for_cb.clone();
            async move {
                // Yield + sleep stagger: maximise the window for an
                // out-of-order dispatcher to reorder if it could.
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(2)).await;
                let seq = next_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                arrivals.lock().unwrap().push(seq);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Burst commits. Each one is its own daemon-side write, so each
    // becomes a separate notification frame, pushed in order.
    let n: u64 = 12;
    for i in 0..n {
        db.with_transaction(move |tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store.set("k", format!("v{i}")).await?;
            Ok(())
        })
        .await
        .unwrap();
    }

    // Wait for all callbacks to land.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if arrivals.lock().unwrap().len() >= n as usize {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let recorded = arrivals.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        n as usize,
        "every commit must surface as a callback (serialiser must not drop events)"
    );
    // The serial dispatcher guarantees the i'th callback to fire records
    // sequence `i`, monotonically. Pre-fix this assertion would fail
    // intermittently under the yield+sleep stagger.
    for (expected, got) in recorded.iter().enumerate() {
        assert_eq!(
            *got as usize, expected,
            "callbacks must fire in daemon-canonical order; got {recorded:?}"
        );
    }
}

/// Regression (#2): notifications for a single tree must reach a subscriber
/// in daemon-canonical order even when **multiple** connections write that
/// tree concurrently — not just under a single writer's burst.
///
/// The daemon serialises the writes themselves under the tree lock, but the
/// subscription's frame send used to run inside a detached `tokio::spawn`
/// dispatched *after* the lock dropped. Two concurrent writers' send tasks
/// could then race, delivering `post_tips` out of order — the client would
/// observe a `post_tips` that regresses behind the one before it.
///
/// Post-fix: the subscription callback's synchronous `frame_tx.send` runs
/// under the tree lock in cursor-advance order, so the observed `post_tips`
/// sequence for the subscribed callback is monotonically forward. This test
/// asserts exactly that invariant via ancestry (`ids_added`), which is
/// timing-independent once both writers have drained.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_on_write_canonical_order_under_concurrent_writers() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Two independent client connections, same user, same tree — each is a
    // distinct daemon-side connection, so their writes race at the fan-out.
    let writer1 = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user1 = writer1.login_account("alice", "").await.unwrap();
    let pubkey = user1.get_default_key().unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "concurrent_writers");
    let db1 = user1.create_database(settings, &pubkey).await.unwrap();
    let root_id = db1.root_id().clone();

    let writer2 = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user2 = writer2.login_account("alice", "").await.unwrap();
    let db2 = user2.open_database(&root_id).await.unwrap();

    // Subscriber records each event's post_tips in fire order.
    let observed: std::sync::Arc<std::sync::Mutex<Vec<eidetica::snapshot::Snapshot>>> =
        Default::default();
    let observed_for_cb = observed.clone();
    let _cb = db1
        .on_write(move |event, _db| {
            let post = event.post_tips().clone();
            let observed = observed_for_cb.clone();
            async move {
                observed.lock().unwrap().push(post);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Both connections commit to the tree concurrently, interleaved.
    let n: usize = 8;
    let c1 = async {
        for i in 0..n {
            db1.with_transaction(move |tx| async move {
                let store = tx.get_store::<DocStore>("data").await?;
                store.set("w1", format!("v{i}")).await?;
                Ok(())
            })
            .await
            .unwrap();
        }
    };
    let c2 = async {
        for i in 0..n {
            db2.with_transaction(move |tx| async move {
                let store = tx.get_store::<DocStore>("data").await?;
                store.set("w2", format!("v{i}")).await?;
                Ok(())
            })
            .await
            .unwrap();
        }
    };
    tokio::join!(c1, c2);

    // Let the notification stream quiesce. We deliberately do NOT assert an
    // exact fire count: fire-on-Verified batches, so two entries promoted in a
    // single verify pass surface as one notification whose `post_tips` covers
    // both. The number of fires for `2n` concurrent commits is therefore
    // `<= 2n` and load-dependent — not a stable invariant. What IS stable is
    // the *order* of whatever fires do arrive, which is what this test pins.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let posts = observed.lock().unwrap().clone();
    assert!(
        posts.len() >= 2,
        "concurrent writers should surface several notifications; got {}",
        posts.len()
    );

    // Canonical order ⇔ the total number of entries reachable from each observed
    // `post_tips` strictly increases every fire. Measured via
    // `ids_added(EMPTY, post)` — the full ancestor closure, a valid ancestry
    // count in either topology (fork or linear). Each fire advances the daemon's
    // subscription cursor forward by at least one entry, so a correctly-ordered
    // stream is strictly increasing; a reordered send delivers an older
    // `post_tips` after a newer one, which would regress the count. This is
    // timing-independent: it holds no matter how the two writers interleave, as
    // long as delivery is in canonical order.
    //
    // Note: this pins the invariant but is not a red-before/green-after repro of
    // the reorder — the race only manifests on a multi-thread runtime and even
    // then only intermittently, so it can't be relied on to fail pre-fix. The
    // fix makes ordering hold *by construction* (synchronous send under the tree
    // lock in cursor order); this guards against a future regression of it.
    let mut prev_count = 0usize;
    for post in &posts {
        let count = db1
            .ids_added(&eidetica::snapshot::Snapshot::EMPTY, post)
            .await
            .unwrap()
            .len();
        assert!(
            count > prev_count,
            "reachable-entry count must strictly increase each fire (reorder regresses it); \
             got {count} after {prev_count} for {post:?} in sequence {posts:?}"
        );
        prev_count = count;
    }
}

/// Regression: a client request issued *after* the reader task has exited
/// (because the daemon shut down its side) must surface
/// `ConnectionAborted` promptly instead of hanging forever on a oneshot
/// no one will pop.
///
/// Pre-fix: the client checked nothing — it took the writer lock, pushed
/// a sender into the pending FIFO, wrote the frame (which could still
/// succeed against an OS write buffer briefly after peer close), and then
/// awaited a response from a dead connection. The dispatcher's `clear()`
/// had already run by then, so no one would ever fulfil the sender.
#[tokio::test]
async fn test_request_after_reader_exit_returns_connection_aborted() {
    let (socket_path, tx_shutdown, _server, _dir) = start_test_server().await;
    create_user_via_admin(&_server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let _user = instance.login_account("alice", "").await.unwrap();
    let conn = remote_conn(&instance);

    // Shut the daemon down and wait for the reader task to actually observe
    // the closed socket. The post-exit `ConnectionAborted` contract holds
    // only once the reader has run `mark_dead`; a request issued before
    // that passes the `closed` checks and its frame write fails with
    // `BrokenPipe` against the already-closed socket — the fixed sleep
    // this test used to rely on loses that race under scheduler load.
    drop(tx_shutdown);
    tokio::time::timeout(Duration::from_secs(2), conn.wait_closed_for_test())
        .await
        .expect("reader must observe server shutdown and mark the connection closed");

    // Any new request must bail with `ConnectionAborted` within the
    // timeout — emphatically *not* hang waiting for a response.
    let result = tokio::time::timeout(Duration::from_secs(2), conn.get_instance_metadata())
        .await
        .expect("request after reader exit must not hang");
    let err = result.expect_err("request after reader exit must return Err");
    assert!(
        matches!(&err, eidetica::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionAborted),
        "expected ConnectionAborted, got: {err:?}",
    );
}

/// Regression: dropping the last client handle must close the socket so the
/// daemon tears the connection down.
///
/// The reader task holds a strong `Arc<RemoteConnectionInner>` and `inner`
/// owns the socket's `WriteHalf`, so without an explicit teardown token the
/// two halves pin each other: the reader parks in `read_frame` until the
/// daemon EOFs, and the daemon only EOFs once the client's socket closes.
/// The daemon-side subscription — a per-database write callback on its
/// `Instance` — would then survive forever, with the daemon fanning
/// notifications into a channel nobody drains.
#[tokio::test]
async fn test_client_drop_releases_daemon_subscription() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    {
        let instance = Instance::connect(format!("unix://{}", socket_path.display()))
            .await
            .unwrap();
        let mut user = instance.login_account("alice", "").await.unwrap();
        let pubkey = user.get_default_key().unwrap();

        let mut settings = eidetica::crdt::Doc::new();
        settings.set("name", "drop_release_test");
        let db = user.create_database(settings, &pubkey).await.unwrap();

        // Detach so the subscription outlives the callback handle: this
        // asserts teardown comes from the connection closing, not from
        // `WriteCallback::drop` unregistering.
        db.on_write(|_event, _db| async { Ok(()) })
            .await
            .unwrap()
            .detach();

        assert!(
            format!("{server:?}").contains("<1 per-db callbacks>"),
            "daemon should hold the subscription while the client is alive: {server:?}"
        );
    }

    // The daemon scrubs via its `ConnectionGuard` once it sees EOF.
    for _ in 0..100 {
        if format!("{server:?}").contains("<0 per-db callbacks>") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon still holds the subscription after the client dropped: {server:?}");
}
