use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use eidetica::backend::{Database, database::InMemory};
use eidetica::basedb::BaseDB;
use eidetica::entry::ID;
use eidetica::subtree::KVStore;

/// Creates a fresh empty tree with in-memory backend for benchmarking
fn setup_tree() -> eidetica::Tree {
    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);
    db.add_private_key("BENCH_KEY")
        .expect("Failed to add benchmark key");
    db.new_tree_default("BENCH_KEY")
        .expect("Failed to create tree")
}

/// Create a linear chain of entries for testing LCA performance
fn create_linear_chain(tree: &eidetica::Tree, length: usize) -> Vec<ID> {
    let mut entry_ids = Vec::with_capacity(length);

    for i in 0..length {
        let op = tree.new_operation().expect("Failed to create op");
        let kv = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get KVStore");
        kv.set(format!("key{i}"), format!("value{i}"))
            .expect("Failed to set value");
        let entry_id = op.commit().expect("Failed to commit");
        entry_ids.push(entry_id);
    }

    entry_ids
}

/// Create a diamond pattern for testing merge scenarios  
fn create_diamond_pattern(tree: &eidetica::Tree) -> (Vec<ID>, ID) {
    // Create root A
    let op_a = tree.new_operation().expect("Failed to create op");
    let kv_a = op_a
        .get_subtree::<KVStore>("data")
        .expect("Failed to get KVStore");
    kv_a.set("key_a", "value_a").expect("Failed to set value");
    let entry_a = op_a.commit().expect("Failed to commit");

    // Create B and C from A
    let op_b = tree
        .new_operation_with_tips(std::slice::from_ref(&entry_a))
        .expect("Failed to create op");
    let kv_b = op_b
        .get_subtree::<KVStore>("data")
        .expect("Failed to get KVStore");
    kv_b.set("key_b", "value_b").expect("Failed to set value");
    let entry_b = op_b.commit().expect("Failed to commit");

    let op_c = tree
        .new_operation_with_tips(std::slice::from_ref(&entry_a))
        .expect("Failed to create op");
    let kv_c = op_c
        .get_subtree::<KVStore>("data")
        .expect("Failed to get KVStore");
    kv_c.set("key_c", "value_c").expect("Failed to set value");
    let entry_c = op_c.commit().expect("Failed to commit");

    // Return B and C for LCA testing (LCA should be A)
    (vec![entry_b, entry_c], entry_a)
}

/// Create multiple branches for testing tips performance
fn create_branching_tree(
    tree: &eidetica::Tree,
    num_branches: usize,
    entries_per_branch: usize,
) -> Vec<Vec<ID>> {
    let mut branches = Vec::new();

    // Get the root entry to branch from
    let backend = tree.backend();
    let root_entries = backend
        .get_tree(tree.root_id())
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
                .new_operation_with_tips([current_tip])
                .expect("Failed to create op");
            let kv = op
                .get_subtree::<KVStore>("data")
                .expect("Failed to get KVStore");
            kv.set(format!("branch_{branch_idx}_entry_{entry_idx}"), "value")
                .expect("Failed to set value");
            let entry_id = op.commit().expect("Failed to commit");
            branch_entries.push(entry_id.clone());
            current_tip = entry_id; // Update tip for next entry in this branch
        }

        branches.push(branch_entries);
    }

    branches
}

/// Create a large tree with specified structure
fn create_large_tree(tree: &eidetica::Tree, num_entries: usize, structure: &str) -> Vec<ID> {
    let mut entry_ids = Vec::new();

    match structure {
        "linear" => {
            for i in 0..num_entries {
                let op = tree.new_operation().expect("Failed to create op");
                let kv = op
                    .get_subtree::<KVStore>("data")
                    .expect("Failed to get KVStore");
                kv.set(format!("key_{i}"), format!("value_{i}"))
                    .expect("Failed to set value");
                let entry_id = op.commit().expect("Failed to commit");
                entry_ids.push(entry_id);
            }
        }

        "wide" => {
            // Create many siblings from root
            // Get the root entry to branch from
            let backend = tree.backend();
            let root_entries = backend
                .get_tree(tree.root_id())
                .expect("Failed to get tree");
            let root_entry = root_entries
                .first()
                .expect("Tree should have at least one entry")
                .id();

            for i in 0..num_entries {
                let op = tree
                    .new_operation_with_tips(std::slice::from_ref(&root_entry))
                    .expect("Failed to create op");
                let kv = op
                    .get_subtree::<KVStore>("data")
                    .expect("Failed to get KVStore");
                kv.set(format!("key_{i}"), format!("value_{i}"))
                    .expect("Failed to set value");
                let entry_id = op.commit().expect("Failed to commit");
                entry_ids.push(entry_id);
            }
        }

        _ => {
            // Default to linear
            for i in 0..num_entries {
                let op = tree.new_operation().expect("Failed to create op");
                let kv = op
                    .get_subtree::<KVStore>("data")
                    .expect("Failed to get KVStore");
                kv.set(format!("key_{i}"), format!("value_{i}"))
                    .expect("Failed to set value");
                let entry_id = op.commit().expect("Failed to commit");
                entry_ids.push(entry_id);
            }
        }
    }

    entry_ids
}

/// Benchmark find_lca performance with linear chains
pub fn bench_lca_linear_chains(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_lca_linear");

    for chain_length in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("length", chain_length),
            &chain_length,
            |b, &length| {
                b.iter_with_setup(
                    || {
                        let tree = setup_tree();
                        let entry_ids = create_linear_chain(&tree, length);
                        (tree, entry_ids)
                    },
                    |(tree, entry_ids)| {
                        // Test LCA of first and last entries
                        let endpoints = vec![entry_ids[0].clone(), entry_ids[length - 1].clone()];
                        let expected_lca = &entry_ids[0]; // In a linear chain, LCA of first and last is the first

                        // Access database to call find_lca
                        let backend = tree.backend();
                        let in_memory = backend
                            .as_any()
                            .downcast_ref::<InMemory>()
                            .expect("Failed to downcast database");

                        let lca = in_memory
                            .find_lca(tree.root_id(), "data", &endpoints)
                            .expect("Failed to find LCA");

                        // Verify correctness
                        assert_eq!(&lca, expected_lca, "LCA mismatch in linear chain");
                        black_box(lca);
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark find_lca performance with diamond patterns
pub fn bench_lca_diamond_merge(c: &mut Criterion) {
    c.bench_function("find_lca_diamond", |b| {
        b.iter_with_setup(
            || {
                let tree = setup_tree();
                let (test_entries, expected_lca) = create_diamond_pattern(&tree);
                (tree, test_entries, expected_lca)
            },
            |(tree, test_entries, expected_lca)| {
                let backend = tree.backend();
                let in_memory = backend
                    .as_any()
                    .downcast_ref::<InMemory>()
                    .expect("Failed to downcast backend");

                let lca = in_memory
                    .find_lca(tree.root_id(), "data", &test_entries)
                    .expect("Failed to find LCA");

                // Verify correctness
                assert_eq!(lca, expected_lca, "LCA mismatch in diamond pattern");
                black_box(lca);
            },
        );
    });
}

/// Benchmark height calculation performance
pub fn bench_tree_heights(c: &mut Criterion) {
    let mut group = c.benchmark_group("height_calculation");

    for depth in [10, 50] {
        group.bench_with_input(BenchmarkId::new("depth", depth), &depth, |b, &depth| {
            b.iter_with_setup(
                || {
                    let tree = setup_tree();
                    let _ = create_linear_chain(&tree, depth);
                    tree
                },
                |tree| {
                    let backend = tree.backend();
                    let in_memory = backend
                        .as_any()
                        .downcast_ref::<InMemory>()
                        .expect("Failed to downcast backend");

                    let heights = in_memory
                        .calculate_heights(tree.root_id(), None)
                        .expect("Failed to calculate heights");
                    black_box(heights);
                },
            );
        });
    }

    group.finish();
}

/// Benchmark repeated height calculations
pub fn bench_height_calculation_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("repeated_height_calculations");
    group.sample_size(20); // Reduce sample size for repeated calculations

    for tree_size in [50, 100] {
        group.bench_with_input(
            BenchmarkId::new("tree_size", tree_size),
            &tree_size,
            |b, &size| {
                b.iter_with_setup(
                    || {
                        let tree = setup_tree();
                        let _ = create_linear_chain(&tree, size);
                        tree
                    },
                    |tree| {
                        let backend = tree.backend();
                        let in_memory = backend
                            .as_any()
                            .downcast_ref::<InMemory>()
                            .expect("Failed to downcast database");

                        // Simulate 5 operations that need height info
                        for _ in 0..5 {
                            let heights = in_memory
                                .calculate_heights(tree.root_id(), None)
                                .expect("Failed to calculate heights");
                            black_box(heights);
                        }
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark tips finding performance
pub fn bench_tips_finding(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_tips");

    for num_tips in [5, 10, 25] {
        group.bench_with_input(
            BenchmarkId::new("num_tips", num_tips),
            &num_tips,
            |b, &num| {
                b.iter_with_setup(
                    || {
                        let tree = setup_tree();
                        let _ = create_branching_tree(&tree, num, 3); // Create branches as tips
                        tree
                    },
                    |tree| {
                        let backend = tree.backend();
                        let tips = backend
                            .get_tips(tree.root_id())
                            .expect("Failed to get tips");
                        black_box(tips);
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark tree traversal scalability
pub fn bench_tree_traversal_scalability(c: &mut Criterion) {
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
                            let tree = setup_tree();
                            let _ = create_large_tree(&tree, size, structure);
                            tree
                        },
                        |tree| {
                            let backend = tree.backend();
                            let entries = backend
                                .get_tree(tree.root_id())
                                .expect("Failed to get tree");
                            black_box(entries);
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
    let mut group = c.benchmark_group("crdt_computation");

    for tree_depth in [10, 20] {
        group.bench_with_input(
            BenchmarkId::new("depth", tree_depth),
            &tree_depth,
            |b, &depth| {
                b.iter_with_setup(
                    || {
                        let tree = setup_tree();
                        let entry_ids = create_linear_chain(&tree, depth);
                        let tip_entry = entry_ids.last().unwrap().clone();
                        (tree, tip_entry)
                    },
                    |(tree, tip_entry)| {
                        let op = tree
                            .new_operation_with_tips([tip_entry])
                            .expect("Failed to create op");
                        let kv = op
                            .get_subtree::<KVStore>("data")
                            .expect("Failed to get KVStore");

                        // Perform a simple operation that requires CRDT computation
                        kv.set("test_key", "test_value")
                            .expect("Failed to set value");
                        let result = op.commit().expect("Failed to commit");
                        black_box(result);
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark individual tip validation
pub fn bench_tip_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_tip");

    for tree_size in [50, 100] {
        group.bench_with_input(
            BenchmarkId::new("tree_size", tree_size),
            &tree_size,
            |b, &size| {
                b.iter_with_setup(
                    || {
                        let tree = setup_tree();
                        let entry_ids = create_linear_chain(&tree, size);
                        let last_entry_id = entry_ids.last().unwrap().clone();
                        (tree, last_entry_id)
                    },
                    |(tree, last_entry_id)| {
                        let backend = tree.backend();
                        let in_memory = backend
                            .as_any()
                            .downcast_ref::<InMemory>()
                            .expect("Failed to downcast database");

                        let is_tip = in_memory.is_tip(tree.root_id(), &last_entry_id);
                        black_box(is_tip);
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group! {
    name = backend_benches;
    config = Criterion::default().sample_size(30);
    targets = bench_lca_linear_chains, bench_lca_diamond_merge, bench_tree_heights,
              bench_height_calculation_overhead, bench_tips_finding, bench_tree_traversal_scalability,
              bench_crdt_merge_operations, bench_tip_validation
}

criterion_main!(backend_benches);
