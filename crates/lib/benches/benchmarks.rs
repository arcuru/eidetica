use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use eidetica::{
    Instance, backend::database::InMemory, instance::LegacyInstanceOps, store::DocStore,
};
use std::hint::black_box;
use tokio::runtime::Runtime;

/// Creates a fresh empty tree with in-memory backend for benchmarking
/// Returns both Instance and Database to keep Instance alive
async fn setup_tree_async() -> (Instance, eidetica::Database) {
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend)
        .await
        .expect("Benchmark setup failed");
    instance
        .add_private_key("BENCH_KEY")
        .await
        .expect("Failed to add benchmark key");
    let db = instance
        .new_database_default("BENCH_KEY")
        .await
        .expect("Failed to create tree");
    (instance, db)
}

fn setup_tree(rt: &Runtime) -> (Instance, eidetica::Database) {
    rt.block_on(setup_tree_async())
}

/// Creates a tree pre-populated with the specified number of key-value entries
/// Each entry has format "key_N" -> "value_N" where N is the entry index
async fn setup_tree_with_entries_async(entry_count: usize) -> (Instance, eidetica::Database) {
    let (instance, tree) = setup_tree_async().await;

    for i in 0..entry_count {
        let op = tree
            .new_transaction()
            .await
            .expect("Failed to start operation");
        let doc_store = op
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get DocStore");

        doc_store
            .set(format!("key_{i}"), format!("value_{i}"))
            .await
            .expect("Failed to set value");

        op.commit().await.expect("Failed to commit operation");
    }

    (instance, tree)
}

fn setup_tree_with_entries(rt: &Runtime, entry_count: usize) -> (Instance, eidetica::Database) {
    rt.block_on(setup_tree_with_entries_async(entry_count))
}

/// Benchmarks adding a single entry to trees of varying sizes
/// Measures how insertion performance scales with existing tree size
/// Creates fresh trees for each measurement to avoid accumulated state effects
fn bench_add_entries(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("add_entries");

    for tree_size in [0, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("single_entry", tree_size),
            tree_size,
            |b, &tree_size| {
                b.iter_with_setup(
                    || setup_tree_with_entries(&rt, tree_size),
                    |(_instance, tree)| {
                        rt.block_on(async {
                            let op = tree
                                .new_transaction()
                                .await
                                .expect("Failed to start operation");
                            let doc_store = op
                                .get_store::<DocStore>("data")
                                .await
                                .expect("Failed to get DocStore");

                            doc_store
                                .set(
                                    black_box(&format!("new_key_{tree_size}")),
                                    black_box(format!("new_value_{tree_size}").as_str()),
                                )
                                .await
                                .expect("Failed to set value");

                            op.commit().await.expect("Failed to commit operation");
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmarks batch insertion of multiple key-value pairs within a single operation
/// Tests atomic operation overhead vs per-KV-pair costs
/// Throughput metrics allow comparing efficiency per key-value pair
fn bench_batch_add_entries(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("batch_add_entries");

    for batch_size in [1, 10, 50, 100].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("batch", batch_size),
            batch_size,
            |b, &batch_size| {
                b.iter_with_setup(
                    || setup_tree(&rt),
                    |(_instance, tree)| {
                        rt.block_on(async {
                            let op = tree
                                .new_transaction()
                                .await
                                .expect("Failed to start operation");
                            let doc_store = op
                                .get_store::<DocStore>("data")
                                .await
                                .expect("Failed to get DocStore");

                            for i in 0..batch_size {
                                doc_store
                                    .set(
                                        black_box(&format!("batch_key_{i}")),
                                        black_box(format!("batch_value_{i}").as_str()),
                                    )
                                    .await
                                    .expect("Failed to set value");
                            }

                            op.commit().await.expect("Failed to commit operation");
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmarks incremental insertion into the same growing tree
/// Unlike bench_add_entries, this reuses the same tree across iterations
/// Measures amortized insertion cost as the tree continuously grows
/// Useful for understanding long-term performance characteristics
fn bench_incremental_add_entries(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("incremental_add_entries");

    for initial_size in [0, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("incremental_single", initial_size),
            initial_size,
            |b, &initial_size| {
                let (_instance, tree) = setup_tree_with_entries(&rt, initial_size);
                let mut counter = initial_size;

                b.iter(|| {
                    rt.block_on(async {
                        let op = tree
                            .new_transaction()
                            .await
                            .expect("Failed to start operation");
                        let doc_store = op
                            .get_store::<DocStore>("data")
                            .await
                            .expect("Failed to get DocStore");

                        doc_store
                            .set(
                                black_box(&format!("inc_key_{counter}")),
                                black_box(format!("inc_value_{counter}").as_str()),
                            )
                            .await
                            .expect("Failed to set value");

                        op.commit().await.expect("Failed to commit operation");
                        counter += 1;
                    });
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks read access to entries in trees of varying sizes
/// Tests lookup performance scaling with tree size
/// Always accesses the middle entry to avoid edge cases
fn bench_access_entries(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("access_entries");

    for tree_size in [10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("random_access", tree_size),
            tree_size,
            |b, &tree_size| {
                let (_instance, tree) = setup_tree_with_entries(&rt, tree_size);
                let target_key = format!("key_{}", tree_size / 2);

                b.iter(|| {
                    rt.block_on(async {
                        let op = tree
                            .new_transaction()
                            .await
                            .expect("Failed to start operation");
                        let doc_store = op
                            .get_store::<DocStore>("data")
                            .await
                            .expect("Failed to get DocStore");

                        let _value = doc_store
                            .get(black_box(&target_key))
                            .await
                            .expect("Failed to get value");
                    });
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks core tree infrastructure operations
/// Measures overhead of tree creation and operation initialization
/// Tests how operation creation scales with tree size
fn bench_tree_operations(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("tree_operations");

    group.bench_function("create_tree", |b| {
        b.iter(|| {
            rt.block_on(async {
                let backend = Box::new(InMemory::new());
                let db = Instance::open(backend)
                    .await
                    .expect("Benchmark setup failed");
                db.add_private_key("BENCH_KEY")
                    .await
                    .expect("Failed to add benchmark key");
                black_box(
                    db.new_database_default("BENCH_KEY")
                        .await
                        .expect("Failed to create tree"),
                );
            });
        });
    });

    for tree_size in [0, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("create_operation", tree_size),
            tree_size,
            |b, &tree_size| {
                let (_instance, tree) = setup_tree_with_entries(&rt, tree_size);

                b.iter(|| {
                    rt.block_on(async {
                        let _op = black_box(
                            tree.new_transaction()
                                .await
                                .expect("Failed to start operation"),
                        );
                    });
                });
            },
        );
    }

    group.finish();
}

/// Custom Criterion configuration for consistent benchmarking
/// Fixed sample size ensures reproducible results across different machines
fn criterion_config() -> Criterion {
    Criterion::default().sample_size(50).configure_from_args()
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets =
        bench_add_entries,
        bench_batch_add_entries,
        bench_incremental_add_entries,
        bench_access_entries,
        bench_tree_operations,
}
criterion_main!(benches);
