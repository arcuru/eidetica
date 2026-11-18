use std::{env, process::ExitCode, sync::Arc};

use eidetica::{
    Cluster,
    auth::{AuthKey, Permission},
    crdt::Doc,
    store::DocStore,
    testing::{SimLoopback, SimNetwork, add_auth_keys, set_global_auth_key},
};
use rand::{Rng, SeedableRng, rngs::StdRng};

const MAX_STEPS: usize = 1000;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    let usage = format!(
        "usage: runtime-test interleaved-sync SEED STEPS [--skip-final-exchange]\n\
         SEED is a u64; STEPS is in 1..={MAX_STEPS}. Use `timeout` to bound wall time."
    );
    if args == ["--help"] {
        println!("{usage}");
        return ExitCode::SUCCESS;
    }

    let parsed = match args.as_slice() {
        [workload, seed, steps, rest @ ..]
            if workload == "interleaved-sync"
                && (rest.is_empty() || rest == ["--skip-final-exchange"]) =>
        {
            seed.parse::<u64>().ok().zip(steps.parse::<usize>().ok())
        }
        _ => None,
    };
    let Some((seed, steps)) = parsed.filter(|(_, steps)| (1..=MAX_STEPS).contains(steps)) else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };

    let skip_final = args.len() == 4;
    eprintln!(
        "workload=interleaved-sync seed={seed} steps={steps} skip_final_exchange={skip_final}"
    );
    match interleaved_sync(seed, steps, skip_final).await {
        Ok(()) => {
            eprintln!("PASS interleaved-sync: {steps} writes present on both peers");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("FAIL interleaved-sync: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn interleaved_sync(
    seed: u64,
    steps: usize,
    skip_final: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
