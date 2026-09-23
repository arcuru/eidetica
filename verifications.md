# Table redesign verification ledger

## Phase 0 / Phase 1: canonical row and algebra, before Table switch

Intended: RFC 8785 canonical bytes, duplicate/member/number validation, JSON
and binary Entry roundtrips, ordered register/map associativity, operation and
serialization goldens. Existing Table stays Doc-backed. The remaining Phase 0
contracts (token sequencing, leases, projection, transaction revision, stale
cursor, typed state and maintenance permission) have **not** been implemented.

Performed (in the Nix development shell):

- `cargo test -p eidetica --all-features --lib crdt::canonical_json::tests`:
  5 passed; 0 failed; 473 filtered out.
- `cargo test -p eidetica --all-features --lib crdt::`:
  51 passed; 0 failed; 428 filtered out (final slice).
- `cargo test -p eidetica --all-features --doc crdt::`:
  31 passed; 0 failed; 103 filtered out.
- A binary roundtrip regression test first failed because `RawValue` became a
  JSON object containing an escaped row under DAG-CBOR. Binary serialization
  now uses validated canonical bytes; the test passes.

- A projection-law negative control (temporarily ignoring Delete in the reference
  applicator) failed at the sequential/collapsed equality assertion, exit 101;
  restored reference passed. No production projection is implemented yet.
- `nix develop -c nix run .#fix` succeeded. Final `nix develop -c just nix full`
  succeeded with Nix backend runner summaries: in-memory 1497/1497 (1 leaky),
  SQLite 1497/1497 (3 leaky), service 1497/1497, PostgreSQL 1497/1497,
  minimal 1356/1356; 5 skipped in each. The first full run had one
  PostgreSQL ownership-release timing failure (1495/1496); pristine upstream
  PostgreSQL passed, a subsequent unchanged PostgreSQL run passed, and the
  final full gate passed. The gate exercised the existing Doc-backed Table,
  password store, and service paths, **not** the unimplemented redesigned Table.

Newly required before a complete redesign: backend-neutral conformance tests
for ordered staging and durable token status; concurrent transaction staging
and stale-cursor race tests; generic typed state and permission/fallback tests;
Table/encrypted/service integration matrix; full Nix gate and actual measured
benchmarks. Do not interpret this ledger as verification of those paths.

## Phase 0 continuation: service ordering and backend conformance slice

Intended: token-wide next-sequence and exact wire-chunk digest enforcement at
the service boundary, remote Backend sequencing across separate stage calls,
publication retry on the same live connection, and backend-neutral assertions
for cancellation and abort/publish visibility. This is **not** durable status,
lease renewal, or complete Phase 0; the legacy Table stays Doc-backed.

Performed: the service sequence test failed on a future sequence before the
fix (1 failed / 0 passed); the service publication retry test failed with
InvalidStoreStateStagingToken before the fix (1 failed / 0 passed). Both now
exercise the live socket and pass. `nix develop -c nix run .#fix` succeeded;
`nix develop -c just nix full` after the final code change succeeded, with
nextest summaries: in-memory 1503 passed, 5 skipped; SQLite 1503 passed,
5 skipped (2 leaky); PostgreSQL 1503 passed, 5 skipped; service 1503 passed,
5 skipped (6 leaky); minimal 1359 passed, 5 skipped. The service gate exercises the
existing Doc-backed Table and the new staging-token tests, not redesigned Table.

Newly required: durable server/backend token status (including adopted,
aborted, expired), lease renewal/reclamation and restart survival; digest and
sequence persisted with the target in all backends rather than only an active
socket; client retry of identical encoded chunks after ambiguous transport
failure; true backend-neutral ordered put/delete conformance once explicit
mutations exist; transaction-state revision, cursor, typed state/capability,
and full remaining Phase 0 contracts before Phase 2 or any Table switch.

## Phase 0 continuation: ordered physical mutation slice

Intended: add an explicit ordered `Put`/`Delete` chunk path across the backend
trait, in-memory, SQLite, PostgreSQL and authenticated service RPC without
switching the Doc-backed Table. Delete removes the private row; put after delete
resurrects it; empty generations publish. Preserve the existing collapsed
Doc-overlay tombstone path, including its publish-time null rejection.

Performed: backend-neutral conformance fixture sends put/delete/put across
sequences, retries the immediate accepted sequence, rejects older/gapped and
conflicting chunks, publishes a physically empty namespace after a same-chunk
put/delete, and resolves a never-written generation. A live authenticated
service-socket fixture exercises the ordered RPC and empty publication. Negative
control disabling the in-memory physical removal failed with 0 passed / 1
failed at the empty scan assertion (exit 101), then the restored test passed.
Focused `TEST_BACKEND=sqlite` fixture passed 1/1; live socket fixture passed
1/1. `nix develop -c nix run .#fix` succeeded. Final formatted-source `nix develop -c just nix full` succeeded: in-memory,
SQLite, PostgreSQL and service each 1514 passed / 5 skipped; minimal 1365
passed / 5 skipped; NixOS service and OCI container integration passed. A
committed-tip `nix develop -c just nix full` also exited 0. Its Nix
`test-all` derivation reused the verified backend outputs; `nix log` for each
input confirmed the ordered physical fixture PASS on in-memory, SQLite,
PostgreSQL and service (and minimal), the socket fixture PASS on all four
full-feature runners, and the same nextest summaries: 1514/1514 with 5 skipped
for each full-feature runner, 1365/1365 with 5 skipped for minimal.

Newly required: exact encoded-chunk replay after ambiguous transport (adapter
still lacks it), bounded terminal-token retention and unknown-token safety,
revision-checked transaction staging, stale cursors, typed Store-state and
permission/fallback fixtures, before Phase 2 or the Table switch. Existing
`table:v0` and Doc Table behavior remain unchanged.

## Phase 0 continuation: durable staging-token slice (recovered dirty worktree)

Intended: preserve the previous worker's 15 dirty files and prove that in-memory
snapshots and SQL transactions retain staging outcomes, sequence and last digest;
remote status and publication survive reconnect/restart; five-minute lease plus
five-minute grace reclaim only unpublished orphans. Preserve old custom backend
service access and existing Doc-backed Table. This does **not** complete Phase 0.

Performed: reviewed the dirty diff and ran `git diff --check`. `nix develop -c
cargo test -p eidetica --all-features --test it staging_ -- --nocapture`:
8 passed; 0 failed. Focused `backend::store_state_records::` matrix with explicit
`TEST_BACKEND`: in-memory 22 passed, SQLite 22 passed, service 22 passed; bare
PostgreSQL 2 passed / 20 failed because the direct command lacked the test
container credentials (`password authentication failed for user "ava"`), **not**
a code verdict. The hermetic PostgreSQL Nix test instead passed below.

A real socket test of a recordless custom backend exposed that running the new
reclamation hook before _every_ request denied login. Negative control with
that call intact: 0 passed / 1 failed, `StoreStateStorageUnsupported` on
connection; after ignoring only the typed unsupported capability, the
connection succeeds but login still failed because the service error lost the
typed unsupported marker. Mapping that variant across the wire yields 1 passed /
0 failed for the new login test. Both failures were reproduced before the fix.

After `nix develop -c nix run .#fix`, `nix develop -c just nix full` finished
successfully on the dirty source tree (build, lint, doc, checks and VM/container
integration). Nix backend nextest logs: in-memory 1512 passed / 5 skipped;
SQLite 1512 passed / 5 skipped; PostgreSQL 1512 passed / 5 skipped; service
1512 passed / 5 skipped; minimal 1364 passed / 5 skipped. `git diff --check`
clean. The new backend fixtures reach sequence/replay, abort/publish race,
reclamation and persistent SQLite restart; socket fixtures reach reconnect and
service rebind; this is not a redesigned Table or encrypted-row test. Prior
worker exit logs were unavailable in its retained scratch directory, so its
nonzero cause cannot be established from that run.

Newly required: explicit backend staging put/delete/put with physical deletes,
client replay of the _exact_ encoded chunk after an ambiguous response, bounded
terminal-token retention and safe unknown-token replacement, stable service
permissions/status validation across users and databases, transaction revision
atomicity, stale-cursor race and typed state/capability fixtures; then Phases
2-6 with encryption/service parity and benchmarks. Do not switch Table until
all Phase 0 contracts are executable; `table:v0` stays unchanged.

## Phase 0 continuation: retained encoded staging request slice

Intended: retain a complete encoded staging request for an ambiguous transport
response and replay precisely those bytes on a newly authenticated socket; reject
older puts after a later delete and conflicting same-sequence retries. This is a
manual replay API at the connection layer, **not** automatic retry in the high-level
RemoteBackend, durable token pruning, or unknown-after-horizon recovery.

Performed: `nix develop -c cargo test -p eidetica --all-features --test it
test_exact_staging_chunk_retry_after_reconnect_and_delete -- --nocapture`:
1 passed, 0 failed. The real service fixture sends the original encoded put,
tears down the daemon connection, observes a failed send, restarts the daemon
against the same backend, resends the same bytes, sends a delete, and verifies
both the older replay and the conflicting delete-sequence put are refused;
published point read is `None`. Backend-neutral ordered physical staging fixture
still covers earlier put/delete/put and older/conflicting/gapped rejection
across in-memory, SQLite, PostgreSQL and service. `nix develop -c nix run .#fix`
completed. Final dirty-tree `nix develop -c just nix full` succeeded:
in-memory 1515/1515 (1 leaky), SQLite 1515/1515, PostgreSQL 1515/1515,
service 1515/1515 (1 leaky), minimal 1365/1365, five skipped each;
NixOS service and OCI integration tests passed. The live fixture reported PASS
in all four full-feature Nix runners. The existing Table remains Doc-backed;
table:v0 is unchanged.

Negative control: temporarily disabled immediate duplicate acknowledgement in
the in-memory backend; the reconnect fixture failed 0 passed / 1 failed
(exit 101) at the exact replay. Restored the backend source byte-for-byte and
reran the focused fixture: 1 passed / 0 failed. No backend change is retained.

Newly required: automatic retry coordination in the high-level remote adapter
after ambiguous transport (and reconnect ownership), bounded terminal-token
retention with safe unknown-after-horizon replacement, typed state/capability,
revision/cursor races, service authorization/fallback; then later phases and
another complete Nix gate. Do not claim Phase 0 complete or switch Table.
