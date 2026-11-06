//! Controlled-clock tests of the production FIFO reader, independent of socket
//! scheduling. The silent-peer integration test also covers real request upload.
use super::*;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};

fn pending(
    conn: &RemoteConnection,
    complete: bool,
) -> (
    Arc<AtomicBool>,
    oneshot::Receiver<Result<ServiceResponse, TransferError>>,
) {
    let (response, rx) = oneshot::channel();
    let uploaded = Arc::new(AtomicBool::new(complete));
    conn.inner.pending_lock().push_back(PendingResponse {
        response,
        uploaded: uploaded.clone(),
        deadline: None,
    });
    conn.inner.pending_changed.notify_one();
    (uploaded, rx)
}

async fn tick(seconds: u64) {
    tokio::time::advance(Duration::from_secs(seconds)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

fn envelope(value: &impl serde::Serialize) -> Vec<u8> {
    let body = cbor4ii::serde::to_vec(Vec::new(), value).unwrap();
    let mut frame = (body.len() as u64).to_be_bytes().to_vec();
    frame.extend(body);
    frame
}

async fn notification(peer: &mut DuplexStream) {
    // Unknown optional notifications consume no FIFO slot.
    peer.write_all(&envelope(&serde_json::json!({"Notification":{"Future":1}})))
        .await
        .unwrap();
    tokio::task::yield_now().await;
}

#[tokio::test]
async fn explicit_overall_deadline_still_bounds_the_call_without_shifting_fifo() {
    let policy = crate::message_io::TransferPolicy {
        overall_timeout: Some(Duration::from_millis(20)),
        ..Default::default()
    };
    let (conn, mut peer) =
        tests::connect_peer_with_budget(TransferBudget::new(policy).unwrap()).await;
    let request = conn.request(ServiceRequest::TrustedLoginAccount {
        username: "deadline".into(),
    });
    let consume = tests::read_request(&mut peer, "deadline");
    let (result, ()) = tokio::join!(request, consume);
    assert!(
        matches!(result, Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut)
    );
    assert!(!conn.inner.closed.load(Ordering::Acquire));
    assert_eq!(conn.inner.pending_lock().len(), 1);
    peer.write_all(&envelope(&ServerFrame::Response(Box::new(
        ServiceResponse::Ok,
    ))))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !conn.inner.pending_lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the canceled deadline's response must be consumed");
}

#[tokio::test(start_paused = true)]
async fn idle_and_incomplete_uploads_are_untimed_then_front_timeout_retires_fifo() {
    let (conn, _socket) = tests::test_conn();
    let (_peer, reader) = duplex(64);
    let reader = tokio::spawn(run_reader_task(reader, conn.inner.clone()));
    tick(60).await;
    assert!(!conn.inner.closed.load(Ordering::Acquire));
    let (uploaded, mut first) = pending(&conn, false);
    let (_, second) = pending(&conn, true);
    tick(60).await;
    assert!(matches!(
        first.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    uploaded.store(true, Ordering::Release);
    conn.inner.pending_changed.notify_one();
    tick(0).await;
    tick(29).await;
    assert!(!conn.inner.closed.load(Ordering::Acquire));
    tick(1).await;
    assert!(matches!(
        first.await.unwrap(),
        Err(TransferError::ResponseTimeout { .. })
    ));
    assert!(second.await.is_err());
    reader.await.unwrap();
    assert!(conn.inner.closed.load(Ordering::Acquire));
    assert!(conn.inner.pending_lock().is_empty());
}

#[tokio::test(start_paused = true)]
async fn notifications_do_not_renew_a_front_response_deadline() {
    let (conn, _socket) = tests::test_conn();
    let (mut peer, reader) = duplex(64);
    let (_, response) = pending(&conn, true);
    let reader = tokio::spawn(run_reader_task(reader, conn.inner.clone()));
    tick(0).await;
    for _ in 0..5 {
        tick(5).await;
        notification(&mut peer).await;
    }
    tick(5).await;
    assert!(matches!(
        response.await.unwrap(),
        Err(TransferError::ResponseTimeout { .. })
    ));
    reader.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn queue_changes_do_not_restart_a_partially_read_notification() {
    let (conn, _socket) = tests::test_conn();
    let (mut peer, reader) = duplex(8);
    let reader = tokio::spawn(run_reader_task(reader, conn.inner.clone()));
    let frame = envelope(&serde_json::json!({"Notification":{"Future":1}}));
    peer.write_all(&frame[..4]).await.unwrap();
    tick(0).await;
    let (_, response) = pending(&conn, true);
    tick(0).await;
    peer.write_all(&frame[4..]).await.unwrap();
    peer.write_all(&envelope(&ServerFrame::Response(Box::new(
        ServiceResponse::Ok,
    ))))
    .await
    .unwrap();
    assert!(matches!(
        response.await.unwrap().unwrap(),
        ServiceResponse::Ok
    ));
    drop(peer);
    reader.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn later_and_cancelled_slots_survive_a_healthy_bulk_response_past_thirty_seconds() {
    let (conn, _socket) = tests::test_conn();
    let (mut peer, reader) = duplex(8 * 1024);
    let (_, first) = pending(&conn, true);
    let (_, second) = pending(&conn, true);
    drop(first); // This response must still be consumed in full.
    let reader = tokio::spawn(run_reader_task(reader, conn.inner.clone()));
    let frame = envelope(&ServerFrame::Response(Box::new(ServiceResponse::Record(
        Some(vec![7; 5 * 1024 * 1024]),
    ))));
    let started = tokio::time::Instant::now();
    for chunk in frame.chunks(8 * 1024) {
        peer.write_all(chunk).await.unwrap();
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    peer.write_all(&envelope(&ServerFrame::Response(Box::new(
        ServiceResponse::SessionKeyChallenge { challenge: vec![2] },
    ))))
    .await
    .unwrap();
    assert!(started.elapsed() > Duration::from_secs(30));
    assert!(
        matches!(second.await.unwrap().unwrap(), ServiceResponse::SessionKeyChallenge { challenge } if challenge == vec![2])
    );
    assert!(!conn.inner.closed.load(Ordering::Acquire));
    drop(peer);
    reader.await.unwrap();
}
