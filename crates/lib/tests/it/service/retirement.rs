use super::*;
use eidetica::service::protocol::DatabaseOp;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UnixListener;
use tokio::sync::oneshot;

/// Forward real service traffic, but retain one successful submit response.
/// This makes a committed write's lost acknowledgement observable without
/// cancelling its client future or changing the daemon's execution path.
#[tokio::test]
async fn test_retire_lost_write_ack_preserves_commit_without_replay() {
    let (socket_path, _shutdown, server, _dir) = start_test_server().await;
    let proxy_dir = tempfile::tempdir().unwrap();
    let proxy_path = proxy_dir.path().join("proxy.sock");
    let listener = UnixListener::bind(&proxy_path).unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let gate = armed.clone();
    let (committed_tx, committed_rx) = oneshot::channel();
    let upstream_path = socket_path.clone();
    let proxy = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut upstream = UnixStream::connect(upstream_path).await.unwrap();
        let handshake: Handshake = read_frame(&mut client).await.unwrap().unwrap();
        write_frame(&mut upstream, &handshake).await.unwrap();
        let ack: HandshakeAck = read_frame(&mut upstream).await.unwrap().unwrap();
        write_frame(&mut client, &ack).await.unwrap();
        loop {
            let request: ServiceRequest = read_frame(&mut client).await.unwrap().unwrap();
            let is_submit = matches!(&request, ServiceRequest::AuthenticatedDb(db)
                if matches!(db.op, DatabaseOp::SubmitSignedEntry { .. }));
            write_frame(&mut upstream, &request).await.unwrap();
            // No subscriptions are installed, so each request has one response.
            let response: ServerFrame = read_frame(&mut upstream).await.unwrap().unwrap();
            if is_submit && gate.load(Ordering::Acquire) {
                assert!(
                    matches!(response, ServerFrame::Response(reply) if matches!(*reply, ServiceResponse::Ok)),
                    "the real daemon did not acknowledge the commit"
                );
                committed_tx.send(()).unwrap();
                assert!(
                    read_frame::<_, ServiceRequest>(&mut client)
                        .await
                        .unwrap()
                        .is_none(),
                    "retirement sent another request or replayed the write"
                );
                return;
            }
            write_frame(&mut client, &response).await.unwrap();
        }
    });

    let (instance, root, identity) = setup_db(&server, &proxy_path, "retirement-writer").await;
    let conn = remote_conn(&instance);
    let retained = conn.clone();
    let user = instance
        .login_account("retirement-writer", None)
        .await
        .unwrap();
    let key = user.get_default_key().unwrap();
    let signing_key = user.get_signing_key(&key).unwrap();
    let context = conn
        .begin_transaction(
            root.clone(),
            identity.clone(),
            vec!["retired".into()],
            ReadScope::Verified,
        )
        .await
        .unwrap();
    let entry = Entry::builder(root.clone())
        .set_parents(context.main_parents.into_iter().map(|(id, _)| id).collect())
        .set_subtree_data("retired", b"{\"value\":1}")
        .build()
        .unwrap();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    let entry = entry.with_auth(|auth| auth.signature = Some(signature));
    let id = entry.id();
    armed.store(true, Ordering::Release);
    let mut writing = Box::pin(conn.submit_signed_entry(root.clone(), identity.clone(), entry));
    tokio::select! {
        result = &mut writing => panic!("write completed without its acknowledgement: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), committed_rx) => {
            result.expect("proxy did not observe the commit").unwrap();
        }
    }
    // No-retirement control: the actual commit has happened, yet its caller
    // remains pending. Retiring must settle it without dropping its owner.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut writing)
            .await
            .is_err()
    );
    retained.retire();
    conn.retire();
    let error = tokio::time::timeout(Duration::from_secs(2), &mut writing)
        .await
        .expect("retirement stranded the write owner")
        .unwrap_err();
    assert!(
        matches!(error, eidetica::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionAborted)
    );
    assert!(conn.get_instance_metadata().await.is_err());
    assert!(
        instance
            .login_account("retirement-writer", None)
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(2), proxy)
        .await
        .unwrap()
        .unwrap();

    // Construction and authentication on a fresh connection still work while
    // the old Instance, AccountSession, clones and completed write stay owned.
    let fresh = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let _fresh_user = fresh
        .login_account("retirement-writer", None)
        .await
        .unwrap();
    let fresh_conn = remote_conn(&fresh);
    assert_eq!(
        fresh_conn
            .db_get_entry(root.clone(), identity.clone(), id.clone())
            .await
            .unwrap()
            .id(),
        id
    );
    let entries = fresh_conn
        .get_store_entries(
            root,
            identity,
            "retired".into(),
            vec![id.clone()],
            ReadScope::Verified,
        )
        .await
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "lost acknowledgement caused a duplicate write"
    );
    assert_eq!(entries[0].id(), id);
    retained.retire();
    assert!(
        fresh_conn.get_instance_metadata().await.unwrap().is_some(),
        "retiring an old generation affected its replacement"
    );
}
