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
just test                              # Run all tests with nextest
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

## Backend Matrix Testing

The test suite runs against multiple storage backends via the `TEST_BACKEND` environment variable. The `test_backend()` factory in `helpers.rs` creates the appropriate backend for each test:

| Value      | Backend               | Notes                            |
| ---------- | --------------------- | -------------------------------- |
| (unset)    | InMemory              | Default, fastest                 |
| `inmemory` | InMemory              | Explicit default                 |
| `sqlite`   | SQLite (in-memory)    | Requires `sqlite` feature        |
| `postgres` | PostgreSQL            | Requires `postgres` feature      |
| `service`  | RemoteBackend via RPC | Requires `service` feature, unix |

The `service` backend starts a fresh in-process daemon with an InMemory backend for each `test_backend()` call, routing all operations through the Unix socket RPC layer. This maintains the same isolation semantics as other backends. The full integration suite passes 1:1 against `TEST_BACKEND=service`; see the [Service Architecture § Testing](./service.md#testing) chapter for the local/wire test-helper split and the rationale for routing subsystem tests (sync internals, raw-backend listings, low-level delegation resolution) through always-local helpers regardless of `TEST_BACKEND`.

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
expiry. Published-view expiry tests use a short view lifetime and keep staging
at its normal five-minute idle lifetime, so a scheduling pause during upload
cannot expire a private build before the view assertions. A separate test checks
that an idle build still publishes while its published view expires. Production
uses the same five-minute idle lifetime for both.
The service Table test asserts that a first point read publishes individual row
records rather than falling back to a whole-`Doc` response.

## Writing Tests

1. Add tests to appropriate module in `tests/it/`
2. Test both happy path and error cases
3. Use helpers from `tests/it/helpers.rs`
4. Follow `test_<component>_<functionality>` naming

## Local-network Iroh sync

`test_iroh_mdns_same_host_sync` bootstraps a signed database between two Instances using an endpoint-ID-only ticket, with relays and public address lookup disabled.
It requires multicast, so the sandboxed backend matrix skips it; `nix build .#integration.mdns` runs it explicitly in a LAN-only NixOS VM with no public DNS.
The VM test is part of `just nix full`.
To run it on a multicast-capable host:

```bash
cargo test -p eidetica --all-features --test it test_iroh_mdns_same_host_sync -- --ignored
```

## Ticket-supplied Iroh dial targets

The ignored `test_iroh_ticket_target_*` tests observe the production
`IrohTransport::send_request` ticket decoding and dial path with local listeners.
They characterize attempted dialing, not successful QUIC/relay connections,
database access, or an outbound-target policy.
A malformed-ticket control must produce no UDP traffic; valid tickets must reach
UDP listeners at loopback and private IPv4 literals, and a TCP listener for an
HTTPS relay hostname resolving to a private IPv4 address.
Relays retain the inherited configuration; public address lookup, mDNS, and port
mapping are disabled to remove unrelated discovery traffic.
No production configuration or authorization checks are changed.

On Linux with unprivileged user/network/mount namespaces enabled, build outside
the namespace, then run the cached test runner in a namespace with only loopback
and a private address assigned to it.
The hosts mapping is bind-mounted only inside that mount namespace; no host
configuration or external network is touched.
The tests refuse non-loopback IPv4 routes and fail before dialing if the private
fixture address or hostname mapping is missing.

```bash
# Enter the dev shell (the archive runner needs cargo on PATH).
nix develop
# Build without running the ignored tests on the host network.
nix run .#test-inmemory -- --no-tests pass -E 'test(test_iroh_ticket_target)'
runner=$(nix eval --raw .#apps.x86_64-linux.test-inmemory.program)
hosts=$(mktemp)
printf '127.0.0.1 localhost\n10.77.0.1 ticket-relay.test\n' > "$hosts"
# Remove proxy settings so relay dialing cannot be diverted through a host proxy.
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u http_proxy -u https_proxy -u all_proxy \
  unshare --user --map-root-user --net --mount --propagation private \
  bash -eu -c '
    ip link set lo up
    ip addr add 10.77.0.1/32 dev lo
    mount --bind "$1" /etc/hosts
    ip address show
    ip route show
    # Hide host runtime sockets, including the host-side nscd DNS cache.
    mount -t tmpfs tmpfs /run
    exec "$2" --run-ignored only -E "test(test_iroh_ticket_target)" --success-output immediate
  ' bash "$hosts" "$runner"
rm "$hosts"
```

Both tests have bounded observation windows.
A received datagram or accepted TCP connection proves an attempt before any
application authorization; it does not prove an arbitrary service can be read,
that a relay TLS handshake succeeds, or that any database permissions were bypassed.
These are characterization assertions for the current dependency behavior, not a
requirement to keep allowing such targets if a later policy is adopted.
The normal sandboxed test matrix skips them.

## Multi-instance sync harness

Multi-peer convergence tests build on `eidetica::testing::Cluster` — a harness
for standing up several `Instance`s that sync a shared database, with a
controllable transport and seeded fault-injecting fuzzers. It has its own page:
see [Multi-Instance Test Harness](test_harness.md).
