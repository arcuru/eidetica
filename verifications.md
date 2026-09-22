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
