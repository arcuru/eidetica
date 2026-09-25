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
just test                              # Run workspace tests on SQLite with nextest
cargo test --all-features --test it    # Run integration tests
cargo test --all-features auth::       # Run specific module tests
```

The `it` target declares `required-features = ["testing"]`, because it uses
internal hooks — `FixedClock`, `Instance::*_with_clock`, the `testing` module —
that only exist under that feature. Without it cargo skips the target rather than
failing to compile it, so pass `--all-features` (or `--features testing`) when
invoking cargo directly. `just test` and the Nix test packages already do.

The `testing` feature is not in `default` or `full` and must never be taken as a
normal dependency by a workspace member: that would compile the hooks into every
release binary built with `--workspace`. The `release-features` lint fails the
build if `eidetica-bin`'s feature graph ever enables it.

Buck's `just buck test` is a separate native shortcut covering unit tests, integration tests, and library and book doctests. Unlike `just test`, it uses the InMemory backend by default; see [Buck2 build](buck2.md#build-and-test-coverage) for its exact targets and limits.

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

Backend conformance tests cover Store-state record-set separation, private
build invisibility, atomic publication failure, concurrent publication of one
target, derived immutability, half-open binary-key scans, empty and exclusive page
continuation, and lifecycle-safe clearing.
Table cached-state tests instrument local record reads to prove zero reads while
loading a handle, one point lookup for `get`, bounded ordered pages, and
transaction-local put/delete overlays. They also assert that a cold record set
contains one backend record per row while historical Entry deltas remain `Doc`.
Store-state records tests cover a derived clear during an active reader and the
rebuild that follows it, plus the reclaim of the unlinked generation.
Store-state service tests cover publication, reconnect durability,
publication immutability, session scope binding with a refused foreign scope,
shared-cache fallback, byte-verbatim ciphertext, and idle private-build
expiry. The service Table test asserts that a first point read publishes
individual row records rather than falling back to a whole-`Doc` response.

## Writing Tests

1. Add tests to appropriate module in `tests/it/`
2. Test both happy path and error cases
3. Use helpers from `tests/it/helpers.rs`
4. Follow `test_<component>_<functionality>` naming

## Multi-instance sync harness

Multi-peer convergence tests build on `eidetica::testing::Cluster` — a harness
for standing up several `Instance`s that sync a shared database, with a
controllable transport and seeded fault-injecting fuzzers. It has its own page:
see [Multi-Instance Test Harness](test_harness.md).
