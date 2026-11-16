//! Database-scoped fetch is read-authorized even when the bytes are local.
use super::*;
use eidetica::service::protocol::{AuthenticatedDbRequest, DatabaseOp};

#[tokio::test]
async fn blob_fetch_range_keeps_session_identity_and_database_read_gates() {
    let (socket, _shutdown, server, _dir) = start_test_server().await;
    let (_client, root, identity) = setup_db(&server, &socket, "alice").await;
    let (_, foreign_root, _) = setup_db(&server, &socket, "bob").await;
    let cid = server.put_blob(b"held locally".to_vec()).await.unwrap();
    let request = |root_id, identity| {
        ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
            root_id,
            identity,
            op: DatabaseOp::FetchBlobRange {
                cid: cid.clone(),
                start: 0,
                end: 4,
            },
        }))
    };
    let (mut reader, mut writer) = raw_handshake(&socket).await;
    write_frame(&mut writer, &request(root.clone(), identity.clone()))
        .await
        .unwrap();
    assert!(
        matches!(read_response(&mut reader).await, ServiceResponse::Error(_)),
        "unauthenticated fetch must not reach even local bytes"
    );
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginAccount {
            username: "alice".into(),
        },
    )
    .await
    .unwrap();
    let challenge = match read_response(&mut reader).await {
        ServiceResponse::TrustedLoginChallenge { challenge, .. } => challenge,
        other => panic!("{other:?}"),
    };
    let alice = server.login_account("alice", "").await.unwrap();
    let key = alice
        .get_signing_key(&alice.get_default_key().unwrap())
        .unwrap();
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: create_challenge_response(&challenge, &key),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_response(&mut reader).await,
        ServiceResponse::TrustedLoginOk
    ));
    write_frame(&mut writer, &request(root.clone(), identity.clone()))
        .await
        .unwrap();
    assert!(
        matches!(read_response(&mut reader).await, ServiceResponse::Blob(Some(bytes)) if bytes == b"held")
    );
    // A valid logged-in identity cannot read through another Database's context.
    write_frame(&mut writer, &request(foreign_root, identity))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut reader).await,
        ServiceResponse::Error(_)
    ));
    // Nor can the session claim a foreign identity, even on an otherwise valid root.
    write_frame(
        &mut writer,
        &request(root.clone(), SigKey::from_pubkey(&generate_keypair().1)),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_response(&mut reader).await,
        ServiceResponse::Error(_)
    ));
    // The new operation is a Read; it must not grow local mutation authority.
    assert_eq!(
        DatabaseOp::FetchBlobRange {
            cid: cid.clone(),
            start: 0,
            end: 4
        }
        .required_permission(),
        Permission::Read
    );
    let local = eidetica::Database::open(&server, &root)
        .await
        .unwrap()
        .with_key(generate_keypair().0);
    assert!(
        local.get_blob_range(&cid, 0..4).await.is_err(),
        "embedded Database read must authorize before consulting local bytes"
    );
}
