//! Exercise caller completion through real sockets, not just encoder errors.
use super::*;
use crate::service::{ServiceConfig, ServiceServer};
use std::time::Duration;
use tokio::io::AsyncReadExt;

const TIMEOUT: Duration = Duration::from_secs(5);

async fn server(
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
    // Cover server-side encode refusal and client-side inbound mismatch.
    for (server_config, client_config) in [
        (ServiceConfig::new(128).unwrap(), ServiceConfig::default()),
        (ServiceConfig::default(), ServiceConfig::new(128).unwrap()),
    ] {
        let (path, shutdown, task, _dir) = server(server_config).await;
        let conn = RemoteConnection::connect_with_config(&path, client_config)
            .await
            .unwrap();
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

        // Another connection is unaffected. Unknown v0 operations get a small
        // Unsupported reply even when this server's metadata cannot fit.
        let mut peer = UnixStream::connect(&path).await.unwrap();
        write_frame(
            &mut peer,
            &Handshake {
                protocol_version: 0,
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
    .expect("server prefix rejection stranded caller");
    assert_disconnected(result);
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
async fn default_and_configured_instances_preserve_small_calls_and_shutdown() {
    let (path, shutdown, task, _dir) = server(ServiceConfig::default()).await;
    let url = format!("unix://{}", path.display());
    let default = crate::Instance::connect(&url).await.unwrap();
    let configured = crate::Instance::connect_with_service_config(
        &url,
        ServiceConfig::new(1024 * 1024 * 1024).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(default.id(), configured.id());
    assert!(
        crate::Instance::connect_with_service_config("memory://", ServiceConfig::default())
            .await
            .is_err()
    );
    let conn = RemoteConnection::connect(&path).await.unwrap();
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
