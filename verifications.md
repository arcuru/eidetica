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
