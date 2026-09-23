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

## Phase 0 continuation: bounded terminal outcomes and guarded recovery

Intended: retain terminal outcomes for the 24-hour retry window plus five-minute
lease and five-minute reclamation grace, then prune; disallow replacement of an
unknown token before its creation horizon or while a published generation or
unexpired build for its exact target exists. Keep the existing Table untouched.

Performed: new backend-neutral `terminal_horizon_and_unknown_replacement_guards`
fixture checks Aborted, Expired, Adopted and Published outcomes before and after
pruning, retry/idempotent publish, recent-unknown refusal, atomic simultaneous
old-unknown replacement (exactly one winner), and generation/active-build
refusal. Existing publication-vs-sweeper race fixture still passes. A negative
control disabled the in-memory horizon check: the fixture failed 0 passed / 1
failed at recent-unknown refusal; restored source and re-ran. `nix develop -c
nix run .#fix` succeeded; final `nix develop -c just nix full` succeeded:
in-memory, SQLite, PostgreSQL and service each 1516/1516 passed, minimal
1366/1366 passed, five skipped in each; service and OCI VM integrations passed.
The service-mode backend-neutral fixture uses the local in-memory backend,
not a recovery RPC. Unknown-token recovery is backend-only and requires a
caller-authorized target; the remote adapter does not automatically retry or
recover lost responses. Legacy random token IDs cannot qualify for recovery.

Newly required: high-level ambiguous transport/reconnect retry and authorized
service recovery, typed state/capability and fallback, revision/cursor race
contracts, remaining Phase 0, then Phases 2-6 and full Table/encryption/service
behavior checks. Do not switch Table before Phase 0 completion.

## Phase 0 continuation: typed Store-state read and permission slice

Intended: typed public Store-state retrieval instead of Doc-shaped generic JSON;
server registry type and projection validation after canonical Read authorization;
explicit capability refusal for unsupported maintenance and typed ordered history
fallback on a recordless backend. Preserve DocStore JSON convenience and the
Doc-backed Table/table:v0. This is not the read-scoped ensure-generation RPC
and grants no read client a shared staging token.

Performed: `typed_store_state_folds_custom_crdt_without_doc_conversion`
reduces a non-Doc max counter and rejects a DocStore identity mismatch.
Live-socket `store_state_read_rejects_wrong_descriptor_and_unauthorized_reader`
checks positive Doc retrieval, unauthenticated refusal, authenticated descriptor
mismatch, typed Store mismatch, and a second user's canonical Read denial.
Live recordless socket reads committed Doc data via the typed authorized Entry
history fallback. A negative control removed the distinct capability wire
mapping and the recordless test failed 0 passed / 1 failed (exit 101) with an
IO error rather than falling back; mapping restored and focused fixture passed
1 passed / 0 failed. `nix develop -c nix run .#fix` succeeded. Final
`nix develop -c just nix full` passed: in-memory, SQLite, PostgreSQL, service
1518/1518 each, minimal 1367/1367, five skipped each; NixOS service and OCI
container integration VMs passed. The changed tests appear as PASS in the full
backend matrix; this is not proof of the future Table codec or encrypted fallback.

Newly required: read-scoped server ensure-generation under internal maintenance
capability, strict descriptor validation for password-wrapped/unknown codecs,
typed history fallback for encrypted Stores with client-side decryption,
transaction revision atomicity, stale cursor races, authorized service recovery
and automatic high-level ambiguous-response retry; then Phases 2-6 including
Table switch, encryption parity, benchmarks, and full gate again. This service
RPC remains a plaintext whole-state read for known Doc-backed types, not a
shared-generation publication grant. No Table format switch was made.

## Phase 0 continuation: read-scoped known-codec maintenance boundary

Intended: expose a named read-scoped ensure-generation operation for the two
known plaintext Store codecs, keep all staging tokens behind Write, reject
mismatched descriptors before maintenance, and refuse encrypted/unknown codecs
without accepting a client projection. Keep Doc-backed Table/table:v0 unchanged.

Performed: real socket read-only user with a global canonical Read grant reads
Doc state via server maintenance and cannot begin staging; unauthorized user and
unauthenticated socket still cannot read, wrong plaintext Store/descriptor is
rejected, and encrypted Store with plaintext descriptor is rejected while its
known wrapper descriptor returns RecordMaintenanceUnavailable. Negative control:
temporarily requiring Write for ensure-generation made the read-only fixture
fail 0 passed / 1 failed at PermissionDenied; restored source and reran.

Remaining: `_index` does not reveal PasswordStore's wrapped codec (encrypted
metadata), so a matching known wrapper descriptor is a claim, not independently
verified; the server refuses maintenance regardless. The generic fallback cannot
decrypt password-wrapped history, so encrypted typed read needs a separate
client-side decrypting path. Automatic remote retry/recovery, atomic revision
staging, stale cursors and subsequent phases remain; no Table switch.

`nix develop -c nix run .#fix` and `nix develop -c just nix full` succeeded
on the formatted source. Nix nextest: in-memory, SQLite, PostgreSQL and service
each 1519 tests run: 1519 passed, 5 skipped; minimal 1367 tests run: 1367
passed, 5 skipped. Both changed live-socket fixtures reported PASS on all four
full-feature runners. NixOS service and OCI container VM integration passed.

## Phase 0 continuation: revision-atomic typed staging fixture

Intended: introduce an internal transaction-local candidate for typed canonical
Entry bytes and logical/physical overlays at one monotonic revision, without
switching the existing Doc-backed Table. Projection, serialization and record
encryption errors must leave the cell and Entry builder unchanged. An unlocked
snapshot may be used only with a revision check and retry. No await under its
installation lock; this is not yet the Phase 3 read path.

Performed: `nix develop -c cargo test -p eidetica --all-features --lib
projected_staging -- --nocapture`: 2 passed, 0 failed (final focused run on
formatted source). The concurrency fixture uses two OS threads and a three-party
barrier in serialization so both candidates start from the same empty revision;
it checks revision 2, both logical and physical entries, canonical local bytes,
and persisted post-commit history. A temporary removal of the revision guard
failed 0 passed / 1 failed (exit 101), then restored. The failure fixture
injects projection, canonical serde serialization, and physical encryption
failures and compares builder bytes and both overlays before/after. Legacy
`stage_record` refuses a Store already using typed staging; a direct subsequent
subtree overwrite is detected on the next typed stage (guard not directly
covered by this fixture). Registering an encryptor after typed staging is
refused. No Table switch or `table:v0` change.

`nix develop -c nix run .#fix` succeeded. One intermediate full gate failed
solely on treefmt for a newly added assertion after the prior formatter run;
a subsequent fix formatted it, and the **final** `nix develop -c just nix full`
passed on the formatted source: in-memory, SQLite, PostgreSQL, service each
1521 tests run: 1521 passed, 5 skipped; minimal 1369 tests run: 1369 passed,
5 skipped. Both new fixtures reported PASS in each backend runner; NixOS and
OCI integration VMs passed. Focused fixture exercises local in-memory Entry
behavior even inside backend-matrix runners, not remote typed staging.

Newly required: genericize the one-way streamable projection and route typed
read-your-writes through this cell; decide and enforce direct-update exclusivity
when other Store APIs move onto typed staging; retain encrypted key-identity
validation when the projection context evolves. Complete stale cursor/race,
encrypted decrypting fallback, authorized remote recovery and automatic
high-level retry fixtures before Table switches; then Phases 3-6 including
actual Table/encryption/service behavior and benchmarks.

## Phase 0 continuation: typed projection cursor and deterministic page race

Intended: opaque transaction-view cursor with overlay revision, effective projection
context and exclusive physical last key; reject stale continuations after typed
put/delete or a racing mutation while a backend page is awaited. Preserve the
Doc-backed table:v0 scan via an internal legacy cursor variant; do not switch
Table's format or claim typed scans are wired to a backend yet.

Performed: `nix develop -c cargo test -p eidetica --all-features --lib
projected_page -- --nocapture`: 2 passed, 0 failed. The focused tests exercise
physical-order continuation with staged overlay, cross-view and descriptor
rejection, put/delete invalidation and a one-shot-channel-controlled backend
fetch that releases only after a competing stage completes. Negative control
removing both post-fetch and pre-return revision checks: race test failed
0 passed / 1 failed (exit 101); restored source passed 2/2. The first negative
control removing only the immediate post-fetch check stayed green because the
final pre-return check also guards the path; it was not a valid kill. No typed
Table scan is exposed yet; the fetcher boundary is internal and is exercised
with a deterministic in-memory page provider, not a real backend.

Newly required: route the typed projection's persisted RecordView and transaction
read-your-writes into this scanner, validate physical-key shadowing and encrypted
key identity against backend-neutral fixtures, and test real backend page races
before the Table switch. Encrypted client decrypting fallback, authorized remote
recovery/automatic retry, subsequent phases, service/encryption parity and
benchmarks remain.

Final formatted-source gate: `nix develop -c nix run .#fix` succeeded;
`nix develop -c just nix full` exited 0. Nix nextest summary: in-memory,
SQLite, PostgreSQL and service each 1523 tests run: 1523 passed, 5 skipped;
minimal 1371 tests run: 1371 passed, 5 skipped. Both cursor fixtures
reported PASS in each runner; NixOS service and OCI container VM integration
passed. These fixtures use the internal deterministic provider in every runner,
not those runners' remote/backend record scanners. No deployed Table switch.

## Phase 0/3 continuation: typed one-way projection and real record view

Intended: make `RecordProjection<D>` one-way and incrementally consumable, reduce
ordered typed Entry deltas into bounded physical put/delete chunks, and serve typed
point reads and physical-order scan pages from a real immutable backend RecordView
plus revision-checked transaction overlays. Keep Doc-backed Table and its old
entry format intact; recordless reads reduce typed history locally. This slice
does not switch Table or complete Phase 0.

Performed: `projected_streaming_history_applies_deletes_across_chunks` writes
140 rows, deletes across a chunk boundary, rebuilds a real in-memory generation,
checks absent physical keys, and resurrects a row. A negative control dropping
ordered Deletes failed 0 passed / 1 failed at the committed deleted row;
restored. `projected_real_record_view_physical_scan_and_overlay` checks cold point
materialization, a real backend record get and ordered scan merged with local
delete/put while the published view remains unchanged. The deterministic
`projected_real_backend_fetch_rejects_racing_overlay` gate releases a real backend
scan only after the overlay changes and expects StaleCursor. The recordless
backend-seam fixture disables record methods while forwarding Entry operations;
point reads and paged scans reduce typed history and merge a local put.
These new unit fixtures use a real in-memory engine even when the Nix runner's
`TEST_BACKEND` is SQLite, PostgreSQL or service. Existing integration tests
cover those backend paths for the legacy Table, not this typed Store API.

Final formatted source: `nix develop -c nix run .#fix` succeeded; `nix develop
-c just nix full` passed: in-memory, SQLite, PostgreSQL and service each
1527 tests run / 1527 passed / 5 skipped; minimal 1375 run / 1375 passed /
5 skipped. NixOS service and OCI container VM integrations passed. The new
real-view, race, recordless and chunk fixtures reported PASS in the runners,
but run against their in-memory fixture. Existing Doc-backed Table and
`table:v0` remained unchanged.

Newly required: backend-neutral typed projection fixtures using actual SQLite,
PostgreSQL and remote socket backends; encrypted physical-key identity and
client-side decrypting fallback checks; automatic authorized remote ambiguous
retry/recovery; service maintenance permission and descriptor validation for
typed records. The Doc Table still uses a deliberately collapsed legacy
projection and commit adapter until its format switch; do not remove it before
remaining Phase 0 fixtures, and do not infer redesign completion from this gate.

After the added recordless fixture and the legacy-adapter rename, the final
formatted-source full gate again reported 1527 passed / 5 skipped on each
full-feature backend runner and 1375 passed / 5 skipped on minimal, with both
VM integration tests passing. `rg encode_entry_delta crates/lib/src` returned
no matches; the legacy Doc Table commit adapter remains as `legacy_doc_delta`.

## Phase 0 continuation: actual typed projection backend matrix

Intended: execute physical-key point reads and paged transaction overlays, cold
streaming deletes across the 128-mutation chunk boundary, and a gated backend
fetch race on the storage engine actually selected by each runner. Include a
recordless seam on that same engine and a test-only authenticated-key envelope
for encrypted physical-key identity and client-side history decryption. Do not
switch the Doc-backed Table or claim its future service codec is implemented.

Performed: `projected_backend_matrix_physical_pages_and_cold_delete` uses
SQLite in-memory, isolated PostgreSQL schema, in-memory backend, or a live Unix
socket daemon authenticated as its bootstrap user according to `TEST_BACKEND`;
creates a signed database with the user API. It commits 140 typed rows, commits
Deletes at keys 000 and 139 in a separate Entry, resolves a cold RecordView,
checks physical absence and an interior surviving key, then pages with exclusive
physical cursors at limit 7 through staged update and insert; the published
view retains its old bytes. The existing `projected_real_backend_fetch_rejects_racing_overlay`
now gates the actual selected backend's scan until a competing stage and rejects
StaleCursor. `projected_recordless_fallback_reduces_typed_history` now forwards
Entry reads through that selected engine, refusing all record methods. The
new `projected_encrypted_physical_identity_and_recordless_fallback` uses a
test-only reversible ciphertext envelope binding the logical key, a reversed
physical key sort, mismatch rejection, physical-order paging, and recordless
history decryption on each selected engine including the authenticated socket.
It is not a PasswordStore interoperability test or server-side maintenance grant.

Negative control: disabling Deletes in the streaming projection made the SQLite
matrix fixture fail 0 passed / 1 failed (exit 101) at the deleted point read;
restored and reran the complete gate. A first negative control removed only the
legacy collapsed-path Delete and stayed green: it does not exercise this typed
projection and cannot be used as evidence. No production defect was observed in
these tested paths; no production code changed. The service path here uses
client-authorized record staging; it does not establish read-scoped typed
maintenance, which is still missing.

Final `nix develop -c nix run .#fix` succeeded; `nix develop -c just nix full`
passed on restored formatted source. Nextest summaries: in-memory, SQLite,
PostgreSQL, service each 1529 tests run: 1529 passed, 5 skipped; minimal 1377
tests run: 1377 passed, 5 skipped. All four named tests reported PASS in each
runner; NixOS service and OCI container integration tests passed. These are
unit-module tests to access internal typed APIs but select real SQL engines and
socket RPC, rather than rerunning an in-memory engine under matrix labels.

Newly required: true encrypted PasswordStore identity and remote read-only
client decrypting fallback, service authorization and typed maintenance
dispatch, automatic remote ambiguous chunk retry/recovery, then Table switch,
legacy removal, service/encryption parity, benchmarks and another full gate.
Existing Doc-backed Table and `table:v0` remain unchanged; no PR or push.

## Phase 0 continuation: unlocked PasswordStore authenticated history fallback

Intended: supply a real encrypted client-side typed Store history read after canonical Read authorization and an explicit server maintenance capability refusal, without a client staging token. Reject descriptor claims; keep decrypt failures separate from capability refusal. Preserve the Doc-backed Table and `table:v0` pending remaining contracts.

Performed: `PasswordStore<S>::get_state` requires `open`, then uses the transaction's registered decryptor with a typed remote EnsureStoreStateGeneration request. On the server's `RecordMaintenanceUnavailable`, the client fetches verified tips and authorized ordered Entry history, decrypts each subtree delta locally and merges `S::Data`; other errors propagate. Local handles use existing typed full-state logic. The socket fixture creates two actual encrypted DocStore Entry deltas, gives another user only global Read, unlocks locally, reads both values, rejects a bad password, confirms the generic ciphertext-only API cannot parse them, denies staging, and rejects unauthenticated and authenticated mismatched descriptors. The existing same-suite `store_state_read_rejects_wrong_descriptor_and_unauthorized_reader` fixture checks denied canonical Read cannot become fallback. A unit fixture modifies ciphertext and confirms decryption failure is not `RecordMaintenanceUnavailable`. No Table switch or client publication occurs.

Negative control: temporarily removed decryption in the fallback Entry fold; the new socket fixture failed 0 passed / 1 failed (exit 101) at the encrypted state read with a serde error. Restored and ran the full gate. Focused restored live socket and tamper tests each passed 1/1.

Final formatted-source `nix develop -c nix run .#fix` succeeded; `nix develop -c just nix full` exited 0. Nix nextest: in-memory 1531 run / 1531 passed / 5 skipped; SQLite 1531/1531 (1 leaky) / 5 skipped; PostgreSQL 1531/1531 / 5 skipped; service 1531/1531 (1 leaky) / 5 skipped; minimal 1378/1378 / 5 skipped. Both changed fixtures PASS in every applicable runner; NixOS service and OCI container integration passed. Scope limit: the socket fixture's daemon runs InMemory even in the outer backend matrix; it tests a real authenticated read-only socket and actual PasswordStore crypto, not tampered Entry replay over an authenticated socket. Generic `get_store_state::<PasswordStore<S>>` remains unusable without an unlocked handle. Typed projected get/scan and read-scoped server maintenance for future non-Doc codecs remain separate; Doc Table and table:v0 unchanged. Remaining Phase 0: authorized remote ambiguous retry/recovery, registered typed descriptor dispatch beyond current plaintext codecs, and tampered-history path test before the Table switch.

## Phase 0 continuation: explicit authenticated staging recovery seam

Intended: allow a caller owning credentials to supply a newly authenticated connection after an ambiguous transport result, preserving exact encoded chunk bytes; never have RemoteBackend silently mint login credentials or start a replacement build. Resolve terminal publication to a new session-scoped view; fail closed on unknown or denied token.

Performed: `RemoteConnection::send_staging_chunk_with_recovery` retains the caller's request bytes, queries scoped status only after I/O ambiguity, sends those bytes again only for Active and resolves Published/Adopted by idempotent publish on the supplied connection. `publish_store_state_with_recovery` similarly resolves an ambiguous publish. Neither reconnects nor authenticates internally; the caller supplies the already authenticated connection. A real socket fixture restarts the daemon with the same backend, denies unauthenticated, wrong-user and wrong-database recovery, acknowledges an identical retry, uploads the next ordered chunk without starting another build, recovers ambiguous publication and checks the fresh view reads the intended row. Negative control bypassing recovery failed 0 passed / 1 failed (exit 101) at the unauthenticated denial; restored focused fixture passed 1/1. This is an explicit opt-in coordination seam, not automatic recovery of arbitrary `RemoteBackend` calls; the latter cannot own login credentials. Failure on a second ambiguous response remains caller-visible with the original payload for subsequent retry.

Final `nix develop -c nix run .#fix` succeeded; formatted-source `nix develop -c just nix full` exited 0. In-memory, SQLite, PostgreSQL and service nextest each reported 1532 tests run / 1532 passed / 5 skipped (SQLite 1 leaky); minimal 1378/1378 / 5 skipped. The named socket test PASS appears in all four full-feature runners (its daemon uses InMemory); NixOS and OCI VM integration passed. Doc Table and table:v0 remain unchanged. Remaining Phase 0: stronger typed descriptor dispatch and encrypted projected get/scan/tampered Entry socket fixture; whole redesign, parity and benchmarks remain incomplete. No push/PR.

## Phase 0 continuation: password-projected read-only history and signed tamper

Intended: exercise real `PasswordStore<S>` encryption, not a test envelope, for
point and physical-order paged typed projection on an authenticated read-only
socket. Preserve the existing descriptor mismatch, password, and cursor
contracts. Never stage client maintenance or switch the Doc-backed Table format.

Performed: `PasswordStore<S>::projected_get` and `projected_scan_page` require an
unlocked handle and a projection matching the wrapped Store descriptor. On a
remote instance they use the existing canonical Read-gated typed ensure request,
fold authorized verified Entry history with the locally registered decryptor,
project physical keyed records and decode through the password record AEAD.
The socket fixture reads two real encrypted DocStore Entries as a second user
with global Read and no Write; point get returns the right row and two pages
return both rows in opaque physical order. A cursor from another transaction
returns StaleCursor. Wrong password fails at open; a locked handle cannot use
the projected API; an incorrect projection returns TypeMismatch, not
RecordMaintenanceUnavailable. A malformed descriptor sent to the authenticated
socket is rejected before the maintenance refusal.

The same fixture signs an Entry with an intentionally invalid opaque encrypted
payload using the authorized owner key, submits it over the socket, confirms
its ID occurs in Bob's Verified frontier, and then confirms both point and page
reads on a fresh unlocked read-only handle propagate the actual decrypt error
(`ImplementationError`), not the capability refusal. This is not corruption
of an existing immutable Entry: the forged Entry has a distinct content ID and
valid signature, and the fixture's isolated in-memory daemon is disposable.
Negative controls: removing the projection-descriptor check failed the socket
test (0 passed/1 failed, exit 101); removing submission and frontier assertion
from the tamper fixture caused the decrypt-error assertion to fail on the old
valid plaintext (0/1, exit 101). Restored focused test passed 1/1. An initial
negative run omitted a larger segment and failed compilation; it was discarded
and the second negative run reached the intended downstream assertion.

`nix develop -c nix run .#fix` succeeded; full `nix develop -c just nix full`
on formatted dirty source succeeded after one retry. First run: 1532/1532
in-memory, SQLite, service, and 1378/1378 minimal (5 skipped each), but
PostgreSQL ownership-release timing test failed 1531/1532 (unrelated test,
`StorageAlreadyOwned` immediately after dropping its backend). Retry of the
unchanged source completed 1532/1532 PostgreSQL, 5 skipped; full gate and both
NixOS/OCI VM integrations passed. Nix logs for all five derivations confirm
the complete counts and socket fixture PASS in every full-feature runner; the
service fixture uses an in-memory daemon under each outer runner. `nix flake
metadata` identified the formatted dirty source snapshot, containing the
signed corrupt Entry test and typed API. No Table format change (`table:v0`).

Remaining: currently remote projected reads fold full decrypted history per
point/page; an independently authenticated read-only record view could avoid
that cost later. Page cursors detect local transaction overlay/view changes,
not a remote frontier changing between page calls. Server codec dispatch for
additional plaintext typed Stores, automatic high-level staging recovery,
full Table redesign/parity/benchmarks and another full gate are still open.

## Phase 0 — explicit typed server codec dispatch (2026-09-23)

Intended: a Read-gated typed registry from `_index`, not caller-selected
plaintext maintenance; unknown or encrypted effective projection must not
create a cache generation. Keep existing `table:v0` and Doc-backed Table.

Performed: `ServiceServer::register_store::<S>` registers an explicit plaintext
Store type, descriptor and typed state decoder before serving; defaults remain
DocStore and Doc-backed Table. The authenticated ensure handler checks `_index`
type identity and registered effective descriptor strictly before probing or
building state. An unregistered codec or encrypted wrapper (whose wrapped
codec is concealed by `_index`) returns distinct
`RecordMaintenanceUnavailable` after the canonical Read gate, even for a
claimed plaintext descriptor. Duplicate and encrypted registration is refused.
No Table format change, push or PR.

A real Unix socket fixture serves a custom non-Doc `SocketCounter` through a
registered daemon, verifies default DocStore and a Read-only client, rejects
wrong type/version/codec descriptor, and confirms a subsequent canonical read
is unchanged. A second daemon sharing the backend but not the registration
returns `RecordMaintenanceUnavailable` on the authenticated socket and the
client folds typed history; pre-auth request is denied and Write-only staging
remains denied. Encrypted wrapper with both the wrapped descriptor and a
plaintext descriptor refuses maintenance. Negative control disabling registered
codec descriptor equality made this fixture fail (0 passed, 1 failed): it
returned `CrdtValue(Number(7))` for the wrong version. Restored test passed
1/1. The first full Nix gate failed two older tests expecting `TypeMismatch`
for unverifiable encrypted descriptors (1531/1533 in in-memory and SQLite);
updated those assertions to the fail-closed capability contract, then ran the
full gate again on formatted source.

`nix develop -c nix run .#fix` exit 0 (clippy, deadnix, markdownlint,
statix, treefmt). Final `nix develop -c just nix full` exit 0: in-memory,
SQLite, PostgreSQL and service each 1533 passed / 5 skipped; minimal 1378
passed / 5 skipped; changed real-socket fixture PASS in all four full-feature
runners; NixOS service and OCI VM integration tests passed. Flake metadata
locked dirtyRev `0f642f4f3e-dirty` on formatted working source; committed-tip
gate to follow after the signed commit.

Remaining: automatic high-level remote staging retry/recovery, remote-frontier
cursor snapshot and full Table redesign/encryption parity/benchmarks. Re-run
complete accumulated checks on final implementation; no Table switch before
Phase 0 fixtures are complete.

## Phase 0 — remote projected cursor frontier (2026-09-23)

Intended: read-only unlocked PasswordStore scans on a live authenticated socket
must never continue a previous page against a newly Verified source frontier;
local overlay changes during the last network await also reject the page.
Preserve the Doc-backed Table and `table:v0`.

Recovered and evaluated the previous worker's two uncommitted files without
reset: the new client helper had not yet returned a frontier (and its tuple
return did not compile); the cursor field had no producer or checker. Completed
the helper by returning the Verified tips used to fetch authorized Entry
history, compared them with the next page's cursor, and checked the current
Verified tips and overlay revision after the last await before returning.
The pre-existing get-verified-tips helper needed the authenticated session
identity when the database supplies its default identity; otherwise the
read-only socket request was denied. No remote record maintenance or Table
format switch was added. A frontier change between the final tip check and
caller observation remains inherently possible; this is a stale-check, not a
server-pinned multi-request snapshot.

The live socket fixture reads two pages with no writer, commits a third
PasswordStore Entry via the owner between pages, rejects the old read-only
client cursor with StaleCursor, and sees three records on a fresh page.
Negative control removing the cursor frontier comparison failed at this
assertion (0 passed / 1 failed, exit 101); restored fixture passed 1/1.
A deterministic unit fixture pauses the final tip-check future while the
transaction overlay changes, and rejects even when returned tips are
unchanged; bypassing the post-await revision comparison failed 0/1 (exit
101), restored 1/1.

`nix develop -c nix run .#fix` succeeded (clippy, deadnix, markdownlint,
statix, treefmt). Final formatted-source `nix develop -c just nix full`
succeeded: in-memory 1534/1534 (1 leaky), SQLite 1534/1534,
PostgreSQL 1534/1534, service 1534/1534, minimal 1378/1378; 5 skipped
each. Both new/extended fixtures PASS in the four full-feature runners;
the socket daemon uses InMemory under each runner. NixOS service and OCI
container VM integrations passed. No push/PR. Remaining: automatic high-level
remote staging recovery, full Table redesign/parity/benchmarks and complete
accumulated verification before delivery.

## Phase 0 — explicit high-level authenticated upload recovery (2026-09-23)

Intended: RemoteBackend must retain encoded chunks through ambiguous transport and
require a caller-supplied reauthenticated connection to resume the same token;
publication must resolve by status without a fresh build or saved credentials.
Preserve the existing Doc-backed Table and `table:v0`.

Performed: the adapter now binds token upload state to its original target and
acting/session identity, holds its exact encoded unacknowledged request plus
remaining pre-encoded batch chunks, and fences staging/publish/abort during
ambiguous I/O or cancellation. An ambiguous error carries the token and
optional chunk sequence. Explicit `resume_staging` checks status on the supplied
connection, acknowledges/replays retained chunks in sequence, resolves a
terminal publication to a new session view, and replaces the backend socket
only after authorized success. Aborted, expired, unknown, mismatched-target or
wrong-session attempts cannot publish or restart a build. An ambiguous begin
remains an orphan lease/reclamation case because no token was returned.

Real authenticated socket fixtures cover lost request on a closed socket, a
server-accepted chunk with lost adapter acknowledgement across daemon restart,
wrong user/unauthenticated and wrong-db status refusal, later ordered delete
and put, and a lost publication acknowledgement resolved to a new view. The
socket server uses InMemory in each Nix runner. Negative control omitting the
exact retained replay failed 0 passed / 1 failed (exit 101) at
`InvalidStoreStateStagingToken`; restored focused tests passed 2/2.
`nix develop -c nix run .#fix` succeeded (clippy, deadnix, markdownlint,
statix, treefmt). Final formatted-source `nix develop -c just nix full` exit 0:
in-memory, SQLite, PostgreSQL and service each 1536/1536 passed (5 skipped),
minimal 1378/1378 passed (5 skipped); both named socket fixtures PASS in all
four full-feature runners; NixOS service and OCI container integrations passed.
A committed-tip gate follows the signed commit.

Phase 0 is not declared complete: the design's backend-neutral lost-publication
request/response and reclamation race matrix is partially exercised by separate
fixtures, but an explicit full inventory against all Phase 0 contract conditions
and reproducible cancellation / concurrent recovery test remain needed before
switching Table. Phase 4 Table switch, encryption/service parity, docs and
benchmarks are still open. No `table:v0` change and no push/PR.

## Phase 0 fixture inventory — approved six-contract gate (2026-09-23)

This checklist maps **Phase 0 of the approved design** to runnable checks, not the
later Table acceptance checklist. Execute the accumulated full gate with
`nix develop -c nix run .#fix` and `nix develop -c just nix full`; the backend
conformance tests use `test_backend()` under `TEST_BACKEND=inmemory|sqlite|postgres|service`.
The `service` implementation of that backend-neutral factory falls back to a
local backend for raw staging operations (no test clock on the RPC), so the
separate live-socket checks below are essential. A checked box means a fixture
exists, not that the entire Table redesign is delivered.

- [x] **CanonicalJson golden parsing/bytes:** `crdt::canonical_json::tests::rfc_and_boundary_vectors` (number boundaries, negative zero, Unicode UTF-16 ordering, escapes, invalid numbers and duplicate keys), `typed_readers_cannot_change_canonical_bytes`, `canonical_row_survives_entry_cbor_roundtrip`, and `row_operation_is_inline_json_not_an_escaped_doc_string`.
- [x] **Token lifecycle, order, digest, retry and lease:** `backend::store_state_records::{ordered_physical_staging_and_empty_generation,sequenced_token_status_adoption_and_abort,terminal_horizon_and_unknown_replacement_guards,expired_orphan_is_reclaimed_and_terminal_result_is_retained}` exercise physical put/delete/put, empty generation, gaps/conflicts/stale replay, status and bounded retention. `service::{test_remote_staging_rejects_late_and_conflicting_replays,test_exact_staging_chunk_retry_after_reconnect_and_delete,test_remote_backend_resume_lost_request_response_and_restart}` cover encoded wire digest/reconnect. `backend::store_state_records::lost_publication_request_expires_before_safe_rebuild` deliberately does not call publish: a 599-second lease/grace probe cannot reclaim; 601 seconds expires the partial build, rejects late publication and permits only a complete replacement. `lost_publish_response_is_resolved_by_token_retry` and `service::test_remote_backend_resume_lost_publication_response` test the opposite (request committed, response lost).
- [x] **Atomic transaction revision:** `transaction::tests::{projected_staging_installs_concurrent_writes_in_canonical_and_both_overlays,projected_staging_failures_leave_canonical_and_overlays_unchanged}`. Canonical and physical overlay install or neither installs; a gated competing writer cannot lose either update.
- [x] **Projection laws:** `crdt::map::tests::reference_projection_obeys_identity_merge_and_composition` plus `merge_laws_and_tombstones`, `crdt::lww::tests::exhaustive_associativity_and_identity`; typed streaming deletion across chunk boundary: `transaction::tests::projected_backend_matrix_physical_pages_and_cold_delete` and `projected_streaming_history_applies_deletes_across_chunks`.
- [x] **Stale cursors, typed state, maintenance refusal:** `transaction::tests::{projected_page_cursor_rejects_put_delete_and_other_view,projected_page_discards_awaited_fetch_after_racing_mutation,projected_real_backend_fetch_rejects_racing_overlay,remote_scan_rejects_overlay_mutation_during_final_frontier_await,typed_store_state_folds_custom_crdt_without_doc_conversion,projected_recordless_fallback_reduces_typed_history}`; `service::{registered_typed_socket_maintenance_is_read_scoped,store_state_read_rejects_wrong_descriptor_and_unauthorized_reader,read_only_password_store_folds_authenticated_remote_history}`. Real socket frontier-change check is in the password read-only test; a stale-check is not a pinned cross-request snapshot.
- [x] **Failure, cancellation, lost request/response, race, orphan reclamation:** backend matrix `backend::store_state_records::{failed_publish_is_invisible_and_ready_derived_is_immutable,cancelled_build_stays_private_and_replacement_publishes,lost_publication_request_expires_before_safe_rebuild,lost_publish_response_is_resolved_by_token_retry,expiration_racing_publication_has_one_terminal_winner,staging_publication_and_abort_race_is_terminal,terminal_horizon_and_unknown_replacement_guards}`. The publish/sweep race synchronizes the start and asserts exactly Published or Expired and matching resolvability/reclaim count; a completed publish remains immutable through a later sweep. `service::{test_remote_backend_resume_lost_request_response_and_restart,test_remote_backend_resume_lost_publication_response,test_remote_backend_cancelled_upload_concurrent_recovery}` use authenticated Unix sockets: the latter pauses a high-level send _after_ encoding but _before_ transmission, cancels the future, verifies no partial published target, then starts two concurrent authenticated recovery calls: exactly one resumes the original token/chunk, the other is fenced; publication exposes only the complete row.

**Genuine scope gaps:** backend-neutral lease-aging and simultaneous sweeper
interleavings are exercised against real InMemory, SQLite and isolated
PostgreSQL in their own Nix runners, **not** through a service RPC because
that test-only clock is deliberately unavailable to service clients. Live
service cancellation and both lost-publication sides instead run against an
InMemory daemon and a real authenticated socket, not an SQL-backed daemon.
The barrier races only order whole public operations; they do not guarantee a
specific internal mid-transaction interleaving (the PostgreSQL
`same_token_stage_vs_publish_is_serialized` test separately gates that SQL
critical section). A deterministic live-socket lease/sweep race would require
an internal server test seam, not a public clock-control RPC. No new Table
behavior, migration, or encrypted Table parity can be claimed from Phase 0.

Performed on this slice: direct focused restored local tests returned 1 passed /
0 failed each for the new lost-request, amended race and live-socket cancelled
upload fixture. Negative control dropping pending uploads produced 0 passed /
1 failed in the cancellation test (timeout at the gate; no upload reached it),
then restored. Final formatted dirty-source full Nix gate passed: in-memory,
SQLite, PostgreSQL and service each `1538 tests run: 1538 passed, 5 skipped`;
minimal `1379 tests run: 1379 passed, 5 skipped`. Nix logs show PASS for
both changed backend fixtures in all five runners and for the new socket
fixture in all four full-feature runners. NixOS service and OCI container
integration VMs passed. The signed-tip gate is recorded after commit below.

## Phase 0 service parity — persistent SQLite RPC (2026-09-23)

Intended: close the fixture inventory's SQL-backed daemon gap without exposing a
clock-control request or bypassing authenticated scope. The `testing` feature
adds only a local Instance-to-backend token-aging/reclaim seam; it retrieves the
backend-owned target for the opaque token instead of accepting a caller's
claimed target. Production builds and the service protocol have no such method.
The daemon's ordinary request dispatch also reclaims expired builds. The
fixture explicitly drives reclamation locally when a precise count matters.

Performed: `sqlite_service_lost_publication_request_expires_and_restarts`
starts an authenticated Unix socket backed by a file SQLite database; stages a
partial row but drops the publication request, advances the private lease to
599 seconds (not reclaimable), then to 601 seconds (Expired). It rejects late
publish, checks the partial row never resolves, denies pre-auth, wrong-database
and wrong-user status and replacement, and stages/publishes a complete
replacement. It shuts down the daemon, releases exclusive ownership, opens the
same SQLite file and socket anew, then verifies Expired and Published outcomes
and the exact published row through authenticated RPC. This checks persistent
token state, not a fresh in-memory backend behind a socket.

`sqlite_service_publish_vs_reclaim_has_one_terminal_result` synchronizes
publication RPC and local reclamation against the same aged SQLite token,
accepting only Expired with no published view or Published with the complete
row resolvable. The service may reclaim before the explicit sweep during its
normal dispatch, so its sweep count is not a winner oracle. It also publishes
another complete token before aging it, and checks a subsequent sweep cannot
expire or remove it. Wire view IDs are session handles, not generation IDs;
therefore compare readable content/status instead of equality of two view IDs.
This barrier establishes competing whole operations, not an artificially
paused SQL statement; the existing PostgreSQL stage/publish pause tests cover
an internal critical section separately.

Focused `nix develop -c cargo test -p eidetica --all-features --test it
sqlite_service_ -- --nocapture` on the formatted source: **2 passed, 0
failed**, no ignored, 1049 filtered out. During construction the fixture
initially failed 0/2 because a service user scope changes the backend target;
the seam now fetches the actual target by token ID. A second assertion failed
1/2 because wire handles are freshly minted on each resolution; it now checks
actual record contents. Negative control in the published-first branch
asserting an incorrect reclaim count of 1 failed **0 passed / 1 failed** at
`left: 0 right: 1`; restored 2/2. The final suite reaches actual service
authorization, SQLite lease transition, restart persistence and both terminal
race branches' acceptance predicate (published-first independently exercised).

`nix develop -c nix run .#fix` succeeded (clippy, deadnix, markdownlint,
statix, treefmt). Formatted-source `nix develop -c just nix full` passed:
in-memory, SQLite, PostgreSQL and service each **1540 tests run: 1540 passed,
5 skipped**; minimal **1379 tests run: 1379 passed, 5 skipped**. Both new
named tests reported PASS in each full-feature runner; each explicitly creates
its own file-backed SQLite daemon, regardless of outer `TEST_BACKEND`. NixOS
service and OCI container integration tests passed. `git diff --check` clean.
Signed committed-tip gate to follow. A file-backed PostgreSQL service daemon
was not added: hermetic PostgreSQL backend conformance already exercises the
lease/race in its Nix runner, while the added service test covers persistent
SQL ownership and restart with SQLite. This does **not** establish a
PostgreSQL-backed RPC restart test; add one if that extra parity is required.

**Phase 0 readiness for the Table format switch:** the six approved Phase 0
contract groups in the preceding inventory now have executable fixtures,
including the previously missing authenticated persistent-SQL service lease,
orphan, restart and publication-vs-reclamation path. This is readiness to
_begin_ the incompatible Table format switch, not a claim that the Table
redesign is delivered or that every scheduling order was deterministically
forced. Existing Table remains Doc-backed, `table:v0` and `canonical-json:v0`
remain unchanged, and no old/new compatibility or migration is provided.
Remaining Phases 3-6: Table switch, encryption/service Table parity, docs,
benchmarks, and a final accumulated gate; no push or PR.

## Phase 4: incompatible canonical LwwMap Table switch (2026-09-23)

Intended: replace Doc-backed `Table<T>` Entry data with `LwwMap<String,
CanonicalJson>` without changing the `table:v0` or `canonical-json:v0` IDs; no
compatibility decoder for old Doc histories. Use exact UTF-8 keys, stream typed
set/delete deltas into physical records, keep transaction-local overlay and
encrypted/recordless/service reads, and remove the legacy commit adapter.

Performed: Table now stages RFC 8785 rows and tombstones through the existing
atomic revision-checked projected staging path; typed `T` appears only on
set/insert and get/page/search. Direct LWW projection uses exact keys (including
empty, dot, prefix and distinct Unicode spellings). Removed hierarchical Table
projection, path normalization, collapsed publication, legacy transaction
record staging and Table-specific commit reconstruction. A new projection
identity includes `canonical-json:v0`; service read-scoped registered Table
maintenance ensures the record generation instead of only reducing whole-state
JSON. Expired cached views are re-resolved for point and page reads, with
recordless history fallback after unsupported reads. Existing password row
cache and real authenticated Unix socket fixtures pass; cursor tests now compare
rows across different transaction view identities instead of equating opaque
view-bound cursors. Old Doc-path-conflict fixture expectations were replaced.

New fixtures: `test_table_entry_delta_has_inline_canonical_json_and_tombstone`
checks persisted Entry bytes, strict old Doc payload rejection and descriptor;
`test_table_exact_keys_multi_operation_and_cold_warm_reads` checks exact
empty/dotted/prefix/slash/composed/decomposed/emoji keys, multi-op overwrite,
delete/resurrection, ordered pages, search and cache clearing; and
`test_table_delete_does_not_swallow_typed_decode_failure` checks that typed
boundary errors propagate. Existing password physical-order, encrypted
recordless fallback and service cold/warm/expired-view fixtures exercise the
changed paths. Negative controls: corrupting the expected `a` value in the
exact-key test yielded 0 passed / 1 failed, then restored; swallowing the typed
delete error yielded 0 passed / 1 failed, then restored. First direct full
integration run yielded 1037 passed / 10 failed (legacy Doc expectations,
cache-count descriptor, stale view). Second run yielded 1040 passed / 5 failed;
third 1044 passed / 1 failed (cross-view cursor equality). Diagnosed and
repaired those rather than accepting a partial gate.

Final formatted-source `nix develop -c nix run .#fix` succeeded with clippy,
deadnix, markdownlint, statix and treefmt (0 changed). Final
`nix develop -c just nix full` exited 0: InMemory, SQLite, PostgreSQL and
service each **1540 tests run: 1540 passed, 5 skipped**; minimal **1379
tests run: 1379 passed, 5 skipped**. Nix logs show PASS for all three named
new fixtures in the full runners, the service warm encrypted Table test, and
encrypted recordless pagination; NixOS service and OCI container integration
VMs passed. `git diff --check` clean. Signed committed-tip gate to follow.

Scope ceiling: this is the Table **format switch**, not completion of Phases
5-6. Encrypted projection's existing cold builder still gathers a logical
`RecordMutations` map before writing one physical batch (not bounded streaming
for very large password tables). A read-only unlocked PasswordStore's remote
projected path folds authenticated full history client-side, rather than using
a server-maintained encrypted generation. New Table-specific encrypted
wrong-password/tamper, real service ordered-delete/recordless-on-socket and
large encrypted chunk fixtures, docs/benchmarks and final accumulated parity
are still required before the todo can be handed off. No mixed old/new
`table:v0` data is supported; regenerate old databases/fixtures, never bump
silently to `v1`. Branch remains local, no push/PR.

Signed-tip verification for Phase 4: commit `64ffab2428` has a valid bot
ED25519 signature, clean worktree; `nix flake metadata --json` reported
revision `64ffab2428d525674e298dcd49012cd0ecf41fa9` and `dirtyRev: null`.
`nix develop -c just nix full` exited 0 on that revision (reused the exact
formatted-source test derivations). `nix derivation show
.#checks.x86_64-linux.test` listed five backend runner inputs; `nix log` on
each reported: in-memory, SQLite, PostgreSQL and service each `1540 tests
run: 1540 passed, 5 skipped`; minimal `1379 tests run: 1379 passed, 5
skipped`. No separate service deployment was made; the new Table socket
fixtures ran against a test daemon, not a production daemon. The signed-tip
run also reused the passing NixOS and OCI VM checks. No push or PR.

## Phase 5: bounded encrypted Table cold projection slice (2026-09-23)

Intended: transform each historical row mutation to its keyed physical record and authenticated value without retaining the full logical Table; preserve put/delete ordering across 128-mutation and 1 MiB chunk boundaries, then publish atomically. Full Phase 5 also requires tamper/no-partial-publication and SQL/service parity.

Performed: encrypted `publish_record_view` now decodes each historical Entry, streams its projected mutations into bounded ordered chunks, transforms each key and encrypts each put before staging, stages physical deletes and publishes only after all chunks succeed. The existing ordered staging implementation removes deleted physical rows and publishes the private generation atomically. Shared chunk framing and digest logic is reused from plaintext projection. Non-encrypted path unchanged; `table:v0` and `canonical-json:v0` unchanged.

Focused regression `test_password_table_cold_streams_overwrite_delete_and_resurrection`: three historical Entries, 260 rows across the chunk limit, overwrite and delete across chunks, 44 resurrections, cold clear and 217 physical rows, wrong password before publication, paginated physical-order scan and opaque 32-byte keys. Direct `nix develop -c cargo test --all-features -p eidetica --test it test_password_table_cold_streams_overwrite_delete_and_resurrection -- --nocapture`: **1 passed; 0 failed**. Negative control turning physical deletes into authenticated puts: **0 passed; 1 failed** (260 records instead of 217), restored. First attempt failed to compile due to moved borrowed keys, then fixture erroneously expected immediate reclaim after one clear; corrected to two clears per two-phase derived reclamation. `nix develop -c nix run .#fix` passed (clippy, deadnix, markdownlint, statix, treefmt); `git diff --check` clean. Formatted-source `nix develop -c just nix full` exit 0: in-memory, SQLite, PostgreSQL and service runners each **1541 tests run: 1541 passed, 5 skipped**; minimal **1380 tests run: 1380 passed, 5 skipped**. Named regression PASS in all runners, but it intentionally creates a local InMemory engine even in SQL/service runners; it does NOT prove SQL or socket encrypted cold streaming. NixOS and OCI VM integrations passed. Signed committed-tip gate to follow.

Newly required before Phase 5 completion: make an actual backend-neutral encrypted cold fixture run on SQLite/PostgreSQL/service daemon, and prove a late malformed/tampered encrypted historical Entry aborts after earlier chunks without partial publication, plus swapped physical ciphertext/key tamper on real backend. Existing `PasswordEncryptor` unit fixtures check wrong store, wrong physical key, malformed/modified ciphertext; they are not cold publication failure evidence. Retain Phase 6 service ordered-delete/recordless socket parity, docs/benchmarks and final accumulated gate. No push/PR.

## Phase 5 continuation: encrypted Table cold matrix and socket parity (2026-09-23)

Intended: exercise the real selected in-memory, SQLite and isolated PostgreSQL engines rather than treating a local InMemory fixture under SQL runners as backend evidence; exercise authenticated Unix RPC and a recordless socket, ordered encrypted put/delete/resurrection across 260 rows and the 128-mutation boundary, a late signed corrupt encrypted Entry with no published generation, and ciphertext under a foreign physical key. Preserve `table:v0` and `canonical-json:v0`.

Performed: extracted the previous 260-row fixture into shared populate/assert helpers; retained its local InMemory instrumentation. `test_password_table_cold_streams_on_selected_backend` constructs `Instance::create_backend(test_backend())` for actual in-memory/SQLite/PostgreSQL (explicitly skips `service`, whose `test_backend` is a local InMemory fallback). After clearing derived state, it verifies wrong password, expected 217 rows and values, physical rather than logical order, all expected keys, and the actual selected backend's published 217 opaque ciphertext records in physical-key order. A new signed late opaque Entry becomes the newest store tip, then the cold build fails and the exact derived request remains unpublished. A separate fixture publishes a deliberately mismatched physical key/ciphertext pair on the selected backend and checks scan authentication failure. The authenticated socket fixture uses a daemon and client with distinct `Instance` handles, verifies the same rows and cold client scan, wrong password, late corrupt signed Entry and no client-visible generation, and swaps a physical key through the server-side owner seam to verify socket read rejection. A recordless InMemory engine behind a real authenticated Unix socket verifies client-side decrypted history and physical-order pages without record support. No production source or wire version changed.

Boundary: the authenticated service read-only client projects ordered decrypted history locally, not the encrypted server-side cold builder; the server-side materialization in the socket fixture is prepared by local writes and physically inspected through the daemon's engine. The true encrypted cold builder and late abort run on actual in-memory, SQLite, PostgreSQL in the backend-neutral fixture. This does not claim a server-maintained encrypted projection over RPC. Service runner's `test_backend` fallback does not count as service evidence; only named socket fixtures do.

Negative controls: omitting the late socket Entry failed `0 passed; 1 failed` on the expected error; omitting the late SQL Entry failed `0 passed; 1 failed`; leaving the physical key unchanged failed `0 passed; 1 failed`; replacing encrypted deletes with malformed puts failed the actual SQLite cold fixture `0 passed; 1 failed` (record authentication error). All restored, SQLite focused fixture `1 passed; 0 failed`, socket streamed and identity focused `1/1` and `2/2`, recordless socket `1 passed; 0 failed`. An intermediate full Nix gate failed only the new socket record-count assertion (260 instead of 217): a single clear leaves a reader-pinned generation; corrected the fixture to use the documented two-phase clear and reran the full gate.

Final formatted-source `nix develop -c nix run .#fix` passed clippy/deadnix/markdownlint/statix/treefmt; `nix develop -c just nix full` exit 0 with actual Nix test derivation logs: in-memory, SQLite, PostgreSQL, service each **1546 tests run: 1546 passed, 5 skipped**; minimal **1381 tests run: 1381 passed, 5 skipped**; named new fixtures PASS where applicable. NixOS service and OCI container VMs passed. Re-run this accumulated full gate on the signed committed tip. Remaining todo scope: Phase 6 docs/benchmarks, final accumulated checks and handoff; no push/PR.

## Phase 6 partial: documentation and comparable Table measurements (2026-09-23)

Intended: describe Map/Lww/LwwMap, deterministic Entry-order and the incompatible
Doc-to-canonical-JSON Table format; fix the cold benchmark, compare baseline and
branch payload, writes, reads, scans and encryption; record a cold rebuild memory
measurement, then finish the accumulated gate. Per the approved contract,
`table:v0` and `canonical-json:v0` remain unchanged; old databases/fixtures must
be rebuilt, not mixed with the new format.

Performed: corrected public/internal CRDT, Table, cache, Store-state, encryption,
performance and testing text. Clarified exact UTF-8 keys, typed read boundary,
physical encrypted order and client-side read-only service fallback (not server
maintenance). Existing Map/Lww/LwwMap Rust doctest examples remain executable.
Repaired both cold benchmark variants by clearing derived generations after
setup; the 1k/10k cold point benchmark no longer pre-reads a row. Added identical
Table payload reporting, fresh one/32-row commit, 100-row paged scan, and fresh
32-row plaintext/password-wrapped scan cases to the old and new harness.

Measurement protocol: old `e0f4645178` detached baseline and candidate starting
at `d80f463da9` with only the _same_ benchmark harness edit temporarily copied
to the baseline (not committed there); both built in release profile with the
same flake toolchain and `TEST_BACKEND=inmemory`. Run sequentially, on the same
x86_64 AMD Ryzen 9 7900 (12 cores/24 threads), 124 GiB RAM host. At start:
~49 GiB available RAM, load averages 3.37/5.38/11.17; background load was **not**
quiescent. Exact command per tree:

```sh
TEST_BACKEND=inmemory nix develop -c cargo bench -p eidetica --bench table_cache_benchmarks -- 'table_(payload_bytes|write|page|warm_cache|cold_point_read)' --sample-size 10 --warm-up-time 0.1 --measurement-time 0.2
TEST_BACKEND=inmemory nix develop -c cargo bench -p eidetica --bench table_cache_benchmarks -- table_encrypted_page --sample-size 10 --warm-up-time 0.1 --measurement-time 0.2
```

Payload numbers are the `Entry::data("bench_table")` subtree byte lengths for
the same exact keys and typed rows; Entry headers/signatures and storage index
are excluded. The payload-only harness prints byte lengths, not a meaningless
sub-nanosecond Criterion `black_box(size)` timing. Results (baseline → branch;
Criterion 95% confidence interval for time, 10 samples):

| Workload                                      |             Old Doc |          New LwwMap |
| --------------------------------------------- | ------------------: | ------------------: |
| one-row payload                               |                85 B |                63 B |
| 32-row payload in one commit                  |              2414 B |              2113 B |
| fresh one-row write+commit                    | [75.195, 76.341] µs | [76.201, 79.935] µs |
| fresh 32-row write+commit                     | [115.19, 119.01] µs | [806.94, 825.48] µs |
| warm point, 100 historical single-row commits | [61.025, 62.154] µs | [59.933, 62.162] µs |
| corrected cold first point, 1k batched rows   | [3.3316, 3.4327] ms | [1.5949, 1.6404] ms |
| corrected cold first point, 10k batched rows  | [302.23, 313.11] ms | [16.863, 18.459] ms |
| scan 100 rows, page size 10                   | [90.202, 91.092] µs | [186.16, 188.25] µs |
| scan 100 rows, page size 50                   | [88.389, 88.803] µs | [171.97, 175.63] µs |
| fresh plain 32-row scan                       | [39.339, 42.467] µs | [104.60, 105.93] µs |
| fresh encrypted 32-row scan                   | [22.469, 24.261] ms | [17.268, 18.278] ms |

The fresh encrypted scan includes `PasswordStore::open`/Argon2id, first-generation
build, authenticated row decoding, and 32-row scan; the plain leg includes its
first build. This is **not** isolated per-record encryption overhead or a warm
point-read comparison. Plain/encrypted legs have different configuration/history
payloads. A fresh 32-row transaction is substantially slower on the new path;
do not present the redesign as an across-the-board speedup. The 10k candidate
reported Criterion's estimated 664-second collection warning (setup dominates,
not timed); its 10 timed reads still completed. Short 0.2s target, only 10
samples, dynamic CPU clocks and shared host load limit precision; intervals
that overlap (warm 100) do not establish a change. The older benchmark groups
with one commit per inserted row show per-history setup cost, not fixed-row
complexity. These numbers describe InMemory only, not SQLite/PostgreSQL/socket.

**Cold peak-memory limitation:** no peak RSS claim. `iter_with_setup` builds a
new database and many Entries inside the _same_ Criterion process on every
sample, so `/usr/bin/time -v` or `/proc/self/status` maximum resident set over
that process measures cumulative setup/allocator high-water plus Criterion,
not the cold projection. An isolated one-shot child with a prebuilt persisted
fixture and a process-level baseline RSS (or allocation instrumentation scoped
to reconstruction) is needed to attribute cold-build peak memory. The current
history API still collects `Vec<Entry>` even though row mutations stream in
bounded 128-change/1-MiB private chunks. Churn-heavy payload, delete-heavy
payload, encrypted cold-build-only and warm encrypted point comparisons likewise
remain unmeasured. No claim of bounded total cold RSS or general throughput win.

`nix develop -c cargo check -p eidetica --bench table_cache_benchmarks` compiled
the changed harness; both benchmark legs emitted time CIs and payload bytes.
`nix develop -c nix run .#fix` succeeded (clippy, deadnix, markdownlint,
statix, treefmt 0 changed in final run). Final formatted-source
`nix develop -c just nix full` exited 0: in-memory, SQLite, PostgreSQL and
service each **1546 tests run: 1546 passed, 5 skipped**; minimal **1381 tests
run: 1381 passed, 5 skipped**. NixOS service and OCI container integration
VMs both reported passed. The gate exercises Table/encryption/socket behavior
from preceding phases; Criterion results are separate InMemory measurements.
`git diff --check` clean. Signed-tip recheck follows the local commit. No
push/PR.

## Phase 6 follow-up: batch and plain-scan regression diagnosis (2026-09-23)

Intended: reproduce 32-row write and plain-scan regressions against the unchanged
baseline with an identical harness; isolate cost before selecting a narrow safe
optimization, preserve canonical Entry bytes, revision-atomic staging, Table
semantics and stale-cursor checks. Do not infer a broad performance win.

Performed: reused the existing identical, uncommitted benchmark harness on old
`e0f4645178` and the signed candidate `44f8849584`. Both ran sequentially on
this host with `TEST_BACKEND=inmemory`, release profile under `nix develop`,
Criterion CLI `--sample-size 15 --warm-up-time 0.3 --measurement-time 0.5`;
no source-level Criterion sample overrides in this bench. Available RAM ~50 GiB,
load 5.06/7.37/10.29: shared host not quiescent. Exact benchmark filter:
`table_(write|page|encrypted_page)`. Timing is full fresh write/commit, warm
100-row scan in 10/50-row pages, and fresh 32-row plain/encrypted scan,
respectively. All reported intervals below are Criterion time 95% CIs.

| Workload                                                   |            Baseline |    Candidate before |         Candidate after |
| ---------------------------------------------------------- | ------------------: | ------------------: | ----------------------: |
| 32-row fresh write                                         | [110.48, 114.27] µs | [826.25, 834.34] µs | not changed/re-measured |
| warm 100-row scan / 10                                     | [90.389, 91.373] µs | [183.96, 187.11] µs |     [173.09, 175.20] µs |
| warm 100-row scan / 50                                     | [89.073, 90.986] µs | [176.60, 182.59] µs |     [166.86, 169.01] µs |
| fresh plain 32-row scan                                    | [37.164, 37.808] µs | [106.09, 107.48] µs |     [102.24, 105.46] µs |
| fresh encrypted 32-row scan (includes password derivation) | [18.049, 19.802] ms | [17.850, 19.294] ms |     [17.917, 19.332] ms |

Read-side path inspection: on the new path every page (even with no staged
rows) copies the backend page into a BTreeMap and re-collects it, while the old
scan returned the backend page directly. A narrow unstaged fast path returns
the backend's bounded ordered page directly, wrapping its physical continuation
in the same opaque transaction/view/revision/projection/frontier cursor; keeps
revision checks after the await (including failed fetch) and after decoding.
The fresh plain interval moves down slightly and warm 100-row intervals move
down ~5-7% in this run, but the ~2x scan regression remains. The evidence does
not isolate the other costs of typed record resolution, canonical decoding,
revision validation and backend page iteration. Do not eliminate their safety
checks merely for speed. An added regression exercises exact continuation,
end-of-scan, a stage racing the await, and stale continuation after a stage;
restoring old code makes it fail (old merge calls fetch twice with a backend
that returns a continuation), fixed path passes.

Write-side discriminating ablations were deliberately temporary and reverted.
Replacing the generic all-staged-key conflict predicate with an exact-key lookup
(no valid general projection semantics) measured 32-row write
[829.92, 840.10] µs: not the cause. Bypassing canonical cumulative
merge/deserialization and writing only the latest delta (invalid history/row
semantics) measured [222.09, 228.15] µs, compared to [826.25, 834.34] µs
before. This identifies repeated full canonical state merge/serialization in
`stage_projected_delta` on every row as the dominant measured batch cost; it
cannot be simply skipped without losing prior rows in the committed Entry.
A mutable accumulator, deferred serialization, or batched staging API would
alter the revision-atomic contract and require design/edge testing; this bounded
diagnosis leaves batch writes unchanged rather than landing a speculative fix.
One-row commits stayed near baseline. Do not generalize the read fast path to
staged overlays or encrypted fallback: they still require merge/decrypt and
cursor checks. Backend numbers are InMemory only, not SQL or live RPC.

Verification: restoring original scanner with the new focused fixture produced
`0 passed; 1 failed` (second fetch on backend continuation); repaired scanner
produced `1 passed; 0 failed`. `nix develop -c nix run .#fix` passed
clippy/deadnix/markdownlint/statix/treefmt; `git diff --check` clean.
Formatted-source `nix develop -c just nix full` exited 0: actual Nix runner
summaries were in-memory, SQLite, PostgreSQL, service **1547 tests run:
1547 passed, 5 skipped** each (SQLite 1 leaky); minimal **1382 tests run:
1382 passed, 5 skipped**. The new cursor/race fixture was PASS in all five
runners; both NixOS service and OCI container integrations passed. Signed-tip
recheck follows. Remaining Phase 6 cold RSS/churn/encrypted point and final
handoff still belong to the parent task; no push/PR or version bump.

## Phase 6 continuation: serialize canonical transaction delta once (2026-09-23)

Intended: diagnose the measured 32-row write regression, keep revision-atomic
canonical Entry bytes and logical/physical overlay with last operation per key,
no await under the install lock, concurrent writers, encrypted Table and failed
serialization atomicity. Keep `table:v0` unchanged; compare identical InMemory
32-row fresh-commit Criterion runs before and after.

Performed: `stage_projected_delta` retains an erased typed CRDT accumulator
alongside the overlay at one revision instead of decoding and reserializing the
full canonical delta on every row. It validates each incoming delta's serializer
and projection before install. At commit, serialize the accumulator outside the
install lock, compare all store revisions, clone the Entry builder, apply all
canonical bytes and subtree tips, then install the new builder and seal projected
writes under the lock. Failed serialization or builder cleanup leaves the old
builder and overlays intact. Subsequent commit encryption and Entry signing use
the sealed builder; get_local_data still serializes on demand, so callers can
read typed staged data without mutating the builder. All awaits occur outside
the revision/install critical section. Existing generic `RecordProjection` and
`CRDT::merge` remain supported; no Table-specific serialization shortcut.

Before (the clean parent tip), after (working candidate, same bench source):
`TEST_BACKEND=inmemory nix develop -c cargo bench -p eidetica --bench
table_cache_benchmarks -- 'table_write/commit/32' --sample-size 15
--warm-up-time 0.3 --measurement-time 0.5`. Ryzen 9 7900 shared host, load
not quiescent (~2.88/13.62/16.48 at final read; 51 GiB available RAM).
Criterion 95% time CIs: **[823.97, 880.39] µs before** vs **[257.60,
260.30] µs after final changes**, ~3.3x lower point estimate with nonoverlapping
intervals, but still >2x the earlier old-Doc baseline [110.48, 114.27] µs.
An intermediate candidate measured [253.19, 256.01] µs; final comparison is
the final candidate. 15 samples, 0.5s measurement target and shared-host clocks
limit external generalization. Payload printed 2113 B for both; no SQL/socket
throughput claim. Tradeoff: stage still serializes each incoming delta once,
clones the growing accumulator and logical/physical overlays, and commit retries
on concurrent revisions; the accumulator replaces stored bytes in projected
state, with on-demand serialization for get_local_data. Residual write overhead
and cold RSS/churn/encrypted-point Phase 6 measurements remain open.

Correctness: strengthened the existing exact-key integration test to inspect
the persisted Entry after set/delete/set in one transaction: ten canonical
operations, latest `a` and resurrected `a.b` values and the `...` tombstone;
then cold/warm reads and physical-order pages. Test negative control replacing
the expected operation count with 1 failed **0 passed; 1 failed**, restored
**1 passed; 0 failed**. Concurrent same-revision disjoint writers' existing
barrier test required one-shot serializer synchronization after the new
per-delta validation; it still proves both overlay revisions and persisted
canonical rows. New fail-on-second-serialization fixture fails commit after a
successful stage and checks unchanged builder, logical/physical overlays,
revision, seal and absent persisted history. Focused projected suite **12 passed;
0 failed**. Actual encrypted 260-row Table cold builds, corruption/identity,
and authenticated socket fixtures remain in the accumulated Nix matrix.

One intermediate gate failed the mixed-height-strategy integration fixture
**1547/1548** in each full backend and **1382/1383** minimal: sealing an
unrelated `init_subtree_parents` after commit's read-scoped get_index blocked
loading per-store height settings, so the independent store inherited timestamp
height. Removed only that new seal check; direct fixture **1 passed; 0 failed**.
Final `nix develop -c nix run .#fix` passed clippy, deadnix, markdownlint,
statix and treefmt; final formatted-source `nix develop -c just nix full` exit
0: in-memory, SQLite, PostgreSQL and service each **1548 tests run: 1548
passed, 5 skipped**; minimal **1383 tests run: 1383 passed, 5 skipped**;
NixOS service and OCI VM integration tests passed. `git diff --check` clean.
These gate fixtures validate actual backend and socket behavior; committed-tip
gate and handoff remain to follow. No push/PR.
