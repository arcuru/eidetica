mod helpers;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use eidetica::{
    Database,
    backend::{BackendImpl, database::InMemory},
    entry::ID,
    store::DocStore,
};
use std::hint::black_box;

use helpers::setup_tree_async;

/// Create a linear chain of entries for testing merge base performance
async fn create_linear_chain(tree: &Database, length: usize) -> Vec<ID> {
    let mut entry_ids = Vec::with_capacity(length);

    for i in 0..length {
        let op = tree.new_transaction().await.expect("Failed to create op");
        let kv = op
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get DocStore");
        kv.set(format!("key{i}"), format!("value{i}"))
            .await
            .expect("Failed to set value");
        let entry_id = op.commit().await.expect("Failed to commit");
        entry_ids.push(entry_id);
    }

    entry_ids
}

/// Create a diamond pattern for testing merge scenarios
async fn create_diamond_pattern(tree: &Database) -> (Vec<ID>, ID) {
    // Create root A
    let op_a = tree.new_transaction().await.expect("Failed to create op");
    let kv_a = op_a
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get DocStore");
    kv_a.set("key_a", "value_a")
        .await
        .expect("Failed to set value");
    let entry_a = op_a.commit().await.expect("Failed to commit");

    // Create B and C from A
    let op_b = tree
        .new_transaction_with_tips(std::slice::from_ref(&entry_a))
        .await
        .expect("Failed to create op");
    let kv_b = op_b
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get DocStore");
    kv_b.set("key_b", "value_b")
        .await
        .expect("Failed to set value");
    let entry_b = op_b.commit().await.expect("Failed to commit");

    let op_c = tree
        .new_transaction_with_tips(std::slice::from_ref(&entry_a))
        .await
        .expect("Failed to create op");
    let kv_c = op_c
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get DocStore");
    kv_c.set("key_c", "value_c")
        .await
        .expect("Failed to set value");
    let entry_c = op_c.commit().await.expect("Failed to commit");

    // Return B and C for merge base testing (merge base should be A)
    (vec![entry_b, entry_c], entry_a)
}

/// Create multiple branches for testing tips performance
async fn create_branching_tree(
    tree: &Database,
    num_branches: usize,
    entries_per_branch: usize,
) -> Vec<Vec<ID>> {
    let mut branches = Vec::new();

    // Get the root entry to branch from
    let backend = tree.backend().expect("Failed to get backend");
    let root_entries = backend
        .get_tree(tree.root_id())
        .await
        .expect("Failed to get tree");
    let root_entry = root_entries
        .first()
        .expect("Tree should have at least one entry")
        .id();

    for branch_idx in 0..num_branches {
        let mut branch_entries = Vec::new();
        let mut current_tip = root_entry.clone();

        for entry_idx in 0..entries_per_branch {
            let op = tree
                .new_transaction_with_tips([current_tip])
                .await
                .expect("Failed to create op");
            let kv = op
                .get_store::<DocStore>("data")
                .await
                .expect("Failed to get DocStore");
            kv.set(format!("branch_{branch_idx}_entry_{entry_idx}"), "value")
                .await
                .expect("Failed to set value");
            let entry_id = op.commit().await.expect("Failed to commit");
            branch_entries.push(entry_id.clone());
            current_tip = entry_id; // Update tip for next entry in this branch
        }

        branches.push(branch_entries);
    }

    branches
}

/// Create a large tree with specified structure
async fn create_large_tree(tree: &Database, num_entries: usize, structure: &str) -> Vec<ID> {
    let mut entry_ids = Vec::new();

    match structure {
        "linear" => {
            for i in 0..num_entries {
                let op = tree.new_transaction().await.expect("Failed to create op");
                let kv = op
                    .get_store::<DocStore>("data")
                    .await
                    .expect("Failed to get DocStore");
                kv.set(format!("key_{i}"), format!("value_{i}"))
                    .await
                    .expect("Failed to set value");
                let entry_id = op.commit().await.expect("Failed to commit");
                entry_ids.push(entry_id);
            }
        }

        "wide" => {
            // Create many siblings from root
            // Get the root entry to branch from
            let backend = tree.backend().expect("Failed to get backend");
            let root_entries = backend
                .get_tree(tree.root_id())
                .await
                .expect("Failed to get tree");
            let root_entry = root_entries
                .first()
                .expect("Tree should have at least one entry")
                .id();

            for i in 0..num_entries {
                let op = tree
                    .new_transaction_with_tips(std::slice::from_ref(&root_entry))
                    .await
                    .expect("Failed to create op");
                let kv = op
                    .get_store::<DocStore>("data")
                    .await
                    .expect("Failed to get DocStore");
                kv.set(format!("key_{i}"), format!("value_{i}"))
                    .await
                    .expect("Failed to set value");
                let entry_id = op.commit().await.expect("Failed to commit");
                entry_ids.push(entry_id);
            }
        }

        _ => {
            // Default to linear
            for i in 0..num_entries {
                let op = tree.new_transaction().await.expect("Failed to create op");
                let kv = op
                    .get_store::<DocStore>("data")
                    .await
                    .expect("Failed to get DocStore");
                kv.set(format!("key_{i}"), format!("value_{i}"))
                    .await
                    .expect("Failed to set value");
                let entry_id = op.commit().await.expect("Failed to commit");
                entry_ids.push(entry_id);
            }
        }
    }

    entry_ids
}

/// Benchmark find_merge_base performance with linear chains
pub fn bench_merge_base_linear_chains(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("find_merge_base_linear");

    for chain_length in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("length", chain_length),
            &chain_length,
            |b, &length| {
                b.iter_with_setup(
                    || {
                        rt.block_on(async {
                            let (_instance, _user, tree) = setup_tree_async().await;
                            let entry_ids = create_linear_chain(&tree, length).await;
                            (_instance, tree, entry_ids)
                        })
                    },
                    |(_instance, tree, entry_ids)| {
                        rt.block_on(async {
                            // Test merge base of first and last entries
                            let endpoints =
                                vec![entry_ids[0].clone(), entry_ids[length - 1].clone()];
                            let expected_merge_base = &entry_ids[0]; // In a linear chain, merge base of first and last is the first

                            // Access database to call find_merge_base
                            let backend = tree.backend().expect("Failed to get backend");
                            let in_memory = backend
                                .as_any()
                                .downcast_ref::<InMemory>()
                                .expect("Failed to downcast database");

                            let merge_base = in_memory
                                .find_merge_base(tree.root_id(), "data", &endpoints)
                                .await
                                .expect("Failed to find merge base");

                            // Verify correctness
                            assert_eq!(
                                &merge_base, expected_merge_base,
                                "Merge base mismatch in linear chain"
                            );
                            black_box(merge_base);
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark find_merge_base performance with diamond patterns
pub fn bench_merge_base_diamond_merge(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    c.bench_function("find_merge_base_diamond", |b| {
        b.iter_with_setup(
            || {
                rt.block_on(async {
                    let (_instance, _user, tree) = setup_tree_async().await;
                    let (test_entries, expected_merge_base) = create_diamond_pattern(&tree).await;
                    (_instance, tree, test_entries, expected_merge_base)
                })
            },
            |(_instance, tree, test_entries, expected_merge_base)| {
                rt.block_on(async {
                    let backend = tree.backend().expect("Failed to get backend");

                    let merge_base = backend
                        .find_merge_base(tree.root_id(), "data", &test_entries)
                        .await
                        .expect("Failed to find merge base");

                    // Verify correctness
                    assert_eq!(
                        merge_base, expected_merge_base,
                        "Merge base mismatch in diamond pattern"
                    );
                    black_box(merge_base);
                });
            },
        );
    });
}

/// Benchmark tips finding performance
pub fn bench_tips_finding(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("get_tips");

    for num_tips in [5, 10, 25] {
        group.bench_with_input(
            BenchmarkId::new("num_tips", num_tips),
            &num_tips,
            |b, &num| {
                b.iter_with_setup(
                    || {
                        rt.block_on(async {
                            let (_instance, _user, tree) = setup_tree_async().await;
                            let _ = create_branching_tree(&tree, num, 3).await; // Create branches as tips
                            (_instance, tree)
                        })
                    },
                    |(_instance, tree)| {
                        rt.block_on(async {
                            let backend = tree.backend().expect("Failed to get backend");
                            let tips = backend
                                .get_tips(tree.root_id())
                                .await
                                .expect("Failed to get tips");
                            black_box(tips);
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark tree traversal scalability
pub fn bench_tree_traversal_scalability(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("large_tree_operations");
    group.sample_size(10); // Reduce sample size for large tree operations

    let tree_sizes = [100, 500];
    let structures = ["linear", "wide"];

    for &size in &tree_sizes {
        for structure in &structures {
            group.throughput(Throughput::Elements(size as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("get_tree_{structure}"), size),
                &(size, structure),
                |b, &(size, structure)| {
                    b.iter_with_setup(
                        || {
                            rt.block_on(async {
                                let (_instance, _user, tree) = setup_tree_async().await;
                                let _ = create_large_tree(&tree, size, structure).await;
                                (_instance, tree)
                            })
                        },
                        |(_instance, tree)| {
                            rt.block_on(async {
                                let backend = tree.backend().expect("Failed to get backend");
                                let entries = backend
                                    .get_tree(tree.root_id())
                                    .await
                                    .expect("Failed to get tree");
                                black_box(entries);
                            });
                        },
                    );
                },
            );
        }
    }

    group.finish();
}

/// Benchmark CRDT merge operations
pub fn bench_crdt_merge_operations(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("crdt_computation");

    for tree_depth in [10, 20] {
        group.bench_with_input(
            BenchmarkId::new("depth", tree_depth),
            &tree_depth,
            |b, &depth| {
                b.iter_with_setup(
                    || {
                        rt.block_on(async {
                            let (_instance, _user, tree) = setup_tree_async().await;
                            let entry_ids = create_linear_chain(&tree, depth).await;
                            let tip_entry = entry_ids.last().unwrap().clone();
                            (_instance, tree, tip_entry)
                        })
                    },
                    |(_instance, tree, tip_entry)| {
                        rt.block_on(async {
                            let op = tree
                                .new_transaction_with_tips([tip_entry])
                                .await
                                .expect("Failed to create op");
                            let kv = op
                                .get_store::<DocStore>("data")
                                .await
                                .expect("Failed to get DocStore");

                            // Perform a simple operation that requires CRDT computation
                            kv.set("test_key", "test_value")
                                .await
                                .expect("Failed to set value");
                            let result = op.commit().await.expect("Failed to commit");
                            black_box(result);
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark individual tip validation
pub fn bench_tip_validation(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("is_tip");

    for tree_size in [50, 100] {
        group.bench_with_input(
            BenchmarkId::new("tree_size", tree_size),
            &tree_size,
            |b, &size| {
                b.iter_with_setup(
                    || {
                        rt.block_on(async {
                            let (_instance, _user, tree) = setup_tree_async().await;
                            let entry_ids = create_linear_chain(&tree, size).await;
                            let last_entry_id = entry_ids.last().unwrap().clone();
                            (_instance, tree, last_entry_id)
                        })
                    },
                    |(_instance, tree, last_entry_id)| {
                        rt.block_on(async {
                            let backend = tree.backend().expect("Failed to get backend");
                            let in_memory = backend
                                .as_any()
                                .downcast_ref::<InMemory>()
                                .expect("Failed to downcast database");

                            let is_tip = in_memory.is_tip(tree.root_id(), &last_entry_id).await;
                            black_box(is_tip);
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark get_tree_from_tips - the function optimized with batch CTE queries
pub fn bench_get_tree_from_tips(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");
    let mut group = c.benchmark_group("get_tree_from_tips");
    group.sample_size(10);

    let tree_sizes = [100, 500];
    let structures = ["linear", "wide"];

    for &size in &tree_sizes {
        for structure in &structures {
            group.throughput(Throughput::Elements(size as u64));
            group.bench_with_input(
                BenchmarkId::new(structure.to_string(), size),
                &(size, structure),
                |b, &(size, structure)| {
                    b.iter_with_setup(
                        || {
                            rt.block_on(async {
                                let (_instance, _user, tree) = setup_tree_async().await;
                                let entry_ids = create_large_tree(&tree, size, structure).await;
                                // Get the tips (last entries created)
                                let tips: Vec<ID> =
                                    entry_ids.iter().rev().take(3).cloned().collect();
                                (_instance, tree, tips)
                            })
                        },
                        |(_instance, tree, tips)| {
                            rt.block_on(async {
                                let backend = tree.backend().expect("Failed to get backend");
                                let entries = backend
                                    .get_tree_from_tips(tree.root_id(), &tips)
                                    .await
                                    .expect("Failed to get tree from tips");
                                black_box(entries);
                            });
                        },
                    );
                },
            );
        }
    }

    group.finish();
}

criterion_group! {
    name = backend_benches;
    config = Criterion::default().sample_size(30);
    targets = bench_merge_base_linear_chains, bench_merge_base_diamond_merge,
              bench_tips_finding, bench_tree_traversal_scalability,
              bench_crdt_merge_operations, bench_tip_validation,
              bench_get_tree_from_tips
}

criterion_main!(backend_benches);
