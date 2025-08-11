/*! Integration tests for Eidetica.
 *
 * This test suite is organized as a single integration test binary
 * following the pattern described by matklad in
 * https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html
 *
 * The module structure mirrors the main library structure:
 * - atomicop: Tests for the AtomicOp struct and its interaction with EntryBuilder
 * - auth: Tests for the authentication system, organized by auth submodules
 * - basedb: Tests for the BaseDB struct and related functionality
 * - backend: Tests for the Backend trait and implementations
 * - crdt: Tests for the CRDT implementations (Map, List, Value types)
 * - data: Tests for the CRDT trait and implementations (e.g., KVOverWrite)
 * - entry: Tests for the Entry struct and related functionality
 * - tree: Tests for the Tree struct and related functionality
 */

mod atomicop;
mod auth;
mod backend;
mod basedb;
mod crdt;
mod data;
mod entry;
mod helpers;
mod subtree;
mod sync;
mod tree;
