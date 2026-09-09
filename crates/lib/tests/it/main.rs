/*! Integration tests for Eidetica.
 *
 * This test suite is organized as a single integration test binary
 * following the pattern described by matklad in
 * https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html
 *
 * The module structure mirrors the main library structure:
 * - transaction: Tests for the Transaction struct and its interaction with EntryBuilder
 * - auth: Tests for the authentication system, organized by auth submodules
 * - instance: Tests for the Instance struct and related functionality
 * - backend: Tests for the Backend trait and implementations
 * - crdt: Tests for the CRDT implementations (Doc, List, Value types)
 * - data: Tests for the CRDT trait and implementations (e.g., KVOverWrite)
 * - entry: Tests for the Entry struct and related functionality
 * - database: Tests for the Database struct and related functionality
 */

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

use tracing_subscriber::EnvFilter;

struct CountingAllocator;

static LIVE_ALLOCATION_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegates allocation to the system allocator with the layout supplied by Rust.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_ALLOCATION_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            LIVE_ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: delegates deallocation to the system allocator with Rust's original layout.
        unsafe { System.dealloc(ptr, layout) };
        LIVE_ALLOCATION_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        LIVE_ALLOCATION_COUNT.fetch_sub(1, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: delegates reallocation to the system allocator with Rust's original layout.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            LIVE_ALLOCATION_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
            LIVE_ALLOCATION_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        new_ptr
    }
}

fn live_allocations() -> (usize, usize) {
    (
        LIVE_ALLOCATION_BYTES.load(Ordering::Relaxed),
        LIVE_ALLOCATION_COUNT.load(Ordering::Relaxed),
    )
}

#[ctor::ctor]
fn init_test_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();
}

mod auth;
mod backend;
mod context;
mod crdt;
mod data;
mod database;
mod entry;
mod helpers;
mod instance;
#[cfg(all(unix, feature = "service"))]
mod service;
mod store;
mod sync;
mod transaction;
mod user;
