//! History is opt-in; unknown legacy frame-policy fields confer no authority.
use super::*;
use crate::service::protocol::{FEATURE_HISTORY_PAGES, MAX_FRAME_SIZE};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn ack_peer(ack: &str) -> crate::Result<RemoteConnection> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("handshake.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: serde_json::Value = read_frame(&mut stream).await.unwrap().unwrap();
        assert!(request.get("learns_frame_policy").is_none());
        stream
            .write_all(&(ack.len() as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(ack.as_bytes()).await.unwrap();
    };
    let (result, ()) = tokio::join!(RemoteConnection::connect(path), server);
    result
}
#[tokio::test]
async fn legacy_advertised_cap_is_ignored_not_learned() {
    for ack in [
        r#"{"protocol_version":0}"#,
        r#"{"protocol_version":0,"max_frame_bytes":1073741824}"#,
    ] {
        let conn = ack_peer(ack).await.unwrap();
        assert!(!conn.has_feature(FEATURE_HISTORY_PAGES));
    }
}
#[tokio::test]
async fn malformed_and_unoffered_features_refuse_handshake() {
    for ack in [
        r#"{"protocol_version":0,"features":null}"#,
        r#"{"protocol_version":0,"features":["future"]}"#,
        r#"{"protocol_version":0,"features":1}"#,
        r#"{"protocol_version":1}"#,
        r#"[0]"#,
        r#"{"features":[]}"#,
        r#"{"protocol_version":0,"protocol_version":0}"#,
    ] {
        assert!(ack_peer(ack).await.is_err(), "{ack}");
    }
}
#[tokio::test]
async fn fixed_cap_handshake_prefix_refuses_without_body() {
    let (mut reader, mut writer) = tokio::io::duplex(4);
    writer
        .write_all(&(MAX_FRAME_SIZE + 1).to_be_bytes())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            read_frame::<_, Handshake>(&mut reader)
        )
        .await
        .unwrap()
        .is_err()
    );
    let mut prefix = [0; 4];
    assert!(
        tokio::time::timeout(Duration::from_millis(5), writer.read(&mut prefix))
            .await
            .is_err()
    );
}
