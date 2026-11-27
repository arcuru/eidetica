use std::{
    env,
    error::Error,
    future::Future,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use eidetica::{
    Cluster, Database, Instance, NewAccount,
    auth::{AuthKey, Permission},
    backend::database::Sqlite,
    crdt::Doc,
    entry::ID,
    service::ServiceServer,
    store::DocStore,
    testing::{SimLoopback, SimNetwork, add_auth_keys, set_global_auth_key},
};
use rand::{Rng, SeedableRng, rngs::StdRng};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MAX_STEPS: usize = 1000;
/// Upper bound for one client operation or one daemon start; a hung daemon fails the run.
const OP_TIMEOUT: Duration = Duration::from_secs(10);
/// Re-exec argument for the child daemon; not a workload.
const DAEMON_CHILD: &str = "__daemon";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if let [mode, db, socket] = args.as_slice()
        && mode == DAEMON_CHILD
    {
        return match run_daemon_child(Path::new(db), Path::new(socket)).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("daemon: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let usage = format!(
        "usage: runtime-test interleaved-sync SEED STEPS [--skip-final-exchange]\n\
         \x20      runtime-test daemon-restart SEED STEPS [--restart-from-setup-snapshot]\n\
         SEED is a u64; STEPS is in 1..={MAX_STEPS} (2..={MAX_STEPS} for daemon-restart). \
         Use `timeout` to bound wall time."
    );
    if args == ["--help"] {
        println!("{usage}");
        return ExitCode::SUCCESS;
    }

    let parsed = match args.as_slice() {
        [workload, seed, steps, rest @ ..]
            if (workload == "interleaved-sync"
                && (rest.is_empty() || rest == ["--skip-final-exchange"]))
                || (workload == "daemon-restart"
                    && (rest.is_empty() || rest == ["--restart-from-setup-snapshot"])) =>
        {
            seed.parse::<u64>()
                .ok()
                .zip(steps.parse::<usize>().ok())
                .map(|(seed, steps)| (workload.as_str(), seed, steps, !rest.is_empty()))
        }
        _ => None,
    };
    let Some((workload, seed, steps, flag)) = parsed.filter(|(workload, _, steps, _)| {
        let min = if *workload == "daemon-restart" { 2 } else { 1 };
        (min..=MAX_STEPS).contains(steps)
    }) else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };

    let result = if workload == "interleaved-sync" {
        eprintln!("workload=interleaved-sync seed={seed} steps={steps} skip_final_exchange={flag}");
        interleaved_sync(seed, steps, flag)
            .await
            .map(|()| format!("{steps} writes present on both peers"))
    } else {
        eprintln!(
            "workload=daemon-restart seed={seed} steps={steps} restart_from_setup_snapshot={flag}"
        );
        daemon_restart(seed, steps, flag).await
    };
    match result {
        Ok(summary) => {
            eprintln!("PASS {workload}: {summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("FAIL {workload}: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn interleaved_sync(seed: u64, steps: usize, skip_final: bool) -> Result<()> {
    eprintln!("setup: create and bootstrap two in-memory peers");
    let mut cluster = Cluster::builder()
        .peers(2)
        .transport(Arc::new(SimLoopback::new(SimNetwork::new())))
        .build()
        .await?;
    let key = cluster.peer(0).key_id().clone();
    let device = cluster.peer(0).instance().id();
    let mut settings = Doc::new();
    settings.set("name", "runtime-test");
    let first = cluster
        .peer_mut(0)
        .account_session_mut()
        .create_database(settings, &key)
        .await?;
    let tree = first.root_id().clone();
    add_auth_keys(
        &first,
        &[(
            &device,
            AuthKey::active(Some("device"), Permission::Admin(10)),
        )],
    )
    .await?;
    set_global_auth_key(&first, AuthKey::active(None, Permission::Write(10))).await?;
    cluster.peer_mut(0).serve(&tree).await?;
    cluster
        .bootstrap(0, 1, &tree, Permission::Write(10))
        .await?;
    cluster.peer_mut(1).serve(&tree).await?;
    let second = cluster
        .peer(1)
        .account_session()
        .open_database(&tree)
        .await?;
    let dbs = [first, second];

    // Drive only manual exchanges so scheduling never depends on a background push timer.
    let mut rng = StdRng::seed_from_u64(seed);
    let mut written = Vec::with_capacity(steps);
    for step in 1..=steps {
        let peer = rng.gen_range(0..dbs.len());
        if step > 1 && rng.gen_bool(0.5) {
            eprintln!("step={step} exchange={peer}->{}", 1 - peer);
            cluster.exchange(peer, 1 - peer, &tree).await?;
        }
        eprintln!("step={step} write peer={peer} key=step-{step}");
        let tx = dbs[peer].new_transaction().await?;
        tx.get_store::<DocStore>("data")
            .await?
            .set_string(&format!("step-{step}"), "written")
            .await?;
        written.push(tx.commit().await?);
    }

    if skip_final {
        eprintln!("settle: deliberately skipping final exchange");
    } else {
        eprintln!("settle: exchange=0->1");
        cluster.exchange(0, 1, &tree).await?;
    }
    eprintln!("check: tip agreement and presence of all committed writes");
    if !cluster.converged_all(&tree).await? {
        return Err("invariant failed: peers disagree on tips after settle".into());
    }
    cluster.assert_all_present(&[0, 1], &tree, &written).await?;
    Ok(())
}

/// Serve one SQLite file over a Unix socket until killed or the parent goes away.
///
/// The parent holds this process's stdin; EOF means the parent exited (even by
/// SIGKILL), so the child never outlives the runner.
async fn run_daemon_child(db: &Path, socket: &Path) -> Result<()> {
    let backend = Sqlite::open(db).await?;
    let (instance, _) =
        Instance::connect_or_create_backend(Box::new(backend), NewAccount::passwordless("admin"))
            .await?;
    let server = ServiceServer::bind(instance, socket).await?;
    let (_shutdown, rx) = tokio::sync::watch::channel(());
    let orphaned = tokio::task::spawn_blocking(|| {
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
    });
    tokio::select! {
        result = server.run(rx) => Ok(result?),
        _ = orphaned => Err("parent runner exited".into()),
    }
}

/// A test-owned daemon process, killed and reaped on drop.
struct Daemon {
    child: Child,
    generation: u32,
}

impl Daemon {
    async fn start(generation: u32, db: &Path, socket: &Path) -> Result<(Self, Instance)> {
        eprintln!(
            "fault: start daemon generation={generation} db={}",
            db.file_name().unwrap_or_default().display()
        );
        let child = Command::new(env::current_exe()?)
            .arg(DAEMON_CHILD)
            .arg(db)
            .arg(socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut daemon = Daemon { child, generation };
        let url = format!("unix://{}", socket.display());
        let deadline = Instant::now() + OP_TIMEOUT;
        loop {
            if let Some(status) = daemon.child.try_wait()? {
                return Err(
                    format!("daemon generation={generation} exited at start: {status}").into(),
                );
            }
            // A stale socket from a killed generation refuses until the new one binds.
            match bounded("connect", Instance::connect(&url)).await {
                Ok(instance) => return Ok((daemon, instance)),
                Err(error) if Instant::now() >= deadline => {
                    return Err(format!("daemon generation={generation} not ready: {error}").into());
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    }

    /// SIGKILL: no graceful drain or flush, like a crash or OOM kill.
    fn kill(&mut self) -> Result<()> {
        eprintln!("fault: SIGKILL daemon generation={}", self.generation);
        self.child.kill()?;
        let status = self.child.wait()?;
        eprintln!(
            "fault: daemon generation={} reaped status={status}",
            self.generation
        );
        Ok(())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn bounded<T, E: Into<Box<dyn Error>>>(
    what: &str,
    operation: impl Future<Output = std::result::Result<T, E>>,
) -> Result<T> {
    match tokio::time::timeout(OP_TIMEOUT, operation).await {
        Ok(result) => result.map_err(|e| format!("{what}: {}", e.into()).into()),
        Err(_) => Err(format!("{what}: timed out after {OP_TIMEOUT:?}").into()),
    }
}

async fn write_step(db: &Database, step: usize, value: &str) -> Result<ID> {
    let tx = db.new_transaction().await?;
    tx.get_store::<DocStore>("data")
        .await?
        .set_string(&format!("step-{step}"), value)
        .await?;
    Ok(tx.commit().await?)
}

async fn read_step(db: &Database, step: usize) -> Result<Option<String>> {
    let store = db.get_store_viewer::<DocStore>("data").await?;
    // Only a missing key is an absent write; transport or decode errors fail the run.
    match store.get_string(format!("step-{step}")).await {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Every write the daemon acknowledged must read back with its value.
async fn check_acknowledged(db: &Database, acked: &[(usize, String)]) -> Result<()> {
    let mut missing = Vec::new();
    for (step, value) in acked {
        if bounded("read", read_step(db, *step)).await?.as_ref() != Some(value) {
            missing.push(format!("step-{step}"));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "invariant failed: {} of {} acknowledged writes missing: {}",
            missing.len(),
            acked.len(),
            missing.join(",")
        )
        .into())
    }
}

async fn open_admin_database(instance: &Instance, root: &ID) -> Result<Database> {
    let session = bounded("login", instance.login_account("admin", "")).await?;
    bounded("open database", session.open_database(root)).await
}

async fn daemon_restart(seed: u64, steps: usize, stale_snapshot: bool) -> Result<String> {
    let scratch = tempfile::Builder::new()
        .prefix("eidetica-runtime-")
        .tempdir()?;
    std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700))?;
    let socket = scratch.path().join("service.sock");
    let db_path = |name: &str| -> PathBuf { scratch.path().join(name) };

    let mut rng = StdRng::seed_from_u64(seed);
    // Restart strictly before the last step so at least one write follows it.
    let restart_at = rng.gen_range(1..steps);
    // Debug-build writes take tens of milliseconds, so both outcomes occur across seeds.
    let kill_delay = Duration::from_micros(rng.gen_range(0..=50_000));
    eprintln!(
        "plan: restart_at=step-{restart_at} kill_delay_us={} (seeded; whether the in-flight write \
         is acknowledged first depends on OS scheduling)",
        kill_delay.as_micros()
    );

    eprintln!("setup: create a database through the daemon client");
    let (mut daemon, instance) = Daemon::start(0, &db_path("eidetica.db"), &socket).await?;
    let mut session = bounded("login", instance.login_account("admin", "")).await?;
    let key = session.get_default_key()?;
    let mut settings = Doc::new();
    settings.set("name", "runtime-test");
    let mut db = bounded("create database", session.create_database(settings, &key)).await?;
    let root = db.root_id().clone();
    let mut client = instance;
    let mut generation = 1;
    if stale_snapshot {
        // A killed daemon leaves a crash image; copy it before any workload write.
        eprintln!("fault: snapshot storage after setup (test-only flag)");
        daemon.kill()?;
        for suffix in ["", "-wal", "-shm"] {
            let from = db_path(&format!("eidetica.db{suffix}"));
            if from.exists() {
                std::fs::copy(&from, db_path(&format!("snapshot.db{suffix}")))?;
            }
        }
        let (next, instance) = Daemon::start(generation, &db_path("eidetica.db"), &socket).await?;
        generation += 1;
        daemon = next;
        db = open_admin_database(&instance, &root).await?;
        client = instance;
    }

    let mut acked: Vec<(usize, String)> = Vec::with_capacity(steps);
    let mut unacked = None;
    for step in 1..=steps {
        let value = format!("seed-{seed}-step-{step}");
        // Draw from earlier steps, not from `acked`, so the fault's outcome
        // does not change the seeded operation sequence.
        if step > 1 && rng.gen_bool(0.25) {
            let old = rng.gen_range(1..step);
            eprintln!("step={step} read key=step-{old}");
            let found = bounded("read", read_step(&db, old)).await?;
            match acked.iter().find(|(s, _)| *s == old) {
                Some((_, expected)) if found.as_ref() != Some(expected) => {
                    return Err(format!(
                        "invariant failed: acknowledged write step-{old} unreadable"
                    )
                    .into());
                }
                Some(_) => {}
                None => eprintln!("step={step} read key=step-{old} unacknowledged, not checked"),
            }
        }
        if step != restart_at {
            eprintln!("step={step} write key=step-{step}");
            bounded("write", write_step(&db, step, &value)).await?;
            eprintln!("step={step} ack key=step-{step}");
            acked.push((step, value));
            continue;
        }

        // Kill the daemon while this write is in flight; only a returned ID is an ack.
        eprintln!("step={step} write key=step-{step} (in flight during fault)");
        let (outcome, killed) = tokio::join!(
            tokio::time::timeout(OP_TIMEOUT, write_step(&db, step, &value)),
            async {
                tokio::time::sleep(kill_delay).await;
                daemon.kill()
            }
        );
        killed?;
        match outcome {
            Ok(Ok(_)) => {
                eprintln!("step={step} ack key=step-{step} (before kill)");
                acked.push((step, value));
            }
            Ok(Err(error)) => {
                eprintln!("step={step} unacknowledged key=step-{step}: {error}");
                unacked = Some(step);
            }
            Err(_) => {
                eprintln!("step={step} unacknowledged key=step-{step}: timed out");
                unacked = Some(step);
            }
        }

        let restart_db = if stale_snapshot {
            eprintln!("fault: deliberately restarting on the setup snapshot (test-only flag)");
            db_path("snapshot.db")
        } else {
            db_path("eidetica.db")
        };
        let (next, instance) = Daemon::start(generation, &restart_db, &socket).await?;
        daemon = next;
        eprintln!("check: acknowledged writes after restart ({})", acked.len());
        db = open_admin_database(&instance, &root).await.map_err(|e| {
            format!("invariant failed: acknowledged database unavailable after restart: {e}")
        })?;
        check_acknowledged(&db, &acked).await?;
        if let Some(step) = unacked {
            let present = bounded("read", read_step(&db, step)).await?.is_some();
            eprintln!(
                "info: unacknowledged step-{step} present_after_restart={present} (either is allowed)"
            );
        }
        client = instance;
    }

    eprintln!(
        "check: all acknowledged writes on the restarted daemon ({})",
        acked.len()
    );
    check_acknowledged(&db, &acked).await?;
    let after = acked.iter().filter(|(step, _)| *step > restart_at).count();
    drop(db);
    drop(client);
    drop(daemon);
    Ok(format!(
        "{} acknowledged writes survived restart at step-{restart_at}; {after} written after it",
        acked.len()
    ))
}
