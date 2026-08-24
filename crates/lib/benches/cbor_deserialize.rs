//! CBOR deserialization micro-benchmark.
//!
//! Measures `serde_ipld_dagcbor::from_slice::<Entry>` cost in isolation
//! (no SQLite, no async, no sqlx) so the 27µs per-entry read cost from
//! the pyramid benchmark can be decomposed into:
//!   SQLite lookup + CBOR deserialize + sqlx async overhead.
//!
//! Tests four entry shapes:
//!   - Minimal: root entry, no parents, no subtrees, default AuthInfo
//!   - Small: root + 1 parent + 1 small subtree
//!   - Representative: chaz shape — root + 1 parent + subtree data + delegation AuthInfo + height
//!   - Large: root + 5 parents + 3 subtrees × 1KB each + delegation AuthInfo

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use eidetica::auth::types::{AuthInfo, DelegationStep, KeyHint, SigKey};
use eidetica::{Entry, ID, PrivateKey};

fn bench_cbor_deserialize(c: &mut Criterion) {
    // Shared key for delegation AuthInfo
    let private_key = PrivateKey::generate();
    let pubkey = private_key.public_key();

    // ── delegation AuthInfo (matches chaz entry shape) ──────────────
    let delegation_sig = AuthInfo {
        signature: None,
        key: SigKey::Delegation {
            path: vec![DelegationStep {
                tree: ID::from_bytes("delegated_tree_root_id_here!!"),
                tips: vec![ID::from_bytes("tip_entry_1_32_bytes_here!!!")],
            }],
            hint: KeyHint::from_pubkey(&pubkey),
        },
    };

    // ── 1. Minimal entry ───────────────────────────────────────────
    // Root entries have no root field, no parents, and carry only the
    // internal _root marker subtree + entropy metadata.  This is the
    // smallest valid entry the builder can produce.
    let minimal = Entry::root_builder().build().unwrap();
    let minimal_bytes = serde_ipld_dagcbor::to_vec(&minimal).unwrap();
    println!("Minimal entry CBOR size: {} bytes", minimal_bytes.len());

    // ── 2. Small entry ─────────────────────────────────────────────
    let small = Entry::builder(ID::from_bytes("some_32_byte_root_id_here!!"))
        .add_parent(ID::from_bytes("parent_32_byte_id_here_here!!"))
        .set_subtree_data("messages", b"hello")
        .build()
        .unwrap();
    let small_bytes = serde_ipld_dagcbor::to_vec(&small).unwrap();
    println!("Small entry CBOR size: {} bytes", small_bytes.len());

    // ── 3. Representative entry (chaz shape) ───────────────────────
    let representative = Entry::builder(ID::from_bytes("some_32_byte_root_id_here!!"))
        .add_parent(ID::from_bytes("parent_32_byte_id_here_here!!"))
        .set_subtree_data(
            "messages",
            b"hello world this is a chat message with some content",
        )
        .set_auth(delegation_sig.clone())
        .set_height(42)
        .build()
        .unwrap();
    let rep_bytes = serde_ipld_dagcbor::to_vec(&representative).unwrap();
    println!("Representative entry CBOR size: {} bytes", rep_bytes.len());

    // ── 4. Large entry ─────────────────────────────────────────────
    let kb_data = vec![b'x'; 1024];
    let large = Entry::builder(ID::from_bytes("some_32_byte_root_id_here!!"))
        .add_parent(ID::from_bytes("parent_1_32_byte_id_here_here!"))
        .add_parent(ID::from_bytes("parent_2_32_byte_id_here_here!"))
        .add_parent(ID::from_bytes("parent_3_32_byte_id_here_here!"))
        .add_parent(ID::from_bytes("parent_4_32_byte_id_here_here!"))
        .add_parent(ID::from_bytes("parent_5_32_byte_id_here_here!"))
        .set_subtree_data("data_a", kb_data.clone())
        .set_subtree_data("data_b", kb_data.clone())
        .set_subtree_data("data_c", kb_data)
        .set_auth(delegation_sig)
        .set_height(100)
        .build()
        .unwrap();
    let large_bytes = serde_ipld_dagcbor::to_vec(&large).unwrap();
    println!("Large entry CBOR size: {} bytes", large_bytes.len());

    // ── Benchmarks ─────────────────────────────────────────────────
    let mut group = c.benchmark_group("cbor_deserialize");

    group.bench_function("minimal", |b| {
        b.iter(|| serde_ipld_dagcbor::from_slice::<Entry>(black_box(&minimal_bytes)).unwrap());
    });

    group.bench_function("small", |b| {
        b.iter(|| serde_ipld_dagcbor::from_slice::<Entry>(black_box(&small_bytes)).unwrap());
    });

    group.bench_function("representative", |b| {
        b.iter(|| serde_ipld_dagcbor::from_slice::<Entry>(black_box(&rep_bytes)).unwrap());
    });

    group.bench_function("large", |b| {
        b.iter(|| serde_ipld_dagcbor::from_slice::<Entry>(black_box(&large_bytes)).unwrap());
    });

    group.finish();
}

criterion_group!(benches, bench_cbor_deserialize);
criterion_main!(benches);
