use super::*;
use eidetica::service::ServiceConfig;

/// A real authenticated response larger than the old ceiling, not a fake prefix
/// or a helper-only codec test. Only ~65 MiB of application data is required.
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
            .set("large", "x".repeat(65 * 1024 * 1024))
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
    let client = Instance::connect_with_service_config(&url, config)
        .await
        .unwrap();
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
    assert!(
        conn.get_instance_metadata().await.unwrap().is_some(),
        "follow-up lost FIFO alignment"
    );

    // The unchanged default client must fail explicitly against the same server.
    let default_client = Instance::connect(&url).await.unwrap();
    let _admin = default_client.login_account("admin", None).await.unwrap();
    let default = remote_conn(&default_client);
    assert!(
        tokio::time::timeout(
            Duration::from_secs(60),
            default.db_get_entry(db.root_id().clone(), SigKey::from_pubkey(&key), id,)
        )
        .await
        .expect("default client hung on oversized response")
        .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), default.get_instance_metadata())
            .await
            .unwrap()
            .is_err()
    );
    drop(shutdown);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
