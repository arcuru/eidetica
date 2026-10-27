//! Frame policy is learned once per connection, before any normal RPC.
use super::*;
use crate::service::protocol::{MAX_FRAME_SIZE, MAX_HANDSHAKE_BYTES};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn legacy_daemon_and_changed_policy_are_learned_per_connection() {
    // The exact old serde shape ignores the new client's support field.
    #[derive(serde::Deserialize)]
    struct LegacyHandshake {
        protocol_version: u32,
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reconnect.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let maximum = u64::from(u32::MAX).min(isize::MAX as u64);
    let limits = [
        None,
        Some(128),
        Some(2 * 1024 * 1024 * 1024 - 1),
        Some(maximum),
        Some(4096),
    ];
    let peer = async {
        for bytes in limits {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handshake: LegacyHandshake = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(handshake.protocol_version, 0);
            let mut ack = serde_json::to_vec(&HandshakeAck {
                protocol_version: 0,
                max_frame_bytes: bytes,
            })
            .unwrap();
            if bytes.is_none() {
                // Exactly the bootstrap bound is accepted; one byte more is
                // rejected by the prefix-only test below.
                ack.resize(MAX_HANDSHAKE_BYTES as usize, b' ');
            }
            stream
                .write_all(&(ack.len() as u32).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&ack).await.unwrap();
            assert!(matches!(
                read_frame(&mut stream).await.unwrap(),
                Some(ServiceRequest::GetInstanceMetadata)
            ));
            write_frame(
                &mut stream,
                &ServerFrame::Response(Box::new(ServiceResponse::InstanceMetadata(None))),
            )
            .await
            .unwrap();
            // Closing a connection must not replay its request on the next one.
            assert!(
                read_frame::<_, ServiceRequest>(&mut stream)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    };
    let client = async {
        for bytes in limits {
            let conn = RemoteConnection::connect(&path).await.unwrap();
            assert_eq!(
                u64::from(conn.inner.config.max_frame_bytes()),
                bytes.unwrap_or(u64::from(MAX_FRAME_SIZE))
            );
            assert!(conn.get_instance_metadata().await.unwrap().is_none());
            drop(conn);
        }
    };
    tokio::time::timeout(TIMEOUT, async { tokio::join!(client, peer) })
        .await
        .unwrap();
}

#[tokio::test]
async fn malformed_advertised_policy_fails_before_normal_requests() {
    for ack in [
        r#"{"protocol_version":0,"max_frame_bytes":0}"#,
        r#"{"protocol_version":0,"max_frame_bytes":4294967296}"#,
        r#"{"protocol_version":0,"max_frame_bytes":18446744073709551616}"#,
        r#"{"protocol_version":0,"max_frame_bytes":-1}"#,
        r#"{"protocol_version":0,"max_frame_bytes":1.5}"#,
        r#"{"protocol_version":0,"max_frame_bytes":"4096"}"#,
        r#"{"protocol_version":0,"max_frame_bytes":null}"#,
        r#"{"protocol_version":0,"max_frame_bytes":true}"#,
        r#"{"protocol_version":0,"max_frame_bytes":{}}"#,
        r#"{"protocol_version":0,"max_frame_bytes":[]}"#,
        r#"{"protocol_version":0,"max_frame_bytes":128,"max_frame_bytes":256}"#,
        r#"{"max_frame_bytes":128}"#,
        r#"{"protocol_version":1,"max_frame_bytes":4294967295}"#,
        r#"{"protocol_version":0"#,
        r#"{"unexpected":4096}"#,
        r#"[0,4096]"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("malformed.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let peer = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handshake: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
            assert!(handshake.learns_frame_policy);
            stream
                .write_all(&(ack.len() as u32).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(ack.as_bytes()).await.unwrap();
            let mut trailing = Vec::new();
            stream.read_to_end(&mut trailing).await.unwrap();
            assert!(trailing.is_empty(), "RPC sent after invalid ack: {ack}");
        };
        let (result, ()) = tokio::time::timeout(TIMEOUT, async {
            tokio::join!(RemoteConnection::connect(&path), peer)
        })
        .await
        .unwrap();
        assert!(result.is_err(), "accepted malformed ack: {ack}");
    }
}

#[tokio::test]
async fn bootstrap_ack_prefix_is_bounded_before_payload() {
    for length in [MAX_HANDSHAKE_BYTES + 1, MAX_FRAME_SIZE, u32::MAX] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefix.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let peer = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
            stream.write_all(&length.to_be_bytes()).await.unwrap();
            // No body: the client must reject and close, not wait or allocate.
            assert_eq!(
                stream.read_u8().await.unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        };
        let (result, ()) = tokio::time::timeout(TIMEOUT, async {
            tokio::join!(RemoteConnection::connect(&path), peer)
        })
        .await
        .unwrap();
        assert!(
            matches!(result, Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData)
        );
    }
}

#[tokio::test]
async fn legacy_client_gets_daemon_smaller_cap_and_bounded_bootstrap() {
    for bytes in [128, MAX_FRAME_SIZE, 1024 * 1024 * 1024] {
        let (path, shutdown, task, _dir) =
            super::frame_limit_tests::server(ServiceConfig::new(u64::from(bytes)).unwrap()).await;
        let mut stream = UnixStream::connect(&path).await.unwrap();
        // Exact legacy wire fixture and exact legacy response shape.
        write_frame(&mut stream, &serde_json::json!({"protocol_version":0}))
            .await
            .unwrap();
        let ack: serde_json::Value = read_frame(&mut stream).await.unwrap().unwrap();
        assert_eq!(ack, serde_json::json!({"protocol_version":0}));
        write_frame(&mut stream, &"FutureOperation").await.unwrap();
        assert!(
            matches!(read_frame::<_, ServerFrame>(&mut stream).await.unwrap(), Some(ServerFrame::Response(r)) if matches!(*r, ServiceResponse::Unsupported))
        );
        // Server must check its smaller cap before waiting for a body.
        let limit = bytes.min(MAX_FRAME_SIZE);
        stream.write_all(&(limit + 1).to_be_bytes()).await.unwrap();
        assert_eq!(
            tokio::time::timeout(TIMEOUT, stream.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        for prefix in [bytes.min(MAX_HANDSHAKE_BYTES) + 1, u32::MAX] {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            stream.write_all(&prefix.to_be_bytes()).await.unwrap();
            assert_eq!(
                tokio::time::timeout(TIMEOUT, stream.read_u8())
                    .await
                    .unwrap()
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
        for invalid_support in ["null", "0", "\"true\"", "{}"] {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            let payload =
                format!("{{\"protocol_version\":0,\"learns_frame_policy\":{invalid_support}}}");
            stream
                .write_all(&(payload.len() as u32).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(payload.as_bytes()).await.unwrap();
            assert_eq!(
                tokio::time::timeout(TIMEOUT, stream.read_u8())
                    .await
                    .unwrap()
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
        drop(shutdown);
        tokio::time::timeout(TIMEOUT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
