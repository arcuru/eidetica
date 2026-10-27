//! Exercise caller completion through real sockets, not just encoder errors.
use super::*;
use crate::service::{ServiceConfig, ServiceServer};
use std::time::Duration;
use tokio::io::AsyncReadExt;

const TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn server(
    config: ServiceConfig,
) -> (
    std::path::PathBuf,
    tokio::sync::watch::Sender<()>,
    tokio::task::JoinHandle<crate::Result<()>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("service.sock");
    let (instance, _) = crate::Instance::create_backend(
        Box::new(crate::backend::database::InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let server = ServiceServer::bind_with_config(instance, &path, config)
        .await
        .unwrap();
    let (tx, rx) = tokio::sync::watch::channel(());
    (path, tx, tokio::spawn(server.run(rx)), dir)
}

fn assert_disconnected<T: std::fmt::Debug>(result: crate::Result<T>) {
    assert!(
        matches!(result, Err(crate::Error::Io(ref e)) if matches!(e.kind(), std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset)),
        "{result:?}"
    );
}

#[tokio::test]
async fn configured_response_limits_complete_multiple_pending_calls() {
    // A metadata response exceeds 128 bytes, but the handshake/requests fit.
    for malicious_reply in [false, true] {
        let (path, shutdown, task, _dir) = server(ServiceConfig::new(128).unwrap()).await;
        let (conn, peer_task) = if malicious_reply {
            let (conn, mut peer) =
                super::tests::connect_peer_with_config(ServiceConfig::new(128).unwrap()).await;
            let task = tokio::spawn(async move {
                for _ in 0..3 {
                    let _: ServiceRequest = read_frame(&mut peer).await.unwrap().unwrap();
                }
                // Deliberately violate the policy the peer advertised.
                write_frame(
                    &mut peer,
                    &ServerFrame::Response(Box::new(ServiceResponse::Token("x".repeat(256)))),
                )
                .await
                .unwrap();
            });
            (conn, Some(task))
        } else {
            (RemoteConnection::connect(&path).await.unwrap(), None)
        };
        let mut calls = (0..3)
            .map(|_| Box::pin(conn.get_instance_metadata()))
            .collect::<Vec<_>>();
        for call in &mut calls {
            assert!(futures_util::poll!(call.as_mut()).is_pending());
        }
        assert_eq!(
            conn.inner.pending_lock().len(),
            3,
            "positive control: three requests on wire"
        );
        let results = tokio::time::timeout(TIMEOUT, futures_util::future::join_all(calls))
            .await
            .expect("oversized response stranded pending RPCs");
        for result in results {
            assert_disconnected(result);
        }
        assert_disconnected(conn.get_instance_metadata().await);
        assert!(conn.inner.pending_lock().is_empty());
        if let Some(task) = peer_task {
            task.await.unwrap();
        }

        // Another connection is unaffected. Unknown v0 operations get a small
        // Unsupported reply even when this server's metadata cannot fit.
        let mut peer = UnixStream::connect(&path).await.unwrap();
        write_frame(
            &mut peer,
            &Handshake {
                protocol_version: 0,
                learns_frame_policy: false,
            },
        )
        .await
        .unwrap();
        let _: HandshakeAck = read_frame(&mut peer).await.unwrap().unwrap();
        write_frame(&mut peer, &"FutureOperation").await.unwrap();
        let frame: ServerFrame = tokio::time::timeout(TIMEOUT, read_frame(&mut peer))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(frame, ServerFrame::Response(r) if matches!(*r, ServiceResponse::Unsupported))
        );
        drop(shutdown);
        tokio::time::timeout(TIMEOUT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn configured_request_mismatch_disconnects_caller() {
    let (path, shutdown, task, _dir) = server(ServiceConfig::new(128).unwrap()).await;
    let conn = RemoteConnection::connect(&path).await.unwrap();
    let result = tokio::time::timeout(
        TIMEOUT,
        conn.request(ServiceRequest::TrustedLoginAccount {
            username: "x".repeat(256),
        }),
    )
    .await
    .expect("learned request policy stranded caller");
    assert!(
        matches!(result, Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_disconnected(conn.get_instance_metadata().await);
    drop(shutdown);
    tokio::time::timeout(TIMEOUT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn local_oversized_encode_fails_existing_pending_without_replay() {
    let (conn, mut peer) =
        super::tests::connect_peer_with_config(ServiceConfig::new(128).unwrap()).await;
    let mut first = Box::pin(conn.get_instance_metadata());
    tokio::select! {
        request = read_frame::<_, ServiceRequest>(&mut peer) => assert!(matches!(request.unwrap(), Some(ServiceRequest::GetInstanceMetadata))),
        result = &mut first => panic!("unexpected response: {result:?}"),
    }
    let result = conn
        .request(ServiceRequest::TrustedLoginAccount {
            username: "\n".repeat(64),
        })
        .await;
    assert!(
        matches!(result, Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_disconnected(
        tokio::time::timeout(TIMEOUT, first)
            .await
            .expect("local refusal stranded earlier call"),
    );
    assert_disconnected(conn.get_instance_metadata().await);
    let mut remaining = Vec::new();
    tokio::time::timeout(TIMEOUT, peer.read_to_end(&mut remaining))
        .await
        .unwrap()
        .unwrap();
    assert!(
        remaining.is_empty(),
        "oversize wrote bytes or replayed earlier request"
    );
}

#[tokio::test]
async fn peer_shutdown_releases_multiple_pending_calls() {
    let (conn, mut peer) = super::tests::connect_peer().await;
    let mut calls = (0..3)
        .map(|_| Box::pin(conn.get_instance_metadata()))
        .collect::<Vec<_>>();
    for call in &mut calls {
        assert!(futures_util::poll!(call.as_mut()).is_pending());
    }
    for _ in 0..3 {
        let _: ServiceRequest = read_frame(&mut peer).await.unwrap().unwrap();
    }
    drop(peer);
    let results = tokio::time::timeout(TIMEOUT, futures_util::future::join_all(calls))
        .await
        .unwrap();
    for result in results {
        assert_disconnected(result);
    }
    assert_disconnected(conn.get_instance_metadata().await);
}

#[tokio::test]
async fn default_instances_preserve_small_calls_and_shutdown() {
    for config in [
        ServiceConfig::default(),
        ServiceConfig::new(1024 * 1024 * 1024).unwrap(),
    ] {
        let (path, shutdown, task, _dir) = server(config).await;
        let url = format!("unix://{}", path.display());
        let first = crate::Instance::connect(&url).await.unwrap();
        let second = crate::Instance::connect(&url).await.unwrap();
        assert_eq!(first.id(), second.id());
        let conn = RemoteConnection::connect(&path).await.unwrap();
        assert_eq!(conn.inner.config, config);
        assert!(conn.get_instance_metadata().await.unwrap().is_some());
        drop(shutdown);
        tokio::time::timeout(TIMEOUT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_disconnected(
            tokio::time::timeout(TIMEOUT, conn.get_instance_metadata())
                .await
                .unwrap(),
        );
    }
}

#[tokio::test]
async fn over_limit_prefix_closes_socket_without_reading_body() {
    let config = ServiceConfig::new(128).unwrap();
    let (conn, mut peer) = super::tests::connect_peer_with_config(config).await;
    let mut call = Box::pin(conn.get_instance_metadata());
    assert!(futures_util::poll!(call.as_mut()).is_pending());
    let _: ServiceRequest = read_frame(&mut peer).await.unwrap().unwrap();
    use tokio::io::AsyncWriteExt;
    peer.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    assert_disconnected(tokio::time::timeout(TIMEOUT, call).await.unwrap());
    // Rejecting the prefix must physically close the socket even while the
    // caller still holds conn, rather than leave a peer writer blocked.
    let mut bytes = Vec::new();
    tokio::time::timeout(TIMEOUT, peer.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn over_limit_prefix_interrupts_blocked_writer_and_pending_calls() {
    use tokio::io::AsyncWriteExt;

    let (conn, mut peer) = super::tests::connect_peer().await;
    let mut first = Box::pin(conn.get_instance_metadata());
    tokio::select! {
        request = read_frame::<_, ServiceRequest>(&mut peer) => assert!(matches!(request.unwrap(), Some(ServiceRequest::GetInstanceMetadata))),
        result = &mut first => panic!("unexpected response: {result:?}"),
    }
    // Fill the send buffer without letting the peer consume the request body.
    let mut sending = Box::pin(conn.request(ServiceRequest::TrustedLoginAccount {
        username: "x".repeat(2 * 1024 * 1024),
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut sending)
            .await
            .is_err()
    );
    let mut prefix = [0; 4];
    peer.read_exact(&mut prefix).await.unwrap();
    let expected = u32::from_be_bytes(prefix) as usize;
    assert!(expected > 1024 * 1024, "frame write never began");
    assert!(
        conn.inner.writer.try_lock().is_err(),
        "writer is not blocked"
    );
    assert_eq!(conn.inner.pending_lock().len(), 2);

    peer.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    let (first, sending) = tokio::time::timeout(TIMEOUT, async { tokio::join!(first, sending) })
        .await
        .expect("prefix rejection stranded a caller behind the writer lock");
    assert_disconnected(first);
    assert_disconnected(sending);
    assert_disconnected(conn.get_instance_metadata().await);
    assert!(conn.inner.pending_lock().is_empty());
    let mut partial = Vec::new();
    tokio::time::timeout(TIMEOUT, peer.read_to_end(&mut partial))
        .await
        .unwrap()
        .unwrap();
    assert!(
        partial.len() < expected,
        "retired writer completed its frame"
    );
}

#[tokio::test]
async fn oversized_write_response_keeps_ambiguous_commit_without_replay() {
    use crate::backend::BackendImpl;
    use tokio::io::AsyncWriteExt;

    let config = ServiceConfig::new(4096).unwrap();
    let (conn, mut peer) = super::tests::connect_peer_with_config(config).await;
    let backend = crate::backend::database::InMemory::new();
    let entry = Entry::root_builder().build().unwrap();
    let id = entry.id();
    let mut call = Box::pin(conn.submit_signed_entry(ID::default(), SigKey::default(), entry));
    let received = tokio::select! {
        request = read_frame::<_, ServiceRequest>(&mut peer) => request.unwrap().unwrap(),
        result = &mut call => panic!("unexpected response before dispatch: {result:?}"),
    };
    // The peer has performed the write before its response becomes unreadable.
    let ServiceRequest::AuthenticatedDb(request) = received else {
        panic!("not database op")
    };
    let DatabaseOp::SubmitSignedEntry { entry } = request.op else {
        panic!("not write")
    };
    backend.put(*entry).await.unwrap();
    peer.write_all(&(config.max_frame_bytes() + 1).to_be_bytes())
        .await
        .unwrap();
    assert_disconnected(tokio::time::timeout(TIMEOUT, call).await.unwrap());
    assert_eq!(
        backend.get(&id).await.unwrap().id(),
        id,
        "transport failure must not roll back a dispatched write"
    );
    assert_disconnected(conn.get_instance_metadata().await);
    let mut trailing = Vec::new();
    tokio::time::timeout(TIMEOUT, peer.read_to_end(&mut trailing))
        .await
        .unwrap()
        .unwrap();
    assert!(
        trailing.is_empty(),
        "ambiguous write was automatically replayed"
    );
}
