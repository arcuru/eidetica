//! Synthetic latency probe: independently controlled history and materialized size.
//! Run with `cargo test -p eidetica --release --all-features --test frontier_probe
//! -- --ignored --nocapture`. Never points at an existing database or daemon.
#![cfg(all(unix, feature = "service", feature = "sqlite"))]

use std::time::{Duration, Instant};

use eidetica::{
    Instance, NewAccount, backend::database::Sqlite, crdt::Doc, service::ServiceServer,
    store::DocStore,
};
use tokio::sync::watch;

fn stats(name: &str, mut v: Vec<Duration>) {
    v.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "{name}: n={} min={:.3}ms median={:.3}ms max={:.3}ms",
        v.len(),
        ms(v[0]),
        ms(v[v.len() / 2]),
        ms(v[v.len() - 1])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn frontier_probe() {
    use std::os::unix::fs::PermissionsExt;
    let n: usize = std::env::var("PROBE_ENTRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(700);
    let keys: usize = std::env::var("PROBE_KEYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    assert!(keys > 0);
    // Runtime refusal only when this ignored probe is invoked.
    if cfg!(debug_assertions) {
        panic!("latency measurements require --release");
    }
    println!("history_commits={n} materialized_keys={keys} reads=30 writes=10 mode=release");
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sqlite = Sqlite::open(&dir.path().join("probe.db")).await.unwrap();
    let (server, _admin) =
        Instance::create_backend(Box::new(sqlite), NewAccount::passwordless("admin"))
            .await
            .unwrap();
    let mut admin = server.login_account("admin", None).await.unwrap();
    let key = admin.get_default_key().unwrap();
    let db = admin.create_database(Doc::new(), &key).await.unwrap();
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        for i in 0..keys {
            store.set(format!("k{i}"), "initial").await?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let populate = Instant::now();
    for i in 0..n {
        db.with_transaction(|tx| async move {
            let s = tx.get_store::<DocStore>("data").await?;
            s.set(format!("k{}", i % keys), i.to_string()).await?;
            Ok(())
        })
        .await
        .unwrap();
    }
    db.verify().await.unwrap();
    println!(
        "populate_{n}_commits={:.1}s",
        populate.elapsed().as_secs_f64()
    );
    let root = db.root_id().clone();
    let engine = server.backend().local_engine().unwrap();
    println!(
        "tree_entries={}",
        engine.get_tree(&root).await.unwrap().len()
    );

    // Cold materialization means disposable Store state cleared, not cold OS pages.
    engine.clear_derived_store_state().await.unwrap();
    let cold = Instant::now();
    db.get_settings().await.unwrap();
    println!(
        "local_cold_settings_ms={:.3}",
        cold.elapsed().as_secs_f64() * 1000.0
    );
    let _ = db.snapshot().await.unwrap();
    let mut snap = Vec::new();
    let mut settings = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        db.snapshot().await.unwrap();
        snap.push(t.elapsed());
        let t = Instant::now();
        db.get_settings().await.unwrap();
        settings.push(t.elapsed());
    }
    stats("local_default_snapshot", snap);
    stats("local_get_settings", settings);

    let mut local_commit = Vec::new();
    for i in 0..10 {
        let t = Instant::now();
        db.with_transaction(|tx| async move {
            let s = tx.get_store::<DocStore>("data").await?;
            s.set(format!("k{}", i % keys), format!("local{i}")).await?;
            Ok(())
        })
        .await
        .unwrap();
        local_commit.push(t.elapsed());
    }
    stats("local_write_transaction", local_commit);

    // Service path (same isolated SQLite instance, no production socket).
    let socket_path = dir.path().join("probe.sock");
    let (tx, rx) = watch::channel(());
    let service = ServiceServer::bind(server.clone(), socket_path.clone())
        .await
        .unwrap();
    let handle = tokio::spawn(service.run(rx));
    let client = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let session = client.login_account("admin", None).await.unwrap();
    let rdb = session.open_database(&root).await.unwrap();
    engine.clear_derived_store_state().await.unwrap();
    let cold = Instant::now();
    rdb.get_settings().await.unwrap();
    println!(
        "service_cold_settings_ms={:.3}",
        cold.elapsed().as_secs_f64() * 1000.0
    );
    let _ = rdb.snapshot().await.unwrap();
    let mut rsnap = Vec::new();
    let mut rsettings = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        rdb.snapshot().await.unwrap();
        rsnap.push(t.elapsed());
        let t = Instant::now();
        rdb.get_settings().await.unwrap();
        rsettings.push(t.elapsed());
    }
    stats("service_snapshot_GetVerifiedTips", rsnap);
    stats("service_get_settings", rsettings);
    let mut rcommit = Vec::new();
    for i in 0..10 {
        let t = Instant::now();
        rdb.with_transaction(|tx| async move {
            let s = tx.get_store::<DocStore>("data").await?;
            s.set(format!("k{}", i % keys), format!("remote{i}"))
                .await?;
            Ok(())
        })
        .await
        .unwrap();
        rcommit.push(t.elapsed());
    }
    stats("service_write_transaction", rcommit);
    drop(tx);
    let _ = handle.await;
}
