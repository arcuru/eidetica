use super::tests::{connect_peer, read_request};
use super::*;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(2);

async fn send_raw(peer: &mut UnixStream, json: &str) {
    peer.write_all(&(json.len() as u32).to_be_bytes())
        .await
        .unwrap();
    peer.write_all(json.as_bytes()).await.unwrap();
}

fn start_call(
    conn: &RemoteConnection,
    label: &str,
) -> tokio::task::JoinHandle<crate::Result<ServiceResponse>> {
    let conn = conn.clone();
    let username = label.to_owned();
    tokio::spawn(async move {
        conn.request_ok(ServiceRequest::TrustedLoginAccount { username })
            .await
    })
}

fn assert_unsupported(result: crate::Result<ServiceResponse>) {
    match result {
        Err(crate::Error::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        }
        other => panic!("expected Unsupported I/O error, got {other:?}"),
    }
}

#[tokio::test]
async fn v0_unsupported_and_unknown_responses_consume_exactly_one_fifo_slot() {
    for unsupported in [
        r#"{"Response":"Unsupported"}"#,
        r#"{"Response":"FutureResponse"}"#,
        r#"{"Response":{"FutureResponse":{"payload":[1,2]}}}"#,
    ] {
        let (conn, mut peer) = connect_peer().await;
        let first = start_call(&conn, "first");
        read_request(&mut peer, "first").await;
        let second = start_call(&conn, "second");
        read_request(&mut peer, "second").await;
        send_raw(
            &mut peer,
            r#"{"Notification":{"FutureNotice":{"value":true}}}"#,
        )
        .await;
        send_raw(&mut peer, unsupported).await;
        send_raw(
            &mut peer,
            r#"{"Response":{"SessionKeyChallenge":{"challenge":[2]}}}"#,
        )
        .await;
        assert_unsupported(tokio::time::timeout(TIMEOUT, first).await.unwrap().unwrap());
        let second = tokio::time::timeout(TIMEOUT, second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(second, ServiceResponse::SessionKeyChallenge { challenge } if challenge == vec![2])
        );
        assert!(!conn.inner.closed.load(Ordering::Acquire));
        assert!(conn.inner.pending_lock().is_empty());
        assert!(conn.inner.session_read().is_none());
        assert!(conn.inner.registered_keys.lock().await.is_empty());
        // No response-to-response loop or implicit retry: only this explicit third call arrives.
        let third = start_call(&conn, "third");
        read_request(&mut peer, "third").await;
        send_raw(&mut peer, r#"{"Response":"Ok"}"#).await;
        assert!(matches!(
            tokio::time::timeout(TIMEOUT, third)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ServiceResponse::Ok
        ));
    }
}

#[tokio::test]
async fn v0_abandoned_unknown_response_keeps_later_fifo_association() {
    for unsupported in [
        r#"{"Response":"Unsupported"}"#,
        r#"{"Response":{"FutureResponse":null}}"#,
    ] {
        let (conn, mut peer) = connect_peer().await;
        let first = start_call(&conn, "abandoned");
        read_request(&mut peer, "abandoned").await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(
            conn.inner.pending_lock().len(),
            1,
            "abandoned slot disappeared"
        );
        let second = start_call(&conn, "second");
        read_request(&mut peer, "second").await;
        send_raw(&mut peer, r#"{"Notification":"FutureNotice"}"#).await;
        send_raw(&mut peer, unsupported).await;
        send_raw(
            &mut peer,
            r#"{"Response":{"SessionKeyChallenge":{"challenge":[2]}}}"#,
        )
        .await;
        let response = tokio::time::timeout(TIMEOUT, second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(response, ServiceResponse::SessionKeyChallenge { challenge } if challenge == vec![2])
        );
        assert!(conn.inner.pending_lock().is_empty());
        assert!(!conn.inner.closed.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn v0_unknown_notification_does_not_consume_response_or_invoke_callback() {
    let (conn, mut peer) = connect_peer().await;
    let (instance, _admin) = crate::Instance::create_backend(
        Box::new(crate::backend::database::InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let db = crate::Database::create(&instance, PrivateKey::generate(), crate::crdt::Doc::new())
        .await
        .unwrap();
    let root = db.root_id().clone();
    conn.attach_instance(instance.downgrade());
    let (tx, mut rx) = mpsc::unbounded_channel();
    instance.register_global_write_callback(move |event, _db| {
        let tx = tx.clone();
        let source = event.source();
        async move {
            tx.send(source).unwrap();
            Ok(())
        }
    });
    let call = start_call(&conn, "waiting");
    read_request(&mut peer, "waiting").await;
    send_raw(
        &mut peer,
        &serde_json::json!({"Notification":{"FutureNotice":{
            "root_id":root, "previous_tips":[], "post_tips":[], "source":"Remote"
        }}})
        .to_string(),
    )
    .await;
    send_raw(&mut peer, r#"{"Notification":"FutureNotice"}"#).await;
    // A known response is a reader barrier for preceding ignored notifications.
    send_raw(&mut peer, r#"{"Response":"Ok"}"#).await;
    assert!(matches!(
        tokio::time::timeout(TIMEOUT, call)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ServiceResponse::Ok
    ));
    assert!(conn.inner.tree_workers.lock().unwrap().is_empty());
    assert!(
        rx.try_recv().is_err(),
        "unknown notification invoked a callback"
    );
    let call = start_call(&conn, "next");
    read_request(&mut peer, "next").await;
    write_frame(
        &mut peer,
        &ServerFrame::Notification(Notification::DatabaseWrite {
            root_id: root,
            previous_tips: Snapshot::EMPTY,
            post_tips: db.snapshot().await.unwrap(),
            source: crate::WriteSource::Remote,
        }),
    )
    .await
    .unwrap();
    send_raw(&mut peer, r#"{"Response":"Ok"}"#).await;
    assert!(matches!(
        tokio::time::timeout(TIMEOUT, call)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ServiceResponse::Ok
    ));
    assert_eq!(
        tokio::time::timeout(TIMEOUT, rx.recv()).await.unwrap(),
        Some(crate::WriteSource::Remote)
    );
    assert!(rx.try_recv().is_err());
}

async fn assert_connection_failed(
    conn: &RemoteConnection,
    call: tokio::task::JoinHandle<crate::Result<ServiceResponse>>,
) {
    let result = tokio::time::timeout(TIMEOUT, call).await.unwrap().unwrap();
    match result {
        Err(crate::Error::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
        }
        other => panic!("expected connection-aborted I/O error, got {other:?}"),
    }
    assert!(conn.inner.closed.load(Ordering::Acquire));
    assert!(conn.inner.pending_lock().is_empty());
    assert!(
        conn.request_ok(ServiceRequest::GetInstanceMetadata)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn v0_client_fails_closed_on_malformed_or_required_unknown_frames() {
    for malformed in [
        "{",
        "null",
        "[]",
        "{}",
        r#""FutureEnvelope""#,
        r#"{"FutureEnvelope":{}}"#,
        r#"{"Response":"Ok","Notification":"FutureNotice"}"#,
        r#"{"Response":"Ok","Response":"Ok"}"#,
        r#"{"Response":{}}"#,
        r#"{"Response":{"Future":{},"Ok":null}}"#,
        r#"{"Response":{"Ids":"bad"}}"#,
        r#"{"Response":{"Unsupported":{}}}"#,
        r#"{"Response":{"SessionKeyChallenge":{"challenge":"bad"}}}"#,
        r#"{"Notification":{"DatabaseWrite":{"root_id":"","previous_tips":[],"post_tips":[],"source":"FutureSource"}}}"#,
        r#"{"Notification":{"DatabaseWrite":{"root_id":"","previous_tips":[],"post_tips":[]}}}"#,
    ] {
        let (conn, mut peer) = connect_peer().await;
        let first = start_call(&conn, "first");
        read_request(&mut peer, "first").await;
        let second = start_call(&conn, "second");
        read_request(&mut peer, "second").await;
        send_raw(&mut peer, malformed).await;
        assert_connection_failed(&conn, first).await;
        assert_connection_failed(&conn, second).await;
    }
}

#[tokio::test]
async fn v0_client_fails_closed_on_oversize_and_truncated_frames() {
    for (header, body) in [
        (
            (crate::service::protocol::MAX_FRAME_SIZE + 1)
                .to_be_bytes()
                .to_vec(),
            vec![],
        ),
        (vec![0, 0], vec![]),
        (50u32.to_be_bytes().to_vec(), b"{\"Response\":".to_vec()),
    ] {
        let (conn, mut peer) = connect_peer().await;
        let call = start_call(&conn, "waiting");
        read_request(&mut peer, "waiting").await;
        peer.write_all(&header).await.unwrap();
        peer.write_all(&body).await.unwrap();
        peer.shutdown().await.unwrap();
        assert_connection_failed(&conn, call).await;
    }
}

#[tokio::test]
async fn v0_unsupported_login_does_not_establish_a_session() {
    for unsupported in [
        r#"{"Response":"Unsupported"}"#,
        r#"{"Response":{"FutureLoginChallenge":{}}}"#,
    ] {
        let (conn, mut peer) = connect_peer().await;
        let login_conn = conn.clone();
        let login = tokio::spawn(async move { login_conn.trusted_login("alice", None).await });
        read_request(&mut peer, "alice").await;
        send_raw(&mut peer, unsupported).await;
        let result = tokio::time::timeout(TIMEOUT, login).await.unwrap().unwrap();
        assert!(
            matches!(result, Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported)
        );
        assert!(conn.inner.session_read().is_none());
        assert!(conn.inner.registered_keys.lock().await.is_empty());
        assert!(!conn.inner.closed.load(Ordering::Acquire));
    }
}

const LOGIN_CHALLENGE: &[u8] = b"login challenge";
const KEY_CHALLENGE: &[u8] = b"session key challenge";

fn passwordless_login_challenge(key: &PrivateKey) -> ServerFrame {
    use crate::account::{AccountCredentials, AccountStatus};

    ServerFrame::Response(Box::new(ServiceResponse::TrustedLoginChallenge {
        challenge: LOGIN_CHALLENGE.to_vec(),
        login_id: "uuid-alice".to_owned(),
        account_info: AccountInfo {
            username: "alice".to_owned(),
            account_id: ID::from_bytes("alice-db"),
            credentials: AccountCredentials {
                root_key_id: key.public_key(),
                root_key: KeyStorage::Unencrypted { key: key.clone() },
                password_salt: None,
            },
            created_at: 1_700_000_000,
            status: AccountStatus::Active,
        },
    }))
}

async fn send_login_challenge_and_check_proof(peer: &mut UnixStream, key: &PrivateKey) {
    read_request(peer, "alice").await;
    write_frame(peer, &passwordless_login_challenge(key))
        .await
        .unwrap();
    let proof: ServiceRequest = tokio::time::timeout(TIMEOUT, read_frame(peer))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ServiceRequest::TrustedLoginProve { signature } = proof else {
        panic!("expected login proof");
    };
    key.public_key()
        .verify(LOGIN_CHALLENGE, &signature)
        .unwrap();
}

async fn assert_next_metadata_call(conn: &RemoteConnection, peer: &mut UnixStream) {
    let next_conn = conn.clone();
    let next = tokio::spawn(async move {
        next_conn
            .request_ok(ServiceRequest::GetInstanceMetadata)
            .await
    });
    let request: ServiceRequest = tokio::time::timeout(TIMEOUT, read_frame(peer))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(request, ServiceRequest::GetInstanceMetadata));
    send_raw(peer, r#"{"Response":{"InstanceMetadata":null}}"#).await;
    assert!(matches!(
        tokio::time::timeout(TIMEOUT, next)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ServiceResponse::InstanceMetadata(None)
    ));
    assert!(!conn.inner.closed.load(Ordering::Acquire));
    assert!(conn.inner.pending_lock().is_empty());
}

#[tokio::test]
async fn v0_unsupported_login_proof_does_not_establish_a_session() {
    for unsupported in [
        r#"{"Response":"Unsupported"}"#,
        r#"{"Response":{"FutureLoginOk":{}}}"#,
    ] {
        let (conn, mut peer) = connect_peer().await;
        let key = PrivateKey::generate();
        let login_conn = conn.clone();
        let login = tokio::spawn(async move {
            login_conn
                .trusted_login("alice", None)
                .await
                .map(|_| ServiceResponse::TrustedLoginOk)
        });
        send_login_challenge_and_check_proof(&mut peer, &key).await;
        assert!(conn.inner.session_read().is_none());
        assert!(conn.inner.registered_keys.lock().await.is_empty());
        send_raw(&mut peer, unsupported).await;
        assert_unsupported(tokio::time::timeout(TIMEOUT, login).await.unwrap().unwrap());
        assert!(conn.inner.session_read().is_none());
        assert!(conn.inner.registered_keys.lock().await.is_empty());
        assert!(conn.inner.pending_lock().is_empty());
        assert_next_metadata_call(&conn, &mut peer).await;
    }
}

#[tokio::test]
async fn v0_unsupported_key_registration_preserves_session_and_retries() {
    for fail_after_challenge in [false, true] {
        for unsupported in [
            r#"{"Response":"Unsupported"}"#,
            r#"{"Response":{"FutureKeyResponse":{}}}"#,
        ] {
            let (conn, mut peer) = connect_peer().await;
            let login_key = PrivateKey::generate();
            let login_conn = conn.clone();
            let login = tokio::spawn(async move { login_conn.trusted_login("alice", None).await });
            send_login_challenge_and_check_proof(&mut peer, &login_key).await;
            send_raw(&mut peer, r#"{"Response":"TrustedLoginOk"}"#).await;
            let (uuid, info, key) = tokio::time::timeout(TIMEOUT, login)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(uuid, "uuid-alice");
            assert_eq!(info.credentials.root_key_id, login_key.public_key());
            assert_eq!(key.public_key(), login_key.public_key());
            let original_cache = conn.inner.registered_keys.lock().await.clone();
            assert_eq!(original_cache, HashSet::from([login_key.public_key()]));

            let key = PrivateKey::generate();
            // The same key must perform a full successful wire retry after either failure.
            for reject in [true, false] {
                let register_conn = conn.clone();
                let register_key = key.clone();
                let register =
                    tokio::spawn(
                        async move { register_conn.register_session_key(&register_key).await },
                    );
                let request: ServiceRequest = tokio::time::timeout(TIMEOUT, read_frame(&mut peer))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert!(
                    matches!(request, ServiceRequest::SessionKeyChallenge { pubkey } if pubkey == key.public_key())
                );
                assert_eq!(*conn.inner.registered_keys.lock().await, original_cache);
                if !reject || fail_after_challenge {
                    write_frame(
                        &mut peer,
                        &ServerFrame::Response(Box::new(ServiceResponse::SessionKeyChallenge {
                            challenge: KEY_CHALLENGE.to_vec(),
                        })),
                    )
                    .await
                    .unwrap();
                    let proof: ServiceRequest =
                        tokio::time::timeout(TIMEOUT, read_frame(&mut peer))
                            .await
                            .unwrap()
                            .unwrap()
                            .unwrap();
                    let ServiceRequest::SessionKeyRegister { pubkey, signature } = proof else {
                        panic!("expected session-key proof");
                    };
                    assert_eq!(pubkey, key.public_key());
                    pubkey.verify(KEY_CHALLENGE, &signature).unwrap();
                    assert_eq!(*conn.inner.registered_keys.lock().await, original_cache);
                }
                send_raw(
                    &mut peer,
                    if reject {
                        unsupported
                    } else {
                        r#"{"Response":"Ok"}"#
                    },
                )
                .await;
                let result = tokio::time::timeout(TIMEOUT, register)
                    .await
                    .unwrap()
                    .unwrap();
                if reject {
                    assert!(
                        matches!(result, Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported)
                    );
                    assert_eq!(*conn.inner.registered_keys.lock().await, original_cache);
                } else {
                    result.unwrap();
                    assert_eq!(
                        *conn.inner.registered_keys.lock().await,
                        HashSet::from([login_key.public_key(), key.public_key()])
                    );
                    // A cached success emits no frame; the metadata call below is a barrier.
                    tokio::time::timeout(TIMEOUT, conn.register_session_key(&key))
                        .await
                        .unwrap()
                        .unwrap();
                }
                assert_eq!(
                    conn.inner.session_read().as_ref().unwrap().session_pubkey,
                    login_key.public_key()
                );
                assert!(conn.inner.pending_lock().is_empty());
                assert_next_metadata_call(&conn, &mut peer).await;
            }
        }
    }
}

#[tokio::test]
async fn v0_unknown_required_login_enums_retire_connection() {
    let key = PrivateKey::generate();
    let valid = serde_json::to_value(passwordless_login_challenge(&key)).unwrap();
    let decoded: IncomingServerFrame = serde_json::from_value(valid.clone()).unwrap();
    let IncomingServerFrame::Response(response) = decoded else {
        panic!("expected login challenge response");
    };
    assert_eq!(
        serde_json::to_value(ServerFrame::Response(response)).unwrap(),
        valid
    );
    for field in ["KeyStorage", "AccountStatus"] {
        let mut malformed = valid.clone();
        let info = &mut malformed["Response"]["TrustedLoginChallenge"]["user_info"];
        match field {
            "KeyStorage" => info["credentials"]["root_key"]["type"] = "future-storage".into(),
            "AccountStatus" => info["status"] = "FutureStatus".into(),
            _ => unreachable!(),
        }
        assert!(serde_json::from_value::<IncomingServerFrame>(malformed.clone()).is_err());
        let (conn, mut peer) = connect_peer().await;
        let login_conn = conn.clone();
        let login = tokio::spawn(async move {
            login_conn
                .trusted_login("alice", None)
                .await
                .map(|_| ServiceResponse::TrustedLoginOk)
        });
        read_request(&mut peer, "alice").await;
        let pending = start_call(&conn, "pending");
        read_request(&mut peer, "pending").await;
        send_raw(&mut peer, &malformed.to_string()).await;
        assert_connection_failed(&conn, login).await;
        assert_connection_failed(&conn, pending).await;
        assert!(conn.inner.session_read().is_none());
        assert!(conn.inner.registered_keys.lock().await.is_empty());
    }
}

#[tokio::test]
async fn v0_client_rejects_nonzero_handshake_ack() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("version.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let peer = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let handshake: Handshake = read_frame(&mut stream).await.unwrap().unwrap();
        assert_eq!(handshake.protocol_version, 0);
        write_frame(
            &mut stream,
            &HandshakeAck {
                protocol_version: 1,
            },
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::time::timeout(TIMEOUT, async {
        tokio::join!(RemoteConnection::connect(&path), peer)
    })
    .await
    .unwrap();
    assert!(
        matches!(result, Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData)
    );
}
