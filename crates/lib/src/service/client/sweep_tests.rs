//! The idle-watch sweep must distinguish local queueing from a silent peer.

use super::tests::connect_peer;
use super::*;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);
/// Stands in for `UNSUBSCRIBE_RTT_TIMEOUT` so tests need not wait five seconds.
const BUDGET: Duration = Duration::from_millis(50);

fn insert_expired(conn: &RemoteConnection, id: CallbackId) {
    let idle_since = std::time::Instant::now()
        .checked_sub(idle_grace_window() + Duration::from_secs(1))
        .expect("monotonic clock predates the grace window");
    conn.inner.subscriptions.lock().unwrap().insert(
        id,
        RemoteSubscription {
            tree: ID::default(),
            identity: SigKey::default(),
            idle_since: Some(idle_since),
        },
    );
}

fn read(conn: &RemoteConnection) -> impl Future<Output = crate::Result<Vec<Entry>>> + '_ {
    conn.get_store_entries(
        ID::default(),
        SigKey::default(),
        "data".into(),
        vec![],
        ReadScope::Verified,
    )
}

async fn next_op(peer: &mut UnixStream) -> DatabaseOp {
    let request: ServiceRequest = tokio::time::timeout(TIMEOUT, read_frame(peer))
        .await
        .expect("peer received no request")
        .unwrap()
        .expect("client closed the connection");
    match request {
        ServiceRequest::AuthenticatedDb(request) => request.op,
        other => panic!("expected an authenticated request, got {other:?}"),
    }
}

async fn respond(peer: &mut UnixStream, response: ServiceResponse) {
    write_frame(peer, &ServerFrame::Response(Box::new(response)))
        .await
        .unwrap();
}

#[tokio::test]
async fn expired_watch_waits_for_in_flight_read_without_retiring_connection() {
    let (conn, mut peer) = connect_peer().await;
    let id = CallbackId(7);
    insert_expired(&conn, id);
    let mut held = Box::pin(read(&conn));
    tokio::select! {
        biased;
        result = &mut held => panic!("read completed without a reply: {result:?}"),
        op = next_op(&mut peer) => assert!(matches!(op, DatabaseOp::GetStoreEntries { .. })),
    }
    let mut sweep = tokio::spawn(sweep_expired(conn.inner.clone(), BUDGET));

    // Hold the authenticated read well past the cleanup budget.
    tokio::select! {
        result = &mut held => panic!("held read settled before its reply: {result:?}"),
        _ = tokio::time::sleep(BUDGET * 4) => {}
    }
    assert!(
        !conn.inner.closed.load(Ordering::Acquire),
        "local admission delay retired a healthy connection"
    );
    assert!(!sweep.is_finished(), "sweep abandoned the pending cleanup");

    respond(&mut peer, ServiceResponse::Entries(vec![])).await;
    tokio::time::timeout(TIMEOUT, held)
        .await
        .expect("released read did not complete")
        .expect("released read failed");

    assert!(matches!(
        next_op(&mut peer).await,
        DatabaseOp::UnsubscribeWrites { subscription_id: 7 }
    ));
    respond(&mut peer, ServiceResponse::Ok).await;
    assert!(
        tokio::time::timeout(TIMEOUT, &mut sweep)
            .await
            .expect("sweep did not finish")
            .unwrap(),
        "acknowledged cleanup reported a dead connection"
    );
    assert!(!conn.inner.subscriptions.lock().unwrap().contains_key(&id));

    let (next, op) = tokio::join!(read(&conn), async {
        let op = next_op(&mut peer).await;
        respond(&mut peer, ServiceResponse::Entries(vec![])).await;
        op
    });
    assert!(matches!(op, DatabaseOp::GetStoreEntries { .. }));
    next.expect("same connection could not serve another operation");
}

#[tokio::test]
async fn submitted_unsubscribe_without_reply_retires_connection() {
    let (conn, mut peer) = connect_peer().await;
    insert_expired(&conn, CallbackId(8));
    let sweep = tokio::spawn(sweep_expired(conn.inner.clone(), BUDGET));
    assert!(matches!(
        next_op(&mut peer).await,
        DatabaseOp::UnsubscribeWrites { subscription_id: 8 }
    ));
    // The peer withholds the reply: a genuinely unresponsive daemon.
    let alive = tokio::time::timeout(TIMEOUT, sweep)
        .await
        .expect("silent peer stranded the sweep")
        .unwrap();
    assert!(!alive);
    assert!(conn.inner.closed.load(Ordering::Acquire));
    assert!(read(&conn).await.is_err());
    assert!(
        tokio::time::timeout(TIMEOUT, read_frame::<_, ServiceRequest>(&mut peer))
            .await
            .expect("peer did not see EOF")
            .unwrap()
            .is_none(),
        "unsubscribe was replayed or the socket stayed open"
    );
}

#[tokio::test]
async fn retirement_releases_sweep_awaiting_admission() {
    let (conn, mut peer) = connect_peer().await;
    insert_expired(&conn, CallbackId(9));
    let mut held = Box::pin(read(&conn));
    tokio::select! {
        biased;
        result = &mut held => panic!("read completed without a reply: {result:?}"),
        op = next_op(&mut peer) => assert!(matches!(op, DatabaseOp::GetStoreEntries { .. })),
    }
    let sweep = tokio::spawn(sweep_expired(conn.inner.clone(), BUDGET));
    tokio::time::sleep(BUDGET * 2).await;
    conn.retire();
    // The read's owner keeps awaiting it; its failure releases admission.
    let (read, alive) = tokio::time::timeout(TIMEOUT, async { tokio::join!(held, sweep) })
        .await
        .expect("retirement stranded the sweep");
    assert!(read.is_err());
    assert!(!alive.unwrap(), "sweep did not observe retirement");
    assert!(
        tokio::time::timeout(TIMEOUT, read_frame::<_, ServiceRequest>(&mut peer))
            .await
            .expect("peer did not see EOF")
            .unwrap()
            .is_none(),
        "sweep submitted cleanup on a retired connection"
    );
}
