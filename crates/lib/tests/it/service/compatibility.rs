//! Raw sockets deliberately bypass this client's enum encoder.

use super::*;
use eidetica::service::protocol::{AuthenticatedDbRequest, DatabaseOp, MAX_FRAME_SIZE};
use serde_json::json;

async fn send_raw(writer: &mut WriteHalf<UnixStream>, json: &str) {
    writer
        .write_all(&(json.len() as u32).to_be_bytes())
        .await
        .unwrap();
    writer.write_all(json.as_bytes()).await.unwrap();
}

async fn response(reader: &mut ReadHalf<UnixStream>) -> ServiceResponse {
    tokio::time::timeout(Duration::from_secs(2), read_response(reader))
        .await
        .unwrap()
}

async fn unsupported_then_metadata(
    reader: &mut ReadHalf<UnixStream>,
    writer: &mut WriteHalf<UnixStream>,
    unknown: &str,
    server: &Instance,
) {
    send_raw(writer, unknown).await;
    send_raw(writer, r#""GetInstanceMetadata""#).await;
    assert!(matches!(
        response(reader).await,
        ServiceResponse::Unsupported
    ));
    match response(reader).await {
        ServiceResponse::InstanceMetadata(Some(metadata)) => assert_eq!(metadata.id, server.id()),
        other => panic!("known request after unknown operation failed: {other:?}"),
    }
}

#[tokio::test]
async fn v0_unknown_requests_have_no_effects_across_auth_states() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    let mut admin = server.login_account("admin", None).await.unwrap();
    let pubkey = admin.get_default_key().unwrap();
    let private = admin.get_signing_key(&pubkey).unwrap();
    let db = admin.create_database(Doc::new(), &pubkey).await.unwrap();
    let root = db.root_id().clone();
    let identity = SigKey::from_pubkey(&pubkey);
    let before = db.snapshot().await.unwrap();
    let metadata_before =
        serde_json::to_value(server.backend().get_instance_metadata().await.unwrap()).unwrap();
    let roots_before = server.backend().all_roots().await.unwrap();
    let known_read = ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
        root_id: root.clone(),
        identity: identity.clone(),
        op: DatabaseOp::GetVerifiedTips,
    }));
    let unknown_db = json!({"AuthenticatedDb": {
        "root_id": root, "identity": identity,
        "op": {"FutureWrite": {"SetInstanceMetadata": null, "SubmitSignedEntry": null}}
    }})
    .to_string();
    let unknown_unit_db = json!({"AuthenticatedDb": {
        "root_id": root, "identity": identity, "op": "FutureWrite"
    }})
    .to_string();
    let (mut reader, mut writer) = raw_handshake(&path).await;

    for unknown in [
        r#""FutureOperation""#,
        r#"{"FutureOperation":{"TrustedLoginUser":{"username":"admin"}}}"#,
        &unknown_db,
        &unknown_unit_db,
    ] {
        unsupported_then_metadata(&mut reader, &mut writer, unknown, &server).await;
    }
    // Neither a future operation nor its embedded payload authenticated us.
    write_frame(&mut writer, &known_read).await.unwrap();
    assert!(matches!(
        response(&mut reader).await,
        ServiceResponse::Error(_)
    ));
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0; 64],
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        response(&mut reader).await,
        ServiceResponse::Error(_)
    ));

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginAccount {
            username: "admin".into(),
        },
    )
    .await
    .unwrap();
    let challenge = match response(&mut reader).await {
        ServiceResponse::TrustedLoginChallenge { challenge, .. } => challenge,
        other => panic!("expected challenge: {other:?}"),
    };
    // Unknown operations do not consume or replace the pending login challenge.
    unsupported_then_metadata(&mut reader, &mut writer, &unknown_db, &server).await;
    unsupported_then_metadata(&mut reader, &mut writer, r#""FutureLoginProve""#, &server).await;
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: create_challenge_response(&challenge, &private),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        response(&mut reader).await,
        ServiceResponse::TrustedLoginOk
    ));

    for unknown in [r#""FutureOperation""#, &unknown_db] {
        unsupported_then_metadata(&mut reader, &mut writer, unknown, &server).await;
    }
    write_frame(&mut writer, &known_read).await.unwrap();
    assert!(matches!(response(&mut reader).await, ServiceResponse::Ids(tips) if tips == before));

    // Unknown variants must not enroll an identity or bypass the known keyset gate.
    let other_private = PrivateKey::generate();
    let other_pubkey = other_private.public_key();
    let foreign_read = ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
        root_id: root.clone(),
        identity: SigKey::from_pubkey(&other_pubkey),
        op: DatabaseOp::GetVerifiedTips,
    }));
    let future_register =
        json!({"FutureSessionKeyRegister": {"pubkey": other_pubkey, "signature": [0]}}).to_string();
    unsupported_then_metadata(&mut reader, &mut writer, &future_register, &server).await;
    write_frame(&mut writer, &foreign_read).await.unwrap();
    assert!(matches!(
        response(&mut reader).await,
        ServiceResponse::Error(_)
    ));

    write_frame(
        &mut writer,
        &ServiceRequest::SessionKeyChallenge {
            pubkey: other_pubkey.clone(),
        },
    )
    .await
    .unwrap();
    let challenge = match response(&mut reader).await {
        ServiceResponse::SessionKeyChallenge { challenge } => challenge,
        other => panic!("expected registration challenge: {other:?}"),
    };
    unsupported_then_metadata(&mut reader, &mut writer, &future_register, &server).await;
    let register = ServiceRequest::SessionKeyRegister {
        pubkey: other_pubkey.clone(),
        signature: create_challenge_response(&challenge, &other_private),
    };
    write_frame(&mut writer, &register).await.unwrap();
    assert!(matches!(response(&mut reader).await, ServiceResponse::Ok));
    write_frame(&mut writer, &register).await.unwrap();
    assert!(
        matches!(response(&mut reader).await, ServiceResponse::Error(_)),
        "challenge was reusable"
    );
    // Even an enrolled key without tree membership remains denied.
    write_frame(&mut writer, &foreign_read).await.unwrap();
    assert!(matches!(
        response(&mut reader).await,
        ServiceResponse::Error(_)
    ));

    assert_eq!(
        db.snapshot().await.unwrap(),
        before,
        "unknown operation wrote entries"
    );
    assert_eq!(
        server.backend().all_roots().await.unwrap(),
        roots_before,
        "unknown operation created a database"
    );
    assert_eq!(
        serde_json::to_value(server.backend().get_instance_metadata().await.unwrap()).unwrap(),
        metadata_before
    );
}

#[tokio::test]
async fn v0_server_rejects_malformed_and_required_unknown_messages() {
    let (path, _shutdown, server, _dir) = start_test_server().await;
    for malformed in [
        "{",
        "null",
        "[]",
        "{}",
        r#"{"Future":{},"GetInstanceMetadata":null}"#,
        r#"{"Future":{},"Future":{}}"#,
        r#"{"TrustedLoginUser":{}}"#,
        r#"{"TrustedLoginUser":{"username":4}}"#,
        r#"{"TrustedLoginUser":{"username":"admin","username":"other"}}"#,
        r#"{"TrustedLoginProve":{"signature":"bad"}}"#,
        r#"{"SessionKeyChallenge":{"pubkey":"unknown-key-format"}}"#,
        r#"{"AuthenticatedDb":{"op":"FutureOp"}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","root_id":"","identity":{"Direct":{"hint":{}}},"op":"FutureOp"}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","identity":{"Direct":{"hint":{}}},"op":"FutureOp","op":"GetVerifiedTips"}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","identity":{"Direct":{"hint":{}}},"op":[]}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","identity":{"FutureIdentity":{}},"op":"FutureOp"}}"#,
        r#"{"AuthenticatedDb":{"root_id":"bad-cid","identity":{"Direct":{"hint":{}}},"op":"FutureOp"}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","identity":{"Direct":{"hint":{}}},"op":{"BeginTransaction":{"stores":[],"scope":"FutureScope"}}}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","identity":{"Direct":{"hint":{}}},"op":{"SubmitSignedEntry":{}}}}"#,
        r#"{"AuthenticatedDb":{"root_id":"","identity":{"Direct":{"hint":{}}},"op":{"GetStoreState":{"store":4}}}}"#,
    ] {
        let (mut reader, mut writer) = raw_handshake(&path).await;
        send_raw(&mut writer, malformed).await;
        let closed = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<_, ServerFrame>(&mut reader),
        )
        .await
        .unwrap();
        assert!(
            matches!(closed, Ok(None) | Err(_)),
            "malformed input received a response: {malformed}"
        );
    }
    for (header, body) in [
        ((MAX_FRAME_SIZE + 1).to_be_bytes().to_vec(), vec![]),
        (vec![0, 0], vec![]),
        (50u32.to_be_bytes().to_vec(), b"{\"Future\":".to_vec()),
    ] {
        let (mut reader, mut writer) = raw_handshake(&path).await;
        writer.write_all(&header).await.unwrap();
        writer.write_all(&body).await.unwrap();
        writer.shutdown().await.unwrap();
        let closed = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<_, ServerFrame>(&mut reader),
        )
        .await
        .unwrap();
        assert!(
            matches!(closed, Ok(None) | Err(_)),
            "invalid frame was accepted"
        );
    }
    // Positive control: malformed peers did not break the daemon or its auth flow.
    let client = login_client(&path, "admin").await;
    assert_eq!(client.id(), server.id());
}

#[tokio::test]
async fn v0_handshake_advertises_zero_and_rejects_nonzero() {
    let (path, _shutdown, _server, _dir) = start_test_server().await;
    assert_eq!(PROTOCOL_VERSION, 0);
    for version in [0, 1, u32::MAX] {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        write_frame(&mut stream, &json!({"protocol_version": version}))
            .await
            .unwrap();
        let ack: serde_json::Value = read_frame(&mut stream).await.unwrap().unwrap();
        assert_eq!(ack, json!({"protocol_version": 0}));
        if version == 0 {
            write_frame(&mut stream, &ServiceRequest::GetInstanceMetadata)
                .await
                .unwrap();
            assert!(matches!(
                read_response(&mut stream).await,
                ServiceResponse::InstanceMetadata(Some(_))
            ));
        } else {
            let closed = tokio::time::timeout(
                Duration::from_secs(2),
                read_frame::<_, ServerFrame>(&mut stream),
            )
            .await
            .unwrap();
            assert!(matches!(closed, Ok(None) | Err(_)));
        }
    }
}
