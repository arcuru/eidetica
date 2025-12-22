use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use eidetica::{
    entry::{Entry, ID},
    instance::LegacyInstanceOps,
    sync::{peer_types::Address, transports::iroh::IrohTransport},
};
use iroh::RelayMode;
use std::{hint::black_box, sync::Arc, time::Duration};

// Helper function to create test entries
fn create_entry_with_parents(tree_id: &str, parents: Vec<ID>) -> Entry {
    let mut builder = Entry::builder(tree_id);
    if !parents.is_empty() {
        builder = builder.set_parents(parents);
    }
    builder
        .build()
        .expect("Benchmark entry should build successfully")
}

// Helper function to setup sync engines for benchmarking
async fn setup_iroh_sync_pair() -> (
    Arc<eidetica::Instance>,
    eidetica::sync::Sync,
    Arc<eidetica::Instance>,
    eidetica::sync::Sync,
    Address,
) {
    use eidetica::{Instance, backend::database::InMemory, sync::Sync};

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
    let transport1 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();
    let transport2 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();

    sync1
        .enable_iroh_transport_with_config(transport1)
        .await
        .unwrap();
    sync2
        .enable_iroh_transport_with_config(transport2)
        .await
        .unwrap();

    // Start servers
    sync1.start_server("ignored").await.unwrap();
    sync2.start_server("ignored").await.unwrap();

    // Allow time for initialization
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Setup peer relationship
    let addr2 = sync2.get_server_address().await.unwrap();
    let pubkey2 = sync2.get_device_public_key().await.unwrap();

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

fn bench_iroh_sync_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Setup connection once for all benchmarks
    let (base_db1, sync1, _base_db2, sync2, addr2) =
        rt.block_on(async { setup_iroh_sync_pair().await });

    // Add authentication key
    rt.block_on(async {
        base_db1.add_private_key("bench_key").await.unwrap();
    });

    let mut group = c.benchmark_group("iroh_sync_throughput");

    // Test different entry counts to measure throughput scaling
    for entry_count in [1, 10, 50, 100, 200].iter() {
        group.throughput(Throughput::Elements(*entry_count as u64));

        group.bench_with_input(
            BenchmarkId::new("entries", entry_count),
            entry_count,
            |b, &entry_count| {
                b.iter_custom(|iters| {
                    rt.block_on(async {
                        let mut total_duration = Duration::new(0, 0);

                        for iter in 0..iters {
                            // Create test entries for this iteration
                            let mut entries = Vec::new();
                            for i in 0..entry_count {
                                let entry = create_entry_with_parents(
                                    &format!("bench_tree_{iter}_{i}"),
                                    vec![],
                                );
                                base_db1
                                    .backend()
                                    .put_verified(entry.clone())
                                    .await
                                    .unwrap();
                                entries.push(entry);
                            }

                            // Measure ONLY the sync time, not setup/teardown
                            let start = std::time::Instant::now();
                            let result = sync1
                                .send_entries(black_box(&entries), black_box(&addr2))
                                .await;
                            let duration = start.elapsed();

                            // Ensure sync succeeded
                            assert!(result.is_ok(), "Sync failed: {:?}", result.err());

                            total_duration += duration;
                        }

                        total_duration
                    })
                });
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

fn bench_iroh_connection_setup(c: &mut Criterion) {
    c.bench_function("iroh_connection_setup", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        b.iter_custom(|iters| {
            rt.block_on(async {
                let mut total_duration = Duration::new(0, 0);

                for _iter in 0..iters {
                    let start = std::time::Instant::now();

                    // Measure time to set up Iroh P2P connection
                    let (_base_db1, sync1, _base_db2, sync2, _addr2) =
                        black_box(setup_iroh_sync_pair().await);

                    let duration = start.elapsed();
                    total_duration += duration;

                    // Cleanup
                    sync1.stop_server().await.unwrap();
                    sync2.stop_server().await.unwrap();
                }

                total_duration
            })
        });
    });
}

criterion_group!(
    iroh_benches,
    bench_iroh_sync_throughput,
    bench_iroh_connection_setup,
);
criterion_main!(iroh_benches);
