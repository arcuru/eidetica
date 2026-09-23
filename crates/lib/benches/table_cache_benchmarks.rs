//! Benchmarks for Table store cache performance
//!
//! These benchmarks measure cold and warm Table reads and the cost of state
//! computation when tips change.

mod helpers;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use eidetica::{
    Instance,
    crdt::Doc,
    store::{PasswordStore, Table},
};
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
/// 2. Read once to resolve the current generation
///
/// Timed: Adding a new transaction and computing the final state
///
/// A changed tip requires a new immutable generation; this is not a delta-only
/// rebuild. Compare with cold_cache to distinguish initial and changed-tip cost.
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
/// The generation is already published; history reconstruction is excluded.
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

/// Benchmarks cold cache (no published derived state) initial population.
///
/// Setup (not timed): Create Table with N records and clear derived state
/// Timed: First `table.get()` must compute state from scratch and populate cache
///
/// This measures the first read's historical rebuild rather than a warm view.
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
                        // Writes may have created a derived generation. Explicitly
                        // unlink it before measuring the first read.
                        let (instance, db, keys) = setup_table_with_history(&rt, history_size);
                        rt.block_on(db.backend().unwrap().clear_derived_store_state())
                            .unwrap();
                        (instance, db, keys)
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

/// Benchmarks one point read including cold historical materialization.
///
/// Setup creates a large Table in one commit, then clears any derived state
/// created during writes. The timed operation opens a new handle and reads one
/// row, including the first record generation build.
fn bench_cold_large_table_point_read(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");

    let mut group = c.benchmark_group("table_cold_point_read");
    for &row_count in &[1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("single_row", row_count),
            &row_count,
            |b, &row_count| {
                b.iter_with_setup(
                    || {
                        let (_instance, _user, db) = rt.block_on(setup_tree_async());
                        let keys = rt.block_on(async {
                            let tx = db.new_transaction().await.unwrap();
                            let table = tx
                                .get_store::<Table<BenchRecord>>("bench_table")
                                .await
                                .unwrap();
                            let mut keys = Vec::with_capacity(row_count);
                            for id in 0..row_count {
                                let key = format!("row-{id:08}");
                                table
                                    .set(
                                        &key,
                                        BenchRecord {
                                            id,
                                            name: format!("record_{id}"),
                                            value: id as i64 * 100,
                                        },
                                    )
                                    .await
                                    .unwrap();
                                keys.push(key);
                            }
                            tx.commit().await.unwrap();

                            // Writing may materialize a generation while checking row state.
                            // A fresh viewer alone is not a cold cache.
                            db.backend()
                                .unwrap()
                                .clear_derived_store_state()
                                .await
                                .unwrap();
                            keys
                        });
                        (_instance, db, keys)
                    },
                    |(_instance, db, keys)| {
                        rt.block_on(async {
                            let table = db
                                .get_store_viewer::<Table<BenchRecord>>("bench_table")
                                .await
                                .unwrap();
                            let row = table.get(&keys[row_count / 2]).await.unwrap();
                            black_box(row);
                        });
                    },
                );
            },
        );
    }
    group.finish();
}

/// Report actual stored subtree payload sizes, excluding Entry framing/signatures.
/// The same rows and exact keys are used on both Table formats.
fn bench_table_payload(_c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    for &rows in &[1, 32] {
        let (_instance, _user, db) = rt.block_on(setup_tree_async());
        let bytes = rt.block_on(async {
            let tx = db.new_transaction().await.unwrap();
            let table = tx
                .get_store::<Table<BenchRecord>>("bench_table")
                .await
                .unwrap();
            for id in 0..rows {
                table
                    .set(
                        format!("row-{id:08}"),
                        BenchRecord {
                            id,
                            name: format!("record_{id}"),
                            value: id as i64 * 100,
                        },
                    )
                    .await
                    .unwrap();
            }
            let entry_id = tx.commit().await.unwrap();
            db.backend()
                .unwrap()
                .get(&entry_id)
                .await
                .unwrap()
                .data("bench_table")
                .unwrap()
                .len()
        });
        eprintln!("table_payload_bytes rows={rows} subtree_bytes={bytes}");
        black_box(bytes);
    }
}

/// One fresh database per sample: commit a single row or a batch, including
/// Entry construction and storage. Setup cost is outside the measured closure.
fn bench_table_writes(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("table_write");
    for &rows in &[1, 32] {
        group.throughput(Throughput::Elements(rows));
        group.bench_with_input(BenchmarkId::new("commit", rows), &rows, |b, &rows| {
            b.iter_with_setup(
                || rt.block_on(setup_tree_async()),
                |(_instance, _user, db)| {
                    rt.block_on(async {
                        let tx = db.new_transaction().await.unwrap();
                        let table = tx
                            .get_store::<Table<BenchRecord>>("bench_table")
                            .await
                            .unwrap();
                        for id in 0..rows as usize {
                            table
                                .set(
                                    format!("row-{id:08}"),
                                    BenchRecord {
                                        id,
                                        name: format!("record_{id}"),
                                        value: id as i64 * 100,
                                    },
                                )
                                .await
                                .unwrap();
                        }
                        black_box(tx.commit().await.unwrap());
                    });
                },
            );
        });
    }
    group.finish();
}

/// Scan a published generation, not the history reconstruction phase.
fn bench_table_pages(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_instance, db, keys) = setup_table_with_history(&rt, 100);
    rt.block_on(async {
        let table = db
            .get_store_viewer::<Table<BenchRecord>>("bench_table")
            .await
            .unwrap();
        table.get(&keys[0]).await.unwrap();
    });
    let mut group = c.benchmark_group("table_page");
    for &limit in &[10, 50] {
        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("scan_100", limit), &limit, |b, &limit| {
            b.iter(|| {
                rt.block_on(async {
                    let table = db
                        .get_store_viewer::<Table<BenchRecord>>("bench_table")
                        .await
                        .unwrap();
                    let mut cursor = None;
                    let mut count = 0;
                    loop {
                        let page = table.scan_page(cursor.as_ref(), limit).await.unwrap();
                        count += page.rows.len();
                        cursor = page.next;
                        if cursor.is_none() {
                            break;
                        }
                    }
                    assert_eq!(count, 100);
                    black_box(count);
                });
            });
        });
    }
    group.finish();
}

/// Include password derivation and authenticated physical scans in both legs.
/// Each sample uses a fresh encrypted Store; setup is excluded from timing.
fn bench_encrypted_pages(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("table_encrypted_page");
    for &encrypted_mode in &[false, true] {
        group.bench_with_input(
            BenchmarkId::new("scan_32", encrypted_mode),
            &encrypted_mode,
            |b, &encrypted_mode| {
                b.iter_with_setup(
                    || {
                        let (instance, user, db) = rt.block_on(setup_tree_async());
                        rt.block_on(async {
                            let tx = db.new_transaction().await.unwrap();
                            if encrypted_mode {
                                let mut wrapped = tx
                                    .get_store::<PasswordStore<Table<BenchRecord>>>("bench_table")
                                    .await
                                    .unwrap();
                                wrapped
                                    .initialize("bench-password", Doc::new())
                                    .await
                                    .unwrap();
                                let table = wrapped.inner().await.unwrap();
                                for id in 0..32 {
                                    table
                                        .set(
                                            format!("row-{id:08}"),
                                            BenchRecord {
                                                id,
                                                name: format!("record_{id}"),
                                                value: id as i64 * 100,
                                            },
                                        )
                                        .await
                                        .unwrap();
                                }
                            } else {
                                let table = tx
                                    .get_store::<Table<BenchRecord>>("bench_table")
                                    .await
                                    .unwrap();
                                for id in 0..32 {
                                    table
                                        .set(
                                            format!("row-{id:08}"),
                                            BenchRecord {
                                                id,
                                                name: format!("record_{id}"),
                                                value: id as i64 * 100,
                                            },
                                        )
                                        .await
                                        .unwrap();
                                }
                            }
                            tx.commit().await.unwrap();
                        });
                        (instance, user, db)
                    },
                    |(_instance, _user, db)| {
                        rt.block_on(async {
                            let tx = db.new_transaction().await.unwrap();
                            let count = if encrypted_mode {
                                let mut wrapped = tx
                                    .get_store::<PasswordStore<Table<BenchRecord>>>("bench_table")
                                    .await
                                    .unwrap();
                                wrapped.open("bench-password").unwrap();
                                let table = wrapped.inner().await.unwrap();
                                table.scan_page(None, 32).await.unwrap().rows.len()
                            } else {
                                let table = tx
                                    .get_store::<Table<BenchRecord>>("bench_table")
                                    .await
                                    .unwrap();
                                table.scan_page(None, 32).await.unwrap().rows.len()
                            };
                            assert_eq!(count, 32);
                            black_box(count);
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
        bench_cold_large_table_point_read,
        bench_table_payload,
        bench_table_writes,
        bench_table_pages,
        bench_encrypted_pages,
}
criterion_main!(table_cache_benches);
