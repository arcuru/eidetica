use std::{sync::Arc, time::Duration};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use eidetica::{
    entry::{Entry, ID},
    sync::{peer_types::Address, transports::iroh::IrohTransport},
};
use iroh::RelayMode;

// Helper function to create test entries
fn create_entry_with_parents(tree_id: &str, parents: Vec<ID>) -> Entry {
    let mut builder = Entry::builder(tree_id);
    if !parents.is_empty() {
        builder = builder.set_parents(parents);
    }
    builder.build()
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
    let base_db1 = Arc::new(Instance::new(Box::new(InMemory::new())));
    let base_db2 = Arc::new(Instance::new(Box::new(InMemory::new())));

    // Create sync engines
    let mut sync1 = Sync::new(base_db1.backend().clone()).unwrap();
    let mut sync2 = Sync::new(base_db2.backend().clone()).unwrap();

    // Configure Iroh transports for local testing (no relays for consistent benchmarks)
    let transport1 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();
    let transport2 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();

    sync1.enable_iroh_transport_with_config(transport1).unwrap();
    sync2.enable_iroh_transport_with_config(transport2).unwrap();

    // Start servers
    sync1.start_server_async("ignored").await.unwrap();
    sync2.start_server_async("ignored").await.unwrap();

    // Allow time for initialization
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Setup peer relationship
    let addr2 = sync2.get_server_address_async().await.unwrap();
    let pubkey2 = sync2.get_device_public_key().unwrap();

    sync1.register_peer(&pubkey2, Some("bench_peer")).unwrap();
    sync1
        .add_peer_address(&pubkey2, Address::iroh(&addr2))
        .unwrap();

    (base_db1, sync1, base_db2, sync2, Address::iroh(&addr2))
}

fn bench_iroh_sync_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Setup connection once for all benchmarks
    let (base_db1, mut sync1, _base_db2, mut sync2, addr2) =
        rt.block_on(async { setup_iroh_sync_pair().await });

    // Add authentication key
    base_db1.add_private_key("bench_key").unwrap();

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
                                base_db1.backend().put_verified(entry.clone()).unwrap();
                                entries.push(entry);
                            }

                            // Measure ONLY the sync time, not setup/teardown
                            let start = std::time::Instant::now();
                            let result = sync1
                                .send_entries_async(black_box(&entries), black_box(&addr2))
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
        sync1.stop_server_async().await.unwrap();
        sync2.stop_server_async().await.unwrap();
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
                    let (_base_db1, mut sync1, _base_db2, mut sync2, _addr2) =
                        black_box(setup_iroh_sync_pair().await);

                    let duration = start.elapsed();
                    total_duration += duration;

                    // Cleanup
                    sync1.stop_server_async().await.unwrap();
                    sync2.stop_server_async().await.unwrap();
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
