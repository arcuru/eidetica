//! Benchmarks for Table store cache performance
//!
//! These benchmarks measure the cost of state computation when tips change.
//! When a new commit is added, the tip combination changes and the cached state
//! for that combination doesn't exist yet. This triggers: cache miss → compute
//! merged state from tips → populate cache.
//!
//! The key optimization target is incremental state computation (process only
//! the diff) vs full recomputation (process all entries).

mod helpers;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use eidetica::{Instance, store::Table};
use helpers::setup_tree_async;
use serde::{Deserialize, Serialize};
use std::hint::black_box;
use tokio::runtime::Runtime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BenchRecord {
    id: usize,
    name: String,
    value: i64,
}

/// Creates a Table with the specified number of entries, each in a separate commit.
/// This simulates a Table with significant history that needs cache rebuilding.
/// Returns (Instance, Database, keys) - Instance must be kept alive for Database to work.
async fn setup_table_with_history_async(
    commit_count: usize,
) -> (Instance, eidetica::Database, Vec<String>) {
    let (instance, _user, db) = setup_tree_async().await;
    let mut keys = Vec::with_capacity(commit_count);

    for i in 0..commit_count {
        let tx = db
            .new_transaction()
            .await
            .expect("Failed to start transaction");
        let table = tx
            .get_store::<Table<BenchRecord>>("bench_table")
            .await
            .expect("Failed to get Table");

        let key = table
            .insert(BenchRecord {
                id: i,
                name: format!("record_{i}"),
                value: i as i64 * 100,
            })
            .await
            .expect("Failed to insert");

        keys.push(key);
        tx.commit().await.expect("Failed to commit");
    }

    (instance, db, keys)
}

fn setup_table_with_history(
    rt: &Runtime,
    commit_count: usize,
) -> (Instance, eidetica::Database, Vec<String>) {
    rt.block_on(setup_table_with_history_async(commit_count))
}

/// Benchmarks cache rebuild cost after a single new commit.
///
/// Setup (not timed):
/// 1. Create Table with N records (each in separate commit)
/// 2. Warm the cache by reading once
///
/// Timed: Adding a new transaction and computing the final state
///
/// - Current behavior: Full recomputation processing ALL entries (O(n))
/// - Target behavior: Incremental update processing only DIFF entries (O(delta))
///
/// Comparing this with cold_cache shows the optimization opportunity: cold must
/// always process all N entries, but after a single commit we could process only
/// the delta.
fn bench_cache_rebuild_after_single_commit(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");

    let mut group = c.benchmark_group("table_cache_rebuild");

    // Test with different table sizes to show O(n) vs O(1) difference
    for &history_size in &[10, 50, 100, 200] {
        group.bench_with_input(
            BenchmarkId::new("single_commit_diff", history_size),
            &history_size,
            |b, &history_size| {
                b.iter_with_setup(
                    || {
                        // Setup: Create table with history
                        // _instance must be kept alive for Database to work
                        let (_instance, db, keys) = setup_table_with_history(&rt, history_size);

                        // Warm up the cache by doing an initial read
                        rt.block_on(async {
                            let tx = db.new_transaction().await.unwrap();
                            let table = tx
                                .get_store::<Table<BenchRecord>>("bench_table")
                                .await
                                .unwrap();
                            let _ = table.get(&keys[0]).await;
                        });

                        (_instance, db, keys)
                    },
                    |(_instance, db, keys)| {
                        // Benchmark: Read triggers cache rebuild
                        // _instance kept alive through this closure
                        rt.block_on(async {
                            let tx = db.new_transaction().await.unwrap();
                            let table = tx
                                .get_store::<Table<BenchRecord>>("bench_table")
                                .await
                                .unwrap();
                            table
                                .insert(BenchRecord {
                                    id: history_size,
                                    name: "new_record".to_string(),
                                    value: 999,
                                })
                                .await
                                .unwrap();
                            tx.commit().await.unwrap();
                            let new_table = db
                                .get_store_viewer::<Table<BenchRecord>>("bench_table")
                                .await
                                .unwrap();
                            // This read triggers cache rebuild since tips changed
                            let _ = black_box(new_table.get(&keys[0]).await.unwrap());
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmarks warm cache read performance (cache hit).
///
/// Setup: Create Table with N records, warm cache once
/// Timed: Full read workflow (get store viewer + get record) with warm cache
///
/// This establishes the O(1) baseline. Cache is valid so no recomputation needed.
fn bench_warm_cache_read(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");

    let mut group = c.benchmark_group("table_warm_cache");

    for &history_size in &[10, 50, 100, 200] {
        group.bench_with_input(
            BenchmarkId::new("single_read", history_size),
            &history_size,
            |b, &history_size| {
                // _instance must be kept alive for Database to work
                let (_instance, db, keys) = setup_table_with_history(&rt, history_size);

                // Warm up the cache
                rt.block_on(async {
                    let table = db
                        .get_store_viewer::<Table<BenchRecord>>("bench_table")
                        .await
                        .unwrap();
                    let _ = table.get(&keys[0]).await;
                });

                b.iter(|| {
                    rt.block_on(async {
                        let table = db
                            .get_store_viewer::<Table<BenchRecord>>("bench_table")
                            .await
                            .unwrap();
                        // Cache is valid, just O(1) lookup
                        let _ = black_box(table.get(&keys[0]).await.unwrap());
                    });
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks cold cache (no prior state) initial population.
///
/// Setup (not timed): Create Table with N records (no cache warming)
/// Timed: First `table.get()` must compute state from scratch and populate cache
///
/// This is always O(n) - no prior state exists to build incrementally from.
/// Provides a baseline for the unavoidable cost of initial cache population.
fn bench_cold_cache_rebuild(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");

    let mut group = c.benchmark_group("table_cold_cache");

    for &history_size in &[10, 50, 100, 200] {
        group.bench_with_input(
            BenchmarkId::new("first_read", history_size),
            &history_size,
            |b, &history_size| {
                b.iter_with_setup(
                    || {
                        // Setup: Create table but don't read (cache cold)
                        // Returns (Instance, Database, keys) - Instance kept alive
                        setup_table_with_history(&rt, history_size)
                    },
                    |(_instance, db, keys)| {
                        // Benchmark: First read triggers full cache build
                        // _instance kept alive through this closure
                        rt.block_on(async {
                            let tx = db.new_transaction().await.unwrap();
                            let table = tx
                                .get_store::<Table<BenchRecord>>("bench_table")
                                .await
                                .unwrap();
                            let _ = black_box(table.get(&keys[0]).await.unwrap());
                        });
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group! {
    name = table_cache_benches;
    config = Criterion::default().configure_from_args();
    targets =
        bench_cache_rebuild_after_single_commit,
        bench_warm_cache_read,
        bench_cold_cache_rebuild,
}
criterion_main!(table_cache_benches);
