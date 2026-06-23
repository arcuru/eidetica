# Multi-Instance Test Harness

Most of Eidetica's hardest guarantees are _multi-instance_ properties:
convergence under reordered or partial sync, causal delivery, no loss of signed
entries, auth-under-merge. They only show up when several `Instance`s exchange
history over a transport — and they are **Eidetica's** guarantees, not a
consumer's. So the controllable plumbing that exercises them lives next to the
sync engine and is exported behind the `testing` cargo feature, where downstream
crates can write _application-level_ scenarios on top without reaching into sync
internals.

This page describes the harness as it exists today and the direction it is built
to grow in: toward continuous, seeded, fault-injecting simulation testing — a
chaos/[DST](#direction-deterministic-simulation-testing) capability in the spirit
of TigerBeetle's VOPR and, longer term, [Antithesis](https://antithesis.com/).

> **Feature gate.** Everything here is behind `feature = "testing"`. The
> integration test binary runs with `--all-features`; a downstream consumer adds
> `eidetica = { …, features = ["testing"] }` as a dev-dependency.

## The owner/consumer split

The harness owns _wiring_, never _policy_:

- **Eidetica owns** the controllable cluster — building peers, serving a tree,
  driving sync, the controllable transport, and the invariant checks.
- **The consumer owns** the scenario and the semantics — what to write, what to
  partition, and what "correct" means at the application layer ("a write here
  converges back there", "two clients race on the same key").

Transport and auth are deliberately left to the test: `add_auth_keys` /
`set_global_auth_key` are policy-neutral tools, and the transport is injectable.
A test whose subject _is_ the transport lifecycle or an auth boundary keeps doing
that setup explicitly rather than converting to the harness.

## `Cluster`

`eidetica::testing::Cluster` stands up several `Instance`s that sync a shared
database. It lives in `src/testing.rs`.

```rust,ignore
let mut net = Cluster::builder().peers(3).build().await?;
// `cluster_shared_database` (tests/it/sync/helpers.rs) bootstraps a
// global-wildcard database onto every peer and returns one handle each.
let (room, dbs) = cluster_shared_database(&mut net, "chat").await?;
```

### Two sync modes

- **Manual** — `Peer::serve` a tree, then `net.exchange(from, to, &tree)` for a
  bidirectional, fully test-ordered sync. `net.converge(&tree)` drives exchange
  across all pairs until the cluster holds one tip set. This is the quiescent
  fixpoint barrier the N-peer and partition-heal tests assert against.
- **Background** — `net.auto_sync(a, b, &tree)` wires the peer relationship both
  ways and tracks the tree on-commit; commits then propagate on their own.
  `net.flush(peer)` is the deterministic barrier — push the queue now instead of
  waiting for the timer.

`auto_sync` is self-contained (idempotent peer registration, dial-back addresses,
per-tree targets, on-commit tracking), so wiring a full mesh is just a loop over
pairs — the substrate the fuzzers below build on.

## Invariants beyond tip equality

`net.converged(&[peers], &tree)` checks that peers agree on a tip set — necessary
but weak: it says nothing about _what_ they agreed on. Two peers can share a tip
set yet differ below it, or converge onto a state that quietly dropped a signed
entry. The `assert_*` methods walk the full entry set behind the tips and panic
with a diagnostic naming the offending peer and entry:

- `assert_no_lost_entries(&peers, &tree)` — the merge converged onto the _union_
  of histories; no peer silently dropped an entry another holds.
- `assert_all_present(&peers, &tree, &ids)` — externally-known entries (e.g. ids
  captured from the test's own writes) survive everywhere.
- `assert_all_signed(peer, &tree)` — no unsigned or malformed-signature entry
  entered a converged state.
- `assert_all_verified(peer, &tree)` — forward-looking: only meaningful once sync
  runs a per-entry verification pass on ingest (see the method's doc comment).

## Controllable transport: `SimTransport`

The default transport is `HttpLoopback` (real HTTP over loopback). For faults
that wired delivery can't model, swap in `SimLoopback`, an in-process
`SyncTransport` over a shared `SimNetwork` — no sockets, deterministic, and
steerable:

```rust,ignore
let fabric = SimNetwork::new();
let mut net = Cluster::builder()
    .peers(2)
    .transport(Arc::new(SimLoopback::new(fabric.clone())))
    .build()
    .await?;
```

`SimNetwork` is the control handle (a cloneable shared fabric) with two fault
families:

- **Partition** — `partition(a, b)` / `heal(a, b)` / `heal_all()` cut and restore
  links. A send across a cut link fails like a connection error, so an auto-sync
  peer's entries stay queued in its retry queue and redeliver on heal. This is the
  _faithful_ model of a dropped message: the sender never gets a false Ack, so
  nothing is lost — it is delayed until the link returns.
- **Store-and-forward delivery** — `set_manual_delivery(true)` captures
  `SendEntries` pushes instead of delivering them inline; the sender gets an
  optimistic `Ack` (it believes it sent; the receiver sees nothing yet) and the
  test drives delivery: `pending()` lists in-flight handles, `deliver(seq)` /
  `deliver_one()` / `deliver_all()` release them in any order, `duplicate(seq)`
  redelivers, `drop_message(seq)` / `drop_all()` discard. Handshake and tree-sync
  stay inline (request/response), so bootstrap and `exchange` are unchanged — only
  auto-sync's fire-and-forget pushes defer.

The two families probe different things and are intentionally distinct.
Store-and-forward owns the _ordering_ of messages the network will definitely
deliver; partition owns _whether and when_ a link carries them at all. A "drop"
under store-and-forward is a synthetic loss the engine never learns about (it
optimistic-Acked) — useful for idempotency/reorder, wrong for modelling a real
outage. A real outage is a partition, which routes through the engine's actual
retry path.

## Simulation fuzzers

The controls above turn convergence into a _property_ rather than a single case.
Each fuzzer drives a dependency-free, seeded xorshift PRNG (shared as `Rng` in
`tests/it/sync/helpers.rs`), so a failing seed replays its exact schedule and
nothing reads the clock or a real RNG.

- **Delivery-order fuzzing** (`sim_schedule_tests.rs`) — over captured pushes,
  randomly interleave writes and deliveries; assert the cluster converges onto one
  complete, signed state regardless of order, and that _redelivery is idempotent_
  (the duplicate count is asserted non-zero so the test can't pass vacuously).
- **Link-fault fuzzing** (`sim_fault_tests.rs`) — over the real retry path,
  randomly `partition`/`heal` links while writing; after the chaos, heal every
  link and drain via retry-queue flushes alone (no reconciling `exchange`), and
  assert the cluster reconverges. Writes-under-fault are asserted non-zero.

Retry-only convergence holds despite cross-peer orphans because a push carries
only its committed entries, not their ancestors — so a peer may receive a child
before its parent. The receiver stores it regardless, and tip reconciliation
completes once the parent arrives. That order-independence (proven by the delivery
fuzzer) is exactly what lets arbitrarily-late, retry-driven delivery still land.

## Where the tests live

`tests/it/sync/`: `n_peer_convergence_tests`, `partition_heal_tests`,
`invariant_assertions_tests`, `sim_delivery_control_tests`, `sim_schedule_tests`,
and `sim_fault_tests`. Shared setup is in `tests/it/sync/helpers.rs`
(`cluster_shared_database`, `cluster_put`, `cluster_get`, and the seeded `Rng`).

## Direction: deterministic simulation testing

The harness is an incremental path toward a standing simulation-testing
investment, not a fixed set of helpers. The target is the model TigerBeetle
demonstrates with its VOPR — a deterministic simulator that runs thousands of
seeded, fault-injected schedules and checks system-level invariants after each
settles — and, longer term, similarity with or integration into
[Antithesis](https://antithesis.com/)-style autonomous, deterministic-hypervisor
testing. The defining properties are the same ones the pieces above are built
around: **determinism** (seeded, clock-free, perfectly reproducible),
**fault injection** (partition, loss, duplication, reorder — and later restart,
clock skew, key rotation under load), and **invariant checking** past mere
agreement.

What exists today is the substrate and the first property fuzzers. The intended
trajectory:

1. **More fault dimensions** — deterministic clock control (pairs with the
   existing `FixedClock`), backend injection and mid-run restart, concurrent
   auth changes (revocation/grant) under load.
2. **A first-class simulator** — promote the per-test driver loops into a reusable
   `Simulation` type that owns N peers + transport + clock and runs seeded
   randomized schedules, so a new scenario is a workload, not a hand-rolled loop.
3. **Stronger invariants on settle** — convergence, _verification monotonicity_
   (an entry that becomes Verified stays Verified), no lost signed entries, and
   auth-under-merge resolving to the documented branch-validity outcome.
4. **Continuous, at scale** — run many seeds per change in CI and let dedicated
   capacity grind schedules continuously, on the premise that spending compute to
   find bugs before release is the right trade.

This is explicitly a post-stable-base investment: the per-increment discipline is
to add a control to the transport/harness, then the property test it unlocks, and
keep the owner/consumer split sharp — Eidetica ships the simulator; consumers
assert their own semantics on top.
