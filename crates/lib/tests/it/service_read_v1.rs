//! Read v1 uses real Tonic/UDS even when the other integration tests are local.

use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
    time::Duration,
};

use eidetica::{
    Database, DatabaseKey, ID, Instance, NewAccount, Snapshot,
    auth::{AuthKey, Permission, PrivateKey},
    crdt::Doc,
    service::{
        ServiceServer,
        read_v1::{ReadClient, ReadError, ReadServer},
    },
    state::{ReadScope, StateOperation, StateReader},
    store::DocStore,
};
use tokio::{sync::watch, task::JoinHandle, time::timeout};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn socket_dir() -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

async fn instance() -> eidetica::Result<(Instance, DatabaseKey)> {
    let (instance, session) = Instance::create_backend(
        crate::helpers::test_backend().await,
        NewAccount::passwordless("admin"),
    )
    .await?;
    let key = session.get_signing_key(&session.get_default_key()?)?;
    Ok((instance, DatabaseKey::new(key)))
}

fn run(
    server: ReadServer,
) -> (
    watch::Sender<()>,
    JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let (stop, rx) = watch::channel(());
    (stop, tokio::spawn(server.run(rx)))
}

#[tokio::test]
async fn test_v1_state_pins_context_and_isolates_signers_and_native_logins() -> TestResult {
    let dir = socket_dir()?;
    let v1 = dir.path().join("read.sock");
    let legacy = dir.path().join("legacy.sock");
    let (instance, native_admin) = instance().await?;
    let (legacy_stop, legacy_rx) = watch::channel(());
    let legacy_server = tokio::spawn(
        ServiceServer::bind(instance.clone(), &legacy)
            .await?
            .run(legacy_rx),
    );
    let (stop, server) = run(ReadServer::bind(instance.clone(), &v1).await?);
    let owner = DatabaseKey::new(PrivateKey::generate());
    let member = DatabaseKey::new(PrivateKey::generate());
    let db = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let other = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let key = member.public_key();
    let client = ReadClient::connect(&v1).await?;
    let root = db.root_id();
    let old = client.capture(&owner, root).await?;
    let empty = client
        .read_state(&owner, root, old.at.snapshot(), "_index")
        .await?
        .collect_complete()
        .await?;
    assert!(empty.value.is_empty());
    assert_eq!(empty.context.source, old);
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_auth_key(&key, AuthKey::active(None, Permission::Read))
            .await?;
        tx.get_store::<DocStore>("notes")
            .await?
            .set("key", "old")
            .await
    })
    .await?;
    let before = client.capture(&member, root).await?;
    // Read at latest grants access to an older cut with no recorded membership.
    assert!(
        client
            .read_state(&member, root, old.at.snapshot(), "_index")
            .await?
            .collect_complete()
            .await?
            .value
            .is_empty()
    );

    let writer = db.clone();
    let key = member.public_key();
    tokio::spawn(async move {
        writer
            .with_transaction(|tx| async move {
                tx.get_settings()?.revoke_auth_key(&key).await?;
                tx.get_store::<DocStore>("notes")
                    .await?
                    .set("key", "new")
                    .await
            })
            .await
    })
    .await??;
    let latest = client.capture(&owner, root).await?;
    assert_ne!(latest.at, before.at);
    let result = client
        .read_state(&member, root, before.at.snapshot(), "notes")
        .await?
        .collect_complete()
        .await?;
    assert_eq!(result.context.source, before);
    assert_eq!(result.context.store, "notes");
    assert_eq!(result.context.format, "eidetica/doc:v0");
    assert_eq!(
        result.value.get("key").and_then(|v| v.as_text()),
        Some("old")
    );
    assert!(matches!(
        client.capture(&member, root).await,
        Err(ReadError::Denied)
    ));
    assert!(matches!(
        client.history(&member, root, before.at.snapshot()).await,
        Err(ReadError::Denied)
    ));
    for cut in [old.at.snapshot(), latest.at.snapshot()] {
        assert!(matches!(
            client.read_state(&member, root, cut, "_index").await,
            Err(ReadError::Denied)
        ));
    }
    assert!(matches!(
        client
            .read_state(&owner, root, &other.snapshot().await?, "notes")
            .await,
        Err(ReadError::InvalidBoundary)
    ));
    let unavailable = Snapshot::from([ID::from_dagcbor_bytes(b"missing")]);
    assert!(matches!(
        client.read_state(&owner, root, &unavailable, "notes").await,
        Err(ReadError::UnavailableHistory)
    ));

    // Actually log in through the native service; no login or privileged
    // signer state from either socket attaches to a v1 operation.
    let remote = Instance::connect(format!("unix://{}", legacy.display())).await?;
    let session = remote.login_account("admin", "").await?;
    assert_eq!(session.get_default_key()?, native_admin.public_key());
    let users = instance
        .backend()
        .get_instance_metadata()
        .await?
        .unwrap()
        .users_db;
    let users_cut = StateReader::new(&instance, &native_admin)
        .capture(&users, ReadScope::Verified)
        .await?;
    assert!(matches!(
        client
            .history(&native_admin, &users, users_cut.snapshot())
            .await,
        Err(ReadError::Denied)
    ));
    assert!(matches!(
        client
            .read_state(&native_admin, &users, users_cut.snapshot(), "_index")
            .await,
        Err(ReadError::Denied)
    ));
    assert!(matches!(
        client
            .read_state(&native_admin, root, before.at.snapshot(), "notes")
            .await,
        Err(ReadError::Denied)
    ));
    let spoof = DatabaseKey::with_identity(PrivateKey::generate(), owner.identity().clone());
    assert!(matches!(
        client.capture(&spoof, root).await,
        Err(ReadError::Denied)
    ));

    drop(client);
    drop((remote, session));
    drop((stop, legacy_stop));
    timeout(Duration::from_secs(5), server).await???;
    timeout(Duration::from_secs(5), legacy_server).await???;
    assert!(!v1.exists());
    Ok(())
}

#[tokio::test]
async fn test_v1_state_refuses_before_and_after_provisional_data() -> TestResult {
    let dir = socket_dir()?;
    let path = dir.path().join("read.sock");
    let (instance, owner) = instance().await?;
    let db = Database::create(&instance, owner.clone(), Doc::new()).await?;
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("notes")
            .await?
            .set("large", "x".repeat(200_000))
            .await
    })
    .await?;
    let grants = Arc::new(AtomicUsize::new(0));
    let policy = grants.clone();
    let (stop, server) = run(ReadServer::bind(instance, &path)
        .await?
        .with_state_admission(Arc::new(move |_, op| {
            !matches!(op, StateOperation::ReadDoc { .. })
                || policy
                    .fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1))
                    .is_ok()
        })));
    let client = ReadClient::connect(&path).await?;
    let root = db.root_id();
    let at = client.capture(&owner, root).await?;
    let read = || client.read_state(&owner, root, at.at.snapshot(), "notes");
    assert!(matches!(read().await, Err(ReadError::Refused)));
    grants.store(2, Relaxed);
    let mut stream = read().await?;
    assert!(!stream.next_chunk().await?.unwrap().is_empty());
    assert!(matches!(stream.next_chunk().await, Err(ReadError::Refused)));
    assert!(matches!(stream.finish(), Err(ReadError::Incomplete)));
    grants.store(2, Relaxed);
    assert!(matches!(
        read().await?.collect_complete().await,
        Err(ReadError::Refused)
    ));
    grants.store(10, Relaxed);
    let result = read().await?.collect_complete().await?;
    assert_eq!(
        result.value.get("large").unwrap().as_text().unwrap().len(),
        200_000
    );
    drop(client);
    drop(stop);
    timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn test_v1_car_covers_all_branches_at_one_cut() -> TestResult {
    let dir = socket_dir()?;
    let path = dir.path().join("read.sock");
    let (instance, owner) = instance().await?;
    let db = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let base = db.snapshot().await?;
    let a = db.new_transaction_at(&base).await?;
    let b = db.new_transaction_at(&base).await?;
    a.get_store::<DocStore>("notes")
        .await?
        .set("a", "x".repeat(300_000))
        .await?;
    b.get_store::<DocStore>("notes")
        .await?
        .set("b", "independent")
        .await?;
    let a = a.commit().await?;
    let b = b.commit().await?;
    let root = db.root_id();
    let (stop, server) = run(ReadServer::bind(instance, &path).await?);
    let client = ReadClient::connect(&path).await?;
    let cut = client.capture(&owner, root).await?;
    assert_eq!(cut.at.snapshot(), &Snapshot::from([a.clone(), b.clone()]));
    let mut stream = client.history(&owner, root, cut.at.snapshot()).await?;
    let first = stream.next_entry().await?.unwrap();
    assert!(first.id() == a || first.id() == b);
    db.with_transaction(|tx| async move { tx.get_settings()?.set_name("advanced").await })
        .await?;
    let mut blocks = vec![first];
    while let Some(entry) = stream.next_entry().await? {
        blocks.push(entry);
    }
    assert_eq!(stream.finish()?, cut);
    for id in [root, &a, &b] {
        let streamed = blocks.iter().find(|entry| entry.id_ref() == id).unwrap();
        let local = db.get_entry(id).await?;
        assert_eq!(streamed.canonical_bytes()?, local.canonical_bytes()?);
        assert!(db.verify_entry_signature(id).await?);
    }
    assert_eq!(blocks.len(), 3);
    let delivered = client
        .history(&owner, root, cut.at.snapshot())
        .await?
        .collect_complete()
        .await?;
    assert_eq!(delivered.value, blocks);
    assert_eq!(delivered.context, cut);
    assert!(matches!(
        client
            .history(&owner, root, cut.at.snapshot())
            .await?
            .finish(),
        Err(ReadError::Incomplete)
    ));
    drop(client);
    drop(stop);
    timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

/// Heavier evidence run: keep the signed fixture local and measure the actual
/// point reads/bytes under production work caps and h2 flow control.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "explicit v1 large-history and buffer profiling gate"]
async fn test_v1_cold_work_budgets() -> TestResult {
    use eidetica::{
        Entry, Registered,
        auth::{AuthInfo, verify_entry_signature},
        backend::{VerificationStatus, database::InMemory},
        crdt::Codec,
    };

    fn hwm_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .unwrap()
            .split_ascii_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    let dir = socket_dir()?;
    let path = dir.path().join("read.sock");
    let instance = crate::helpers::test_local_instance().await;
    let owner = DatabaseKey::new(PrivateKey::generate());
    let db = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let root = db.root_id();
    let engine = instance.backend().local_engine().unwrap();
    let memory = engine.as_any().downcast_ref::<InMemory>().unwrap();
    let mut parent = root.clone();
    // InMemory recomputes raw tips on every insert. A few hundred larger
    // entries put the cold producer over its byte budget without making
    // fixture construction the expensive part of the probe.
    const LONG: usize = 480;
    const MIB: usize = 1024 * 1024;
    let mut registry = Doc::new();
    for i in 0..4096 {
        let mut metadata = Doc::new();
        metadata.set("type", DocStore::type_id());
        metadata.set("marker", "r".repeat(256));
        registry.set(format!("store-{i}"), metadata);
    }
    let registry = registry.encode()?;
    let mut corpus_bytes = 0;
    for height in 1..=LONG {
        let mut delta = Doc::new();
        delta.set("payload", "x".repeat(144 * 1024));
        let mut builder = Entry::builder(root.clone())
            .add_parent(parent.clone())
            .set_height(height as u64)
            .set_subtree_data("store-0", delta.encode()?)
            .set_auth(AuthInfo::from_pubkey(&owner.public_key()));
        if height > 1 {
            builder = builder.add_subtree_parent("store-0", parent);
        }
        if height == LONG {
            builder = builder.set_subtree_data("_index", registry.clone());
        }
        // The budget fixture supplies a trusted retained prefix directly to
        // avoid O(n²) history checks during setup. Domain admission is tested
        // by the smaller integration round-trips with real commits.
        let entry = builder.build()?.sign(owner.signing_key())?;
        verify_entry_signature(&entry, &owner.public_key())?;
        parent = entry.id();
        corpus_bytes += entry.canonical_bytes()?.len();
        engine.put(entry).await?;
        engine
            .update_verification_status(&parent, VerificationStatus::Verified)
            .await?;
    }
    assert!(corpus_bytes > 64 * MIB);
    engine.clear_derived_store_state().await?;
    let policy = Arc::new(AtomicUsize::new(0));
    let enabled = policy.clone();
    let (stop, server) = run(ReadServer::bind(instance.clone(), &path)
        .await?
        .with_state_admission(Arc::new(move |_, op| {
            !matches!(op, StateOperation::ReadDoc { .. }) || enabled.load(Relaxed) > 0
        })));
    let client = ReadClient::connect(&path).await?;
    let baseline = memory.bounded_entry_read_counts();
    let baseline_hwm = hwm_kib();
    let cut = client.capture(&owner, root).await?;
    assert_eq!(cut.at.snapshot(), &Snapshot::from([parent]));
    let after_capture = memory.bounded_entry_read_counts();
    assert_eq!(after_capture.0 - baseline.0, 2); // the root and indexed Settings
    let old_bulk = memory.store_history_read_count();
    let state = || client.read_state(&owner, root, cut.at.snapshot(), "store-0");
    assert!(matches!(state().await, Err(ReadError::Refused)));
    assert_eq!(memory.bounded_entry_read_counts(), after_capture);
    policy.store(1, Relaxed);

    // Cancel after positive backend progress, before the cold index can finish.
    let pending = {
        let client = client.clone();
        let at = cut.at.snapshot().clone();
        let root = root.clone();
        let owner = owner.clone();
        tokio::spawn(async move { client.read_state(&owner, &root, &at, "store-0").await })
    };
    timeout(Duration::from_secs(3), async {
        while memory.bounded_entry_read_counts().0 - after_capture.0 < 20 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    pending.abort();
    assert!(pending.await.err().unwrap().is_cancelled());
    // A successful query proves the connection remains usable; further bulk
    // probes below verify permits are not leaked by the canceled request.
    client.capture(&owner, root).await?;
    let after_cancel = memory.bounded_entry_read_counts();
    assert!(after_cancel.0 - after_capture.0 < 256);
    assert!(after_cancel.1 - after_capture.1 < 12 * MIB);

    let before_limited = memory.bounded_entry_read_counts();
    assert!(matches!(state().await, Err(ReadError::Limit)));
    let after_limited = memory.bounded_entry_read_counts();
    assert!(after_limited.0 - before_limited.0 < LONG);
    assert!((30 * MIB..=32 * MIB).contains(&(after_limited.1 - before_limited.1)));
    assert_eq!(memory.store_history_read_count(), old_bulk);
    eprintln!(
        "read-v1 cold: registry={} bytes, history={} entries/{} bytes, early refusal=0 point reads/0 bytes, cancelled prefix={} reads/{} bytes (includes capture probe), limited={} reads/{} bytes, HWM {} -> {} KiB (fixture included)",
        registry.len(),
        LONG + 1,
        corpus_bytes,
        after_cancel.0 - after_capture.0,
        after_cancel.1 - after_capture.1,
        after_limited.0 - before_limited.0,
        after_limited.1 - before_limited.1,
        baseline_hwm,
        hwm_kib()
    );

    // A stalled reader must not pull the whole history into a queue. Occupy
    // all producer slots and prove independent control RPCs still get through.
    let before_stall = memory.bounded_entry_read_counts();
    let mut stalled = Vec::new();
    for _ in 0..4 {
        let mut stream = client.history(&owner, root, cut.at.snapshot()).await?;
        assert!(stream.next_entry().await?.is_some());
        stalled.push(stream);
    }
    assert!(matches!(
        client.capture(&owner, root).await,
        Err(ReadError::Limit)
    ));
    let control = timeout(Duration::from_secs(2), ReadClient::connect(&path)).await??;
    let after_stall = memory.bounded_entry_read_counts();
    assert!(after_stall.0 - before_stall.0 < 128);
    assert!(after_stall.1 - before_stall.1 < 16 * MIB);
    drop(stalled);
    timeout(Duration::from_secs(2), async {
        loop {
            match control.capture(&owner, root).await {
                Ok(_) => break Ok(()),
                Err(ReadError::Limit) => tokio::task::yield_now().await,
                Err(error) => break Err(error),
            }
        }
    })
    .await??;

    // Same >64 MiB source, but read to completion one Entry at a time.
    let mut complete = client.history(&owner, root, cut.at.snapshot()).await?;
    let mut seen = 0;
    let mut bytes = 0;
    let mut largest = 0;
    while let Some(entry) = complete.next_entry().await? {
        seen += 1;
        let length = entry.canonical_bytes()?.len();
        bytes += length;
        largest = largest.max(length);
    }
    assert_eq!(complete.finish()?, cut);
    assert_eq!(seen, LONG + 1);
    assert!(bytes > 64 * MIB);
    eprintln!(
        "read-v1 history: stalled={} reads/{} bytes before cancellation, delivered={} entries/{} bytes, largest Entry={} bytes, HWM={} KiB",
        after_stall.0 - before_stall.0,
        after_stall.1 - before_stall.1,
        seen,
        bytes,
        largest,
        hwm_kib()
    );
    drop((client, control, stop));
    timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn test_v1_state_delegation_uses_current_or_committed_authority() -> TestResult {
    use eidetica::auth::{
        DelegatedTreeRef, DelegationStep, KeyHint, PermissionBounds, SigKey, TreeReference,
    };

    let dir = socket_dir()?;
    let path = dir.path().join("read.sock");
    let (instance, admin) = instance().await?;
    let member = PrivateKey::generate();
    let public = member.public_key();
    let db = Database::create(&instance, admin.clone(), Doc::new()).await?;
    let identity = Database::create(&instance, admin.clone(), Doc::new()).await?;
    let pin = |at: Snapshot| {
        let root = identity.root_id().clone();
        db.with_transaction(move |tx| async move {
            tx.get_settings()?
                .add_delegated_tree(DelegatedTreeRef {
                    tree: TreeReference {
                        root,
                        tips: at.into_tips(),
                    },
                    permission_bounds: PermissionBounds {
                        min: None,
                        max: Permission::Read,
                    },
                })
                .await
        })
    };
    pin(identity.snapshot().await?).await?;
    let (stop, server) = run(ReadServer::bind(instance, &path).await?);
    let client = ReadClient::connect(&path).await?;
    let before = client.capture(&admin, db.root_id()).await?;
    let grant = public.clone();
    identity
        .with_transaction(|tx| async move {
            tx.get_settings()?
                .set_auth_key(&grant, AuthKey::active(None, Permission::Read))
                .await
        })
        .await?;
    let grant_at = identity.snapshot().await?;
    pin(grant_at.clone()).await?;
    let member = DatabaseKey::with_identity(
        member,
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: identity.root_id().clone(),
                tips: grant_at.into_tips(),
            }],
            hint: KeyHint::from_pubkey(&public),
        },
    );
    let granted = client.capture(&member, db.root_id()).await?;
    assert!(
        client
            .read_state(&member, db.root_id(), before.at.snapshot(), "_index")
            .await?
            .collect_complete()
            .await?
            .value
            .is_empty()
    );
    identity
        .with_transaction(|tx| async move { tx.get_settings()?.revoke_auth_key(&public).await })
        .await?;
    assert!(matches!(
        client.capture(&member, db.root_id()).await,
        Err(ReadError::Denied)
    ));
    let still_readable = client
        .read_state(&member, db.root_id(), granted.at.snapshot(), "_index")
        .await?
        .collect_complete()
        .await?;
    assert_eq!(still_readable.context.source, granted);
    assert!(matches!(
        client
            .read_state(&member, db.root_id(), before.at.snapshot(), "_index")
            .await,
        Err(ReadError::Denied)
    ));
    drop((client, stop));
    timeout(Duration::from_secs(5), server).await???;
    Ok(())
}
