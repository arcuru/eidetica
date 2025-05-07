/*! Integration tests for EideticaDB.
 *
 * This test suite is organized as a single integration test binary
 * following the pattern described by matklad in
 * https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html
 *
 * The module structure mirrors the main library structure:
 * - atomicop: Tests for the AtomicOp struct and its interaction with EntryBuilder
 * - basedb: Tests for the BaseDB struct and related functionality
 * - backend: Tests for the Backend trait and implementations
 * - data: Tests for the CRDT trait and implementations (e.g., KVOverWrite)
 * - entry: Tests for the Entry struct and related functionality
 * - tree: Tests for the Tree struct and related functionality
 */

mod atomicop;
mod backend;
mod basedb;
mod data;
mod entry;
mod subtree;
mod tree;
