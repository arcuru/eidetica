use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
use eidetica::subtree::KVStore;

/// Creates a fresh empty tree with in-memory backend for benchmarking
fn setup_tree() -> eidetica::Tree {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    db.new_tree_default().expect("Failed to create tree")
}

/// Creates a tree pre-populated with the specified number of key-value entries
/// Each entry has format "key_N" -> "value_N" where N is the entry index
fn setup_tree_with_entries(entry_count: usize) -> eidetica::Tree {
    let tree = setup_tree();

    for i in 0..entry_count {
        let op = tree.new_operation().expect("Failed to start operation");
        let kv_store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get KVStore");

        kv_store
            .set(format!("key_{i}"), format!("value_{i}"))
            .expect("Failed to set value");

        op.commit().expect("Failed to commit operation");
    }

    tree
}

/// Benchmarks adding a single entry to trees of varying sizes
/// Measures how insertion performance scales with existing tree size
/// Creates fresh trees for each measurement to avoid accumulated state effects
fn bench_add_entries(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_entries");

    for tree_size in [0, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("single_entry", tree_size),
            tree_size,
            |b, &tree_size| {
                b.iter_with_setup(
                    || setup_tree_with_entries(tree_size),
                    |tree| {
                        let op = tree.new_operation().expect("Failed to start operation");
                        let kv_store = op
                            .get_subtree::<KVStore>("data")
                            .expect("Failed to get KVStore");

                        kv_store
                            .set(
                                black_box(&format!("new_key_{tree_size}")),
                                black_box(&format!("new_value_{tree_size}")),
                            )
                            .expect("Failed to set value");

                        op.commit().expect("Failed to commit operation");
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
    let mut group = c.benchmark_group("batch_add_entries");

    for batch_size in [1, 10, 50, 100].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("batch", batch_size),
            batch_size,
            |b, &batch_size| {
                b.iter_with_setup(setup_tree, |tree| {
                    let op = tree.new_operation().expect("Failed to start operation");
                    let kv_store = op
                        .get_subtree::<KVStore>("data")
                        .expect("Failed to get KVStore");

                    for i in 0..batch_size {
                        kv_store
                            .set(
                                black_box(&format!("batch_key_{i}")),
                                black_box(&format!("batch_value_{i}")),
                            )
                            .expect("Failed to set value");
                    }

                    op.commit().expect("Failed to commit operation");
                });
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
    let mut group = c.benchmark_group("incremental_add_entries");

    for initial_size in [0, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("incremental_single", initial_size),
            initial_size,
            |b, &initial_size| {
                let tree = setup_tree_with_entries(initial_size);
                let mut counter = initial_size;

                b.iter(|| {
                    let op = tree.new_operation().expect("Failed to start operation");
                    let kv_store = op
                        .get_subtree::<KVStore>("data")
                        .expect("Failed to get KVStore");

                    kv_store
                        .set(
                            black_box(&format!("inc_key_{counter}")),
                            black_box(&format!("inc_value_{counter}")),
                        )
                        .expect("Failed to set value");

                    op.commit().expect("Failed to commit operation");
                    counter += 1;
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
    let mut group = c.benchmark_group("access_entries");

    for tree_size in [10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("random_access", tree_size),
            tree_size,
            |b, &tree_size| {
                let tree = setup_tree_with_entries(tree_size);
                let target_key = format!("key_{}", tree_size / 2);

                b.iter(|| {
                    let op = tree.new_operation().expect("Failed to start operation");
                    let kv_store = op
                        .get_subtree::<KVStore>("data")
                        .expect("Failed to get KVStore");

                    let _value = kv_store
                        .get(black_box(&target_key))
                        .expect("Failed to get value");
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
    let mut group = c.benchmark_group("tree_operations");

    group.bench_function("create_tree", |b| {
        b.iter(|| {
            let backend = Box::new(InMemoryBackend::new());
            let db = BaseDB::new(backend);
            black_box(db.new_tree_default().expect("Failed to create tree"));
        });
    });

    for tree_size in [0, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("create_operation", tree_size),
            tree_size,
            |b, &tree_size| {
                let tree = setup_tree_with_entries(tree_size);

                b.iter(|| {
                    let _op = black_box(tree.new_operation().expect("Failed to start operation"));
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
