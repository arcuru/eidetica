# Testing

Most tests are in `tests/it/` as a single integration test binary, following the [matklad pattern](https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html). Tests validate behavior through public interfaces only.

Unit tests should only be used when integration tests are not feasible or when testing private implementation details.

## Organization

The module structure in `tests/it/` mirrors `src/`. Each module has:

- `mod.rs` for test declarations
- `helpers.rs` for module-specific utilities
- Common helpers in `tests/it/helpers.rs`

## Running Tests

```bash
just test              # Run all tests with nextest
cargo test --test it   # Run integration tests
cargo test auth::      # Run specific module tests
```

## Backend Matrix Testing

The test suite runs against multiple storage backends via the `TEST_BACKEND` environment variable. The `test_backend()` factory in `helpers.rs` creates the appropriate backend for each test:

| Value      | Backend               | Notes                            |
| ---------- | --------------------- | -------------------------------- |
| (unset)    | InMemory              | Default, fastest                 |
| `inmemory` | InMemory              | Explicit default                 |
| `sqlite`   | SQLite (in-memory)    | Requires `sqlite` feature        |
| `postgres` | PostgreSQL            | Requires `postgres` feature      |
| `service`  | RemoteBackend via RPC | Requires `service` feature, unix |

The `service` backend starts a fresh in-process daemon with an InMemory backend for each `test_backend()` call, routing all operations through the Unix socket RPC layer. This maintains the same isolation semantics as other backends. The full integration suite passes 1:1 against `TEST_BACKEND=service`; see the [Service Architecture § Testing](./service.md#testing) chapter for the local/wire test-helper split and the rationale for routing subsystem tests (sync internals, raw-backend listings, delegation validation) through always-local helpers regardless of `TEST_BACKEND`.

## Writing Tests

1. Add tests to appropriate module in `tests/it/`
2. Test both happy path and error cases
3. Use helpers from `tests/it/helpers.rs`
4. Follow `test_<component>_<functionality>` naming

## Multi-instance sync harness

Multi-peer convergence tests build on `eidetica::testing::Cluster`, a harness for
standing up several `Instance`s that sync a shared database. It lives in
`src/testing.rs` behind the `testing` cargo feature (the integration binary runs
with `--all-features`). The harness owns the wiring boilerplate — building peers,
serving a tree, driving sync — but leaves **transport** and **auth** to the test,
so tests whose subject *is* the transport lifecycle or an auth boundary keep doing
that setup explicitly rather than converting.

```rust,ignore
let mut net = Cluster::builder().peers(3).build().await?;
// `cluster_shared_database` (in tests/it/sync/helpers.rs) bootstraps a
// global-wildcard database onto every peer and returns one handle each.
let (room, dbs) = cluster_shared_database(&mut net, "chat").await?;
```

Two sync modes:

- **Manual** — `Peer::serve` a tree, then `net.exchange(from, to, &tree)` for a
  bidirectional, fully test-ordered sync. `net.converge(&tree)` drives exchange
  across all pairs until the cluster holds one tip set.
- **Background** — `net.auto_sync(a, b, &tree)` wires the peer relationship both
  ways and tracks the tree on-commit; commits then propagate on their own.
  `net.flush(peer)` is the deterministic barrier (push the queue now instead of
  waiting for the timer).

Auth is the test's: `add_auth_keys` / `set_global_auth_key` are policy-neutral
tools; the harness never grants keys.

### Invariants beyond tip equality

`net.converged(&[peers], &tree)` checks that peers agree on a tip set — necessary
but weak (it says nothing about *what* they agreed on). The `assert_*` methods
walk the full entry set behind the tips:

- `assert_no_lost_entries(&peers, &tree)` — the merge converged onto the *union*
  of histories; no peer silently dropped an entry another holds.
- `assert_all_present(&peers, &tree, &ids)` — externally-known entries (e.g. ids
  captured from the test's own writes) survive everywhere.
- `assert_all_signed(peer, &tree)` — no unsigned or malformed-signature entry
  entered a converged state.
- `assert_all_verified(peer, &tree)` — forward-looking: only meaningful once sync
  runs a per-entry verification pass on ingest (see the method's doc comment).

### Controllable transport: `SimTransport`

The default transport is `HttpLoopback` (real HTTP over loopback). For faults that
wired delivery can't model, swap in `SimLoopback`, an in-process `SyncTransport`
over a shared `SimNetwork` — no sockets, deterministic, and steerable:

```rust,ignore
let fabric = SimNetwork::new();
let mut net = Cluster::builder()
    .peers(2)
    .transport(Arc::new(SimLoopback::new(fabric.clone())))
    .build()
    .await?;
```

`SimNetwork` is the control handle (a cloneable shared fabric):

- **Partition** — `partition(a, b)` / `heal(a, b)` / `heal_all()` cut and restore
  links. A send across a cut link fails like a connection error, so an auto-sync
  peer's entries stay queued in the retry queue and redeliver on heal — the
  faithful model of a dropped message (no false Ack, so nothing is lost).
- **Store-and-forward delivery** — `set_manual_delivery(true)` captures
  `SendEntries` pushes instead of delivering them inline; the sender gets an
  optimistic `Ack` (it believes it sent; the receiver sees nothing yet) and the
  test drives delivery: `pending()` lists in-flight handles, `deliver(seq)` /
  `deliver_one()` / `deliver_all()` release them in any order, `duplicate(seq)`
  redelivers, `drop_message(seq)` / `drop_all()` discard. Handshake and tree-sync
  stay inline (request/response), so bootstrap and `exchange` are unchanged — only
  auto-sync's fire-and-forget pushes defer.

This is what makes order-independence and idempotency testable: reorder a fanned-out
push back-to-front, or deliver a message twice, and assert the cluster still
converges onto the same complete, signed state.

### Where the consumer tests live

`tests/it/sync/`: `n_peer_convergence_tests`, `partition_heal_tests`,
`invariant_assertions_tests`, `sim_delivery_control_tests` (reorder + the
delivery controls), `sim_schedule_tests` (seeded random-schedule fuzzers for
order-independence and duplicate-idempotency over captured pushes), and
`sim_fault_tests` (a seeded fuzzer that flaps links with random partition/heal
and asserts the cluster reconverges through retry-queue redelivery alone).
Shared setup is in `tests/it/sync/helpers.rs` (`cluster_shared_database`,
`cluster_put`, `cluster_get`, and the seeded `Rng`).
