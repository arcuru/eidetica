mod helpers;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use eidetica::{
    Database, Instance, backend::database::InMemory, crdt::Doc, store::DocStore, user::User,
};
use std::hint::black_box;
use tokio::runtime::Runtime;

use helpers::setup_tree_async;

fn setup_tree(rt: &Runtime) -> (Instance, User, Database) {
    rt.block_on(setup_tree_async())
}

/// Creates a tree pre-populated with the specified number of key-value entries
/// Each entry has format "key_N" -> "value_N" where N is the entry index
async fn setup_tree_with_entries_async(entry_count: usize) -> (Instance, User, Database) {
    let (instance, user, tree) = setup_tree_async().await;

    for i in 0..entry_count {
        let txn = tree
            .new_transaction()
            .await
            .expect("Failed to start transaction");
        let doc_store = txn
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get DocStore");

        doc_store
            .set(format!("key_{i}"), format!("value_{i}"))
            .await
            .expect("Failed to set value");

        txn.commit().await.expect("Failed to commit transaction");
    }

    (instance, user, tree)
}

fn setup_tree_with_entries(rt: &Runtime, entry_count: usize) -> (Instance, User, Database) {
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
                    |(_instance, _user, tree)| {
                        rt.block_on(async {
                            let txn = tree
                                .new_transaction()
                                .await
                                .expect("Failed to start transaction");
                            let doc_store = txn
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

                            txn.commit().await.expect("Failed to commit transaction");
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmarks batch insertion of multiple key-value pairs within a single transaction
/// Tests transaction overhead vs per-KV-pair costs
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
                    |(_instance, _user, tree)| {
                        rt.block_on(async {
                            let txn = tree
                                .new_transaction()
                                .await
                                .expect("Failed to start transaction");
                            let doc_store = txn
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

                            txn.commit().await.expect("Failed to commit transaction");
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
                let (_instance, _user, tree) = setup_tree_with_entries(&rt, initial_size);
                let mut counter = initial_size;

                b.iter(|| {
                    rt.block_on(async {
                        let txn = tree
                            .new_transaction()
                            .await
                            .expect("Failed to start transaction");
                        let doc_store = txn
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

                        txn.commit().await.expect("Failed to commit transaction");
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
                let (_instance, _user, tree) = setup_tree_with_entries(&rt, tree_size);
                let target_key = format!("key_{}", tree_size / 2);

                b.iter(|| {
                    rt.block_on(async {
                        let txn = tree
                            .new_transaction()
                            .await
                            .expect("Failed to start transaction");
                        let doc_store = txn
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
/// Measures overhead of tree creation and transaction initialization
/// Tests how transaction creation scales with tree size
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
                let instance = Instance::open(backend)
                    .await
                    .expect("Benchmark setup failed");

                // Create and login user
                instance
                    .create_user("bench_user", None)
                    .await
                    .expect("Failed to create user");
                let mut user = instance
                    .login_user("bench_user", None)
                    .await
                    .expect("Failed to login user");

                let key_id = user.get_default_key().expect("Failed to get default key");
                black_box(
                    user.create_database(Doc::new(), &key_id)
                        .await
                        .expect("Failed to create database"),
                );
            });
        });
    });

    for tree_size in [0, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("create_transaction", tree_size),
            tree_size,
            |b, &tree_size| {
                let (_instance, _user, tree) = setup_tree_with_entries(&rt, tree_size);

                b.iter(|| {
                    rt.block_on(async {
                        let _txn = black_box(
                            tree.new_transaction()
                                .await
                                .expect("Failed to start transaction"),
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
