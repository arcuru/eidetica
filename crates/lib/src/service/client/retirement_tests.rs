use super::tests::connect_peer;
use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TIMEOUT: Duration = Duration::from_secs(2);
const STALL: Duration = Duration::from_millis(50);

fn assert_aborted<T: std::fmt::Debug>(result: crate::Result<T>) {
    match result {
        Err(crate::Error::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
        }
        other => panic!("expected ConnectionAborted, got {other:?}"),
    }
}

#[tokio::test]
async fn retire_stalled_response_with_retained_clones() {
    let (conn, mut peer) = connect_peer().await;
    let retained = conn.clone();
    let weak = Arc::downgrade(&conn.inner);
    let mut metadata = Box::pin(conn.get_instance_metadata());
    tokio::select! {
        biased;
        result = &mut metadata => panic!("metadata completed without a reply: {result:?}"),
        request = read_frame::<_, ServiceRequest>(&mut peer) => {
            assert!(matches!(request.unwrap().unwrap(), ServiceRequest::GetInstanceMetadata));
        }
    }
    let mut auth = Box::pin(retained.trusted_login("retirement-test", None));
    tokio::select! {
        biased;
        result = &mut auth => panic!("auth completed without a reply: {result:?}"),
        request = read_frame::<_, ServiceRequest>(&mut peer) => {
            assert!(matches!(request.unwrap().unwrap(), ServiceRequest::TrustedLoginAccount { username } if username == "retirement-test"));
        }
    }
    // No-retirement control: both real requests are on the wire and stay
    // pending while the peer withholds responses. The futures are not dropped.
    assert!(
        tokio::time::timeout(STALL, async { tokio::join!(&mut metadata, &mut auth) })
            .await
            .is_err()
    );

    // Even a separately held writer mutex must not obstruct retirement.
    let writer = conn.inner.writer.lock().await;
    retained.retire();
    conn.retire();
    let (metadata_result, auth_result) =
        tokio::time::timeout(TIMEOUT, async { tokio::join!(&mut metadata, &mut auth) })
            .await
            .expect("retirement stranded response waiters");
    assert_aborted(metadata_result);
    assert_aborted(auth_result);
    assert_aborted(retained.get_instance_metadata().await);

    // Retaining every handle and the writer proves physical shutdown, not
    // merely a closed flag or the eventual last-handle Drop path.
    assert!(
        tokio::time::timeout(TIMEOUT, read_frame::<_, ServiceRequest>(&mut peer))
            .await
            .expect("peer did not see EOF")
            .unwrap()
            .is_none()
    );
    assert!(
        peer.write_all(b"after retirement").await.is_err(),
        "retirement did not shut down the socket's read direction"
    );
    drop(writer);
    drop(metadata);
    drop(auth);
    drop(retained);
    drop(conn);
    tokio::time::timeout(TIMEOUT, async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retirement leaked the reader's connection owner");
}

#[tokio::test]
async fn retire_blocked_writer_settles_queued_metadata_and_auth() {
    let (conn, mut peer) = connect_peer().await;
    let retained = conn.clone();
    // Force a real partial frame to occupy the writer mutex. The peer reads
    // only the prefix until after retirement, so the send buffer cannot drain.
    let mut writing = Box::pin(conn.request(ServiceRequest::TrustedLoginAccount {
        username: "x".repeat(2 * 1024 * 1024),
    }));
    assert!(tokio::time::timeout(STALL, &mut writing).await.is_err());
    let expected = tokio::time::timeout(TIMEOUT, peer.read_u32())
        .await
        .unwrap()
        .unwrap() as usize;
    assert!(
        expected > 1024 * 1024,
        "positive control: no large frame began"
    );
    assert!(conn.inner.writer.try_lock().is_err(), "write never blocked");
    let mut metadata = Box::pin(retained.get_instance_metadata());
    let mut auth = Box::pin(retained.trusted_login("queued", None));
    assert!(
        tokio::time::timeout(STALL, async {
            tokio::join!(&mut writing, &mut metadata, &mut auth)
        })
        .await
        .is_err(),
        "no-retirement control unexpectedly settled"
    );

    retained.retire();
    retained.retire();
    let (write_result, metadata_result, auth_result) = tokio::time::timeout(TIMEOUT, async {
        tokio::join!(&mut writing, &mut metadata, &mut auth)
    })
    .await
    .expect("retirement failed to release writer and queued calls");
    assert!(
        matches!(write_result, Err(crate::Error::Io(_))),
        "blocked write did not surface its native I/O error: {write_result:?}"
    );
    assert_aborted(metadata_result);
    assert_aborted(auth_result);
    assert_aborted(conn.get_instance_metadata().await);
    let mut remainder = Vec::new();
    tokio::time::timeout(TIMEOUT, peer.read_to_end(&mut remainder))
        .await
        .expect("blocked writer's peer did not see EOF")
        .unwrap();
    assert!(remainder.len() < expected, "the frame was not partial");
    assert!(conn.inner.pending_lock().is_empty());
    // No later metadata/auth frame can have followed the incomplete frame.
    assert!(
        !remainder
            .windows(b"queued".len())
            .any(|bytes| bytes == b"queued")
    );
}
