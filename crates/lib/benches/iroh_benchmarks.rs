use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use eidetica::{
    Instance,
    backend::database::InMemory,
    entry::Entry,
    sync::{Sync, peer_types::Address, transports::iroh::IrohTransport},
};
use iroh::RelayMode;
use std::{hint::black_box, sync::Arc, time::Duration};

/// Creates a root entry for benchmarking. Content is irrelevant; each call produces a unique entry.
fn create_root_entry() -> Entry {
    Entry::root_builder()
        .build()
        .expect("Benchmark entry should build successfully")
}

// Helper function to setup sync engines for benchmarking
async fn setup_iroh_sync_pair() -> (Arc<Instance>, Sync, Arc<Instance>, Sync, Address) {
    // Create databases
    let base_db1 = Arc::new(
        Instance::open(Box::new(InMemory::new()))
            .await
            .expect("Benchmark setup failed"),
    );
    let base_db2 = Arc::new(
        Instance::open(Box::new(InMemory::new()))
            .await
            .expect("Benchmark setup failed"),
    );

    // Create sync engines
    let sync1 = Sync::new((*base_db1).clone()).await.unwrap();
    let sync2 = Sync::new((*base_db2).clone()).await.unwrap();

    // Configure Iroh transports for local testing (no relays for consistent benchmarks)
    sync1
        .register_transport(
            "iroh",
            IrohTransport::builder().relay_mode(RelayMode::Disabled),
        )
        .await
        .unwrap();
    sync2
        .register_transport(
            "iroh",
            IrohTransport::builder().relay_mode(RelayMode::Disabled),
        )
        .await
        .unwrap();

    // Start servers
    sync1.accept_connections().await.unwrap();
    sync2.accept_connections().await.unwrap();

    // Allow time for initialization
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Setup peer relationship
    let addr2 = sync2.get_server_address().await.unwrap();
    let pubkey2 = sync2.get_device_id().unwrap();

    sync1
        .register_peer(&pubkey2, Some("bench_peer"))
        .await
        .unwrap();
    sync1
        .add_peer_address(&pubkey2, Address::iroh(&addr2))
        .await
        .unwrap();

    (base_db1, sync1, base_db2, sync2, Address::iroh(&addr2))
}

/// Measures Iroh P2P sync round-trip time: sending entries, peer processing, and acknowledgment.
/// Each entry is an independent root entry; content is irrelevant - this benchmarks transport throughput.
/// Entry creation is excluded from timing.
fn bench_iroh_sync_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Setup connection once for all benchmarks
    let (base_db1, sync1, _base_db2, sync2, addr2) =
        rt.block_on(async { setup_iroh_sync_pair().await });

    let mut group = c.benchmark_group("iroh_sync_throughput");
    // Reduce sample size and measurement time for network benchmarks
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    // Test different entry counts to measure throughput scaling
    for entry_count in [1, 5, 10].iter() {
        group.throughput(Throughput::Elements(*entry_count as u64));

        group.bench_with_input(
            BenchmarkId::new("entries", entry_count),
            entry_count,
            |b, &entry_count| {
                b.iter_with_setup(
                    || {
                        // Setup: create and store entries (not timed)
                        rt.block_on(async {
                            let entries: Vec<Entry> =
                                (0..entry_count).map(|_| create_root_entry()).collect();
                            for entry in &entries {
                                base_db1
                                    .backend()
                                    .put_verified(entry.clone())
                                    .await
                                    .unwrap();
                            }
                            entries
                        })
                    },
                    |entries| {
                        // Routine: only sync is timed
                        rt.block_on(async {
                            sync1
                                .send_entries(black_box(&entries), black_box(&addr2))
                                .await
                                .expect("Sync failed");
                        });
                    },
                );
            },
        );
    }

    group.finish();

    // Cleanup after all benchmarking is done
    rt.block_on(async {
        sync1.stop_server().await.unwrap();
        sync2.stop_server().await.unwrap();
    });
}

/// Measures time to establish an Iroh P2P connection between two peers.
/// Includes transport initialization, server startup, and peer registration.
/// Cleanup is excluded from timing.
fn bench_iroh_connection_setup(c: &mut Criterion) {
    let mut group = c.benchmark_group("iroh_connection");
    // Reduce sample size and measurement time for network benchmarks
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("setup", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        b.iter_custom(|iters| {
            rt.block_on(async {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = std::time::Instant::now();
                    let (_, sync1, _, sync2, _) = setup_iroh_sync_pair().await;
                    total += start.elapsed();
                    // Cleanup
                    sync1.stop_server().await.unwrap();
                    sync2.stop_server().await.unwrap();
                }
                total
            })
        });
    });

    group.finish();
}

criterion_group!(
    iroh_benches,
    bench_iroh_sync_throughput,
    bench_iroh_connection_setup,
);
criterion_main!(iroh_benches);
