use super::*;
use eidetica::entry::ID;
use eidetica::service::ServiceConfig;
use eidetica::service::protocol::{AuthenticatedDbRequest, DatabaseOp};

/// A real authenticated response larger than the old ceiling, not a fake prefix
/// or a helper-only codec test. Only ~17 MiB of application data is required.
#[tokio::test]
async fn configured_frame_limit_roundtrips_entry_above_64_mib() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.sock");
    let (instance, mut admin) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("data")
            .await?
            .set("large", "x".repeat(17 * 1024 * 1024))
            .await?;
        Ok(())
    })
    .await
    .unwrap();
    let id = db
        .snapshot()
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .clone();
    let config = ServiceConfig::new(1024 * 1024 * 1024).unwrap();
    let server = ServiceServer::bind_with_config(instance.clone(), &path, config)
        .await
        .unwrap();
    let (shutdown, rx) = watch::channel(());
    let task = tokio::spawn(server.run(rx));
    let url = format!("unix://{}", path.display());
    let client = Instance::connect(&url).await.unwrap();
    let _admin = client.login_account("admin", None).await.unwrap();
    let conn = remote_conn(&client);
    let entry = tokio::time::timeout(
        Duration::from_secs(60),
        conn.db_get_entry(db.root_id().clone(), SigKey::from_pubkey(&key), id.clone()),
    )
    .await
    .expect("large response did not complete")
    .unwrap();
    assert_eq!(entry.id(), id);

    // Count the real encoded envelope without making another huge allocation.
    #[derive(Default)]
    struct Count(u64);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count::default();
    serde_json::to_writer(
        &mut count,
        &ServerFrame::Response(Box::new(ServiceResponse::Entry(entry))),
    )
    .unwrap();
    assert!(
        count.0 > 64 * 1024 * 1024,
        "positive control: response was only {} bytes",
        count.0
    );
    assert!(count.0 < u64::from(config.max_frame_bytes()));
    eprintln!(
        "Ordinary client with a 1 GiB daemon roundtripped an encoded {}-byte response",
        count.0
    );
    assert!(
        conn.get_instance_metadata().await.unwrap().is_some(),
        "follow-up lost FIFO alignment"
    );

    let private = admin.get_signing_key(&key).unwrap();
    let mut legacy = request_large_entry(
        &path,
        false,
        db.root_id().clone(),
        SigKey::from_pubkey(&key),
        id.clone(),
        &private,
    )
    .await;
    // The server must refuse the large frame before writing even its prefix.
    assert!(
        tokio::time::timeout(
            Duration::from_secs(60),
            read_frame::<_, ServerFrame>(&mut legacy)
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    assert!(conn.get_instance_metadata().await.unwrap().is_some());

    // A capable raw peer gets the large prefix; shutdown must interrupt its
    // blocked response writer rather than leave a detached task behind.
    let mut reader = request_large_entry(
        &path,
        true,
        db.root_id().clone(),
        SigKey::from_pubkey(&key),
        id,
        &private,
    )
    .await;
    use tokio::io::AsyncReadExt;
    let mut prefix = [0; 4];
    tokio::time::timeout(Duration::from_secs(60), reader.read_exact(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(u32::from_be_bytes(prefix) as u64, count.0);
    drop(shutdown);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut partial = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut partial))
        .await
        .unwrap()
        .unwrap();
    assert!(
        partial.len() < count.0 as usize,
        "orphaned writer completed a response after shutdown"
    );
}

/// Raw old/new peers use the same authenticated request, differing only in the
/// optional framing opt-in. Authentication and the operation itself do not change.
async fn request_large_entry(
    path: &std::path::Path,
    capable: bool,
    root: ID,
    identity: SigKey,
    id: ID,
    private: &PrivateKey,
) -> ReadHalf<UnixStream> {
    let stream = UnixStream::connect(path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_frame(
        &mut writer,
        &Handshake {
            protocol_version: 0,
            learns_frame_policy: capable,
        },
    )
    .await
    .unwrap();
    let ack: HandshakeAck = read_frame(&mut reader).await.unwrap().unwrap();
    assert_eq!(ack.max_frame_bytes, capable.then_some(1024 * 1024 * 1024));
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginAccount {
            username: "admin".into(),
        },
    )
    .await
    .unwrap();
    let challenge = match read_response(&mut reader).await {
        ServiceResponse::TrustedLoginChallenge { challenge, .. } => challenge,
        other => panic!("unexpected login response: {other:?}"),
    };
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: create_challenge_response(&challenge, private),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_response(&mut reader).await,
        ServiceResponse::TrustedLoginOk
    ));
    write_frame(
        &mut writer,
        &ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
            root_id: root,
            identity,
            op: DatabaseOp::GetEntry { id },
        })),
    )
    .await
    .unwrap();
    reader
}
