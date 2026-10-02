//! Lifetime bounds for failed address races.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use eidetica::{
    auth::crypto::PublicKey,
    entry::ID,
    sync::{Address, dial_attempts, reset_dial_attempts, transports::http::HttpTransport},
};

use super::helpers::{setup, setup_public_sync_enabled_server, start_sync_server};

const PEERS: usize = 2;
const ADDRESSES_PER_PEER: usize = 60;
const TOTAL_ATTEMPTS: usize = PEERS * ADDRESSES_PER_PEER;
const ADDRESS_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const ALLOWED_EXTRA_BYTES: usize = 128 * 1024;
const ALLOWED_EXTRA_ALLOCS: usize = 256;
/// Black-hole addresses racing one live answering peer.
const LOSER_ADDRESSES: usize = ADDRESSES_PER_PEER;
/// How long after the sync returns the losing dials may take to disappear.
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);

struct BlackHoles {
    addresses: Vec<Address>,
    accepted: Arc<AtomicUsize>,
    accepted_notify: Arc<tokio::sync::Notify>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl BlackHoles {
    async fn start(count: usize) -> Self {
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_notify = Arc::new(tokio::sync::Notify::new());
        let mut addresses = Vec::with_capacity(count);
        let mut tasks = Vec::with_capacity(count);

        for _ in 0..count {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind a black-hole listener");
            addresses.push(Address::http(
                listener
                    .local_addr()
                    .expect("read listener address")
                    .to_string(),
            ));
            let accepted = Arc::clone(&accepted);
            let accepted_notify = Arc::clone(&accepted_notify);
            tasks.push(tokio::spawn(async move {
                let (_connection, _) = listener.accept().await.expect("accept address attempt");
                accepted.fetch_add(1, Ordering::Release);
                accepted_notify.notify_waiters();
                std::future::pending::<()>().await;
            }));
        }

        Self {
            addresses,
            accepted,
            accepted_notify,
            tasks,
        }
    }

    async fn wait_for_all_accepts(&self) {
        tokio::time::timeout(ACCEPT_WAIT_TIMEOUT, async {
            loop {
                let notified = self.accepted_notify.notified();
                if self.accepted.load(Ordering::Acquire) == TOTAL_ATTEMPTS {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "only {} of {TOTAL_ATTEMPTS} configured address attempts reached their black-hole listeners within {ACCEPT_WAIT_TIMEOUT:?}",
                self.accepted.load(Ordering::Acquire),
            )
        });
    }

    async fn stop(self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// Failed address races must return close to the baseline allocation once
/// every address attempt times out.
///
/// The integration binary's global allocator needs nextest's process-per-test
/// mode to isolate the before and after samples.
#[tokio::test]
#[ignore = "Slow test: waits 30 seconds for failed address attempts"]
async fn failed_address_attempts_release_allocations_after_timeout() {
    if std::env::var("NEXTEST_EXECUTION_MODE").as_deref() != Ok("process-per-test") {
        eprintln!("skipping allocation diagnostic outside nextest process-per-test execution");
        return;
    }

    let black_holes = BlackHoles::start(TOTAL_ATTEMPTS).await;
    let (_instance, sync) = setup().await;
    sync.register_transport("http", HttpTransport::builder())
        .await
        .expect("register HTTP transport");

    let peers: Vec<_> = (0..PEERS).map(|_| PublicKey::random()).collect();
    for (peer, addresses) in peers
        .iter()
        .zip(black_holes.addresses.chunks_exact(ADDRESSES_PER_PEER))
    {
        sync.register_peer(peer, Some("unreachable peer"))
            .await
            .expect("register peer");
        for address in addresses {
            sync.add_peer_address(peer, address.clone())
                .await
                .expect("record black-hole address");
        }
    }
    assert_eq!(
        black_holes
            .addresses
            .chunks_exact(ADDRESSES_PER_PEER)
            .remainder()
            .len(),
        0,
        "each peer must receive the configured number of addresses"
    );

    let tree = ID::from_bytes("missing tree");
    let baseline = crate::live_allocations();
    let started = Instant::now();
    let ((first, second), ()) = tokio::join!(
        async {
            tokio::join!(
                sync.sync_tree_with_peer(&peers[0], &tree),
                sync.sync_tree_with_peer(&peers[1], &tree),
            )
        },
        black_holes.wait_for_all_accepts(),
    );
    assert!(first.is_err() && second.is_err(), "all addresses fail");
    assert!(
        started.elapsed() >= ADDRESS_ATTEMPT_TIMEOUT,
        "the black-holed attempts must remain alive through their 30-second deadline"
    );
    black_holes.stop().await;

    for _ in 0..10 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let after_timeout = crate::live_allocations();

    eprintln!(
        "failed address race: attempts={TOTAL_ATTEMPTS}, elapsed={:?}, baseline={baseline:?}, after_timeout={after_timeout:?}",
        started.elapsed(),
    );
    assert!(
        after_timeout.0 <= baseline.0 + ALLOWED_EXTRA_BYTES,
        "live bytes remained more than {ALLOWED_EXTRA_BYTES} bytes above baseline after timeout: baseline={baseline:?}, after={after_timeout:?}"
    );
    assert!(
        after_timeout.1 <= baseline.1 + ALLOWED_EXTRA_ALLOCS,
        "live allocations remained more than {ALLOWED_EXTRA_ALLOCS} above baseline after timeout: baseline={baseline:?}, after={after_timeout:?}"
    );
}

/// Losing dials must disappear once a winner emerges, not at their deadline.
///
/// The test above proves eventual reclamation when every attempt fails at
/// its own 30-second deadline. This one proves the other half, RFC 8305
/// section 5: with a live peer answering behind a wall of black holes, every
/// dial the race started must be torn down shortly after the winner is
/// selected. Each attempt carries a liveness handle; a `Weak::upgrade()`
/// that still succeeds means that dial's task — its connection, buffers,
/// and registration work — survived the winner.
///
/// Unlike the allocation assertion this needs no allocator baseline and no
/// deadline wait, so it runs in milliseconds.
#[tokio::test]
async fn address_race_losers_tear_down_once_a_winner_emerges() {
    if std::env::var("NEXTEST_EXECUTION_MODE").as_deref() != Ok("process-per-test") {
        eprintln!("skipping dial-liveness diagnostic outside nextest process-per-test execution");
        return;
    }

    let black_holes = BlackHoles::start(LOSER_ADDRESSES).await;
    let (_server_instance, _server_user, _server_key_id, _server_db, tree, server_sync) =
        setup_public_sync_enabled_server("winner_server", "winner_key", "winner_database").await;
    let server_address = start_sync_server(&server_sync).await;
    let server_peer = _server_instance.id();

    let (_instance, sync) = setup().await;
    sync.register_transport("http", HttpTransport::builder())
        .await
        .expect("register HTTP transport");

    sync.register_peer(&server_peer, Some("answering peer"))
        .await
        .expect("register peer");
    for address in &black_holes.addresses {
        sync.add_peer_address(&server_peer, address.clone())
            .await
            .expect("record black-hole address");
    }
    sync.add_peer_address(&server_peer, server_address.clone())
        .await
        .expect("record the live address");

    // Observe only the race this test is about to start.
    reset_dial_attempts();

    sync.sync_tree_with_peer(&server_peer, &tree)
        .await
        .expect("bootstrap through the winning address");

    // One handle per raced address. Attempts whose tasks had not run yet
    // when the winner was selected are recorded after the sync returns, so
    // wait for the full set to appear before requiring all of them dead.
    let expected_attempts = LOSER_ADDRESSES + 1;
    let deadline = Instant::now() + TEARDOWN_GRACE;
    loop {
        let attempts = dial_attempts();
        let alive = attempts
            .iter()
            .filter(|attempt| attempt.upgrade().is_some())
            .count();
        if attempts.len() == expected_attempts && alive == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{} of {expected_attempts} dial attempts recorded, {alive} still alive more than \
             {TEARDOWN_GRACE:?} after the winner emerged — losing dials are not torn down",
            attempts.len(),
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    black_holes.stop().await;
    server_sync.stop_server().await.expect("stop server");
}
