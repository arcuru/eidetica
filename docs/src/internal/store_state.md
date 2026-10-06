# Store state

Stores define how their CRDT state is represented for reading.
The default `StoreStateModel` caches state in one opaque record: it folds ordered Store deltas with the Store's `CRDT` implementation and stores the `Codec`-encoded result under a reserved key.
This default applies to any Store data type and does not assume `Doc`.
`Database::get_store_state::<S>` validates the registered Store type and returns `S::Data`; `get_doc_store_state` is explicitly DocStore-specific JSON convenience.

## Retained explicit maintenance APIs

The read-scoped `EnsureStoreStateGeneration` request carries the expected Store type and effective projection descriptor; after the ordinary canonical Read gate the server checks `_index` and its explicitly registered plaintext Store codecs. `ServiceServer::register_store::<S>` admits a concrete Store type and its effective projection descriptor before serving; the caller cannot register a codec over the socket or choose its descriptor. DocStore and opaque-row Table are registered by default. With `records_only: true`, maintenance ensures the row generation and returns `Ok`; otherwise it returns the encoded complete state. A matching type and descriptor resolve or build derived state internally and return `StoreState(Vec<u8>)` via the Store's `Codec::encode`; the client uses `Codec::decode`, never a staging token or a generic JSON conversion.
An authenticated read-only user can invoke this maintenance, but cannot invoke the separate Write-gated staging operation.
Unknown codecs and recordless storage report `RecordMaintenanceUnavailable`, which selects a typed, ordered Entry-history fold on the client; a verifiable registered descriptor mismatch, authorization failure or Codec error never does. The fallback resolves Store tips reachable from the Verified main frontier before traversing Store parents, so a later Entry affecting another Store cannot hide earlier deltas. Decoding consumes complete Codec values. Cache and wire decoding remain strict, as does source replay for non-Table Stores; Table source-payload tolerance is described below.
Password-wrapped Stores remain opaque to server maintenance: `_index` does not expose the encrypted wrapped codec, so the server cannot authenticate even a claimed known wrapper descriptor. Any such claim yields `RecordMaintenanceUnavailable` after Read authorization and type identity validation, including a claimed plaintext descriptor; it cannot select a plaintext codec or publish a poisoned generation. An unlocked `PasswordStore<S>::get_state` on a service connection sends the expected wrapper descriptor through the canonical Read gate, then folds authorized Entry deltas after local decryption and `Codec::decode`; it neither receives a staging token nor publishes records. Wrong passwords fail during `open`, and ciphertext/authentication errors during folding propagate rather than becoming capability refusals. The generic remote `get_store_state::<PasswordStore<S>>` cannot supply a password; callers use the unlocked Store handle. Its `projected_get` and `projected_scan_page` accept a projection matching the wrapped Store descriptor, fold read-authorized decrypted history into physical-key order locally, and decode authenticated records through the registered password encryptor. Pages carry transaction-view/revision-bound cursors for local overlay mutations, not a durable snapshot of a changing remote frontier. This is a client-side read-only path, not server maintenance, and callers must supply the wrapped Store's projection. These explicit APIs remain available; normal inner Table/RawTable reads use their Store-owned query plans instead.

## Table query records

Backends persist each Store state as an opaque byte-keyed record set.
Keys use unsigned lexicographic byte order, point reads address one key, and scans use half-open ranges with an exclusive continuation key.
The backend does not parse record keys or values and does not know Store types.

`Table<T, C = SerdeJson>` uses type ID `table:v0.1` and independent
`TableData(LwwMap<String, ByteBuf>)` operation/state data. Its strict DAG-CBOR
Codec preserves opaque row byte strings and delete tombstones; it rejects
alternate encodings and trailing data. No decoder or migration accepts old
`table:v0` histories. Its projection descriptor is
`eidetica/table/rows/opaque:v0.1`, version 1. Exact UTF-8 primary keys (including
empty strings, dots and Unicode) address one row without path normalization or
ancestor conflicts. The row codec `C` alone encodes and decodes application
rows, with `SerdeJson` using direct typed JSON and `RawBytes` preserving any
bytes. Historical reductions and daemon projections never deserialize rows into
Rust `T` or run `C`. A cold generation streams set/delete operations into bounded private
chunks, while typed reads decode only the requested rows. Repeated operations
on a key in one transaction reduce to the final LWW operation and atomically
update the canonical builder and read-your-writes overlays.

LWW-winning configuration selects Table's format and row codec; historical
configuration disagreement alone is not an error. A typed handle with the wrong
codec still fails. A winning row that cannot be decoded is absent from the typed
view, with a warning and no fallback to an older row. Source replay also skips a
whole unreadable Table Entry payload with a warning, before emitting any of its
mutations; this can leave older state visible. Both paths retain original bytes.
The source exception requires concrete `TableData` and the Table or password-Table
projection descriptor. It applies only after successful decryption, not to caches,
staged data, wire responses, authorization, ancestry, I/O or mutation errors.
Warnings omit row keys, payload contents and decoder error text.

`PasswordStore` preserves the wrapped Store's state model but namespaces its
descriptor. For a Table, the cached record key becomes a stable keyed hash of
the logical key and the value becomes an authenticated encrypted envelope;
scans and cursors use physical-key order. For DocStore, YDoc, and other opaque
models, the complete serialized state remains one encrypted record at the
reserved opaque key. See [Encryption](encryption.md) for the exact formats and
trust boundary.

Table handles retain only their name and transaction. `get` deserializes one
row, `scan_page` reads bounded deterministic pages, and `search` collects those
pages only because its public return type is a `Vec`. Local and service-backed
Tables use the same Store-owned point/page plans, also exposed as `query(GetRow(...))`,
`query(ScanRows { ... })` and `query(SearchRows(...))`. Pages inspect at most 128
physical rows, even if the caller asks for more. Cursors bind the transaction,
projection, format witness, source and staged revision. Overlay or remote-frontier
changes invalidate them. Staged puts/deletes compose with committed physical pages,
not a universal whole-state dirty-transaction fallback.

The installed daemon handler copies opaque bytes without an application codec.
Its Shared Derived query representation is separate from explicit projection
namespaces and carries source/key/checksum framing. Encrypted/unknown capability
refusal uses bounded canonical source assistance and private physical record reuse.
Warm encrypted reads never require opaque-state hydration or a full record scan;
cold reconstruction still folds the bounded source. Private publication is inline
and optional. Canonical source validation on the daemon remains history-sensitive.
Only derived payload corruption is repairable; source binding, authorization,
missing keys, authoritative decryption and malformed response errors stay hard.
A RowCodec failure skips that winning row without rebuilding valid opaque records.

A record set has one lifecycle:

- **Derived** record sets materialize historical Entries at fixed Store tips, are immutable after publication, and can be cleared and rebuilt.
- **Authoritative** record sets contain durable current state and are not eligible for cache clearing.
- **Staging** builds are unpublished private state and are invisible to record readers.

Clearing derived state unlinks published record sets and reclaims the generation unlinked by the previous clear. An active reader keeps its view, while a new lookup misses and rebuilds. Authoritative record sets are never selected.

A builder creates private state, writes record chunks, then publishes atomically.
Aborting records a terminal token outcome and removes private records. A failed publication leaves the private build invisible; a caller may correct the error or abort it. An abandoned build remains private until lease-based reclamation. Two builders can derive the same target concurrently. Publication resolves that race to one shared record set and records `Adopted` for the loser.
Format descriptors identify the Store-owned record format and version, so cached state from different formats or historical sources cannot collide.

An explicit ordered staging chunk applies physical `Put` and `Delete` mutations in message order. Deleting a missing key succeeds; deleting a previously staged key removes its row rather than storing a null marker. A later put resurrects it, including across chunks. An empty private namespace can publish a resolvable empty generation. Explicit projection maintenance and private assistance reuse this physical substrate.

Remote record operations use session-scoped read views and backend-owned opaque staging tokens. The latter retain `Active`, `Published`, `Adopted`, `Aborted`, or `Expired` status across socket reconnects and service restarts when the backend is persisted. Chunk sequence and the digest of the last encoded wire chunk are stored with the backend token; an identical immediate retry is acknowledged without replay, while gaps, older retries and conflicting digests fail. SQL stores the token atomically with its namespace, and in-memory snapshots include both. `RemoteConnection::encode_staging_chunk` and `send_staging_chunk` let a caller retain and resend the exact encoded request after an ambiguous response, including on a newly authenticated connection. The high-level `RemoteBackend` keeps the exact encoded unacknowledged chunk (and any remaining pre-encoded chunks in its batch) with the token's sequence, target and original acting/session identities. An ambiguous I/O error includes the token and optional chunk sequence; further staging or publication is refused until the caller supplies a freshly authenticated `RemoteConnection` to `resume_staging`. Recovery checks the database/user-scoped token status, replays only the exact retained bytes against an Active token, resolves Published/Adopted to a fresh view, and never begins a second build or saves login credentials. Unknown, expired and mismatched tokens refuse recovery. Recovery replaces the backend handle's connection only after authorization and successful replay; it does not reconnect by itself. An ambiguous abort can also be resolved by status; a terminal Aborted token cannot be uploaded again.

Each accepted chunk renews a five-minute lease. Expiry stops staging or publication; a sweeper marks an unpublished build `Expired` and removes its private records only after five minutes of additional grace. A terminal outcome is retained for at least 24 hours of client retries plus the five-minute lease and five-minute reclamation grace (24 hours 10 minutes from its transition); reclamation prunes older terminal rows. New opaque tokens carry a UUIDv7 creation timestamp. An unknown token cannot authorize replacement before that creation horizon; after it, a backend-atomic check must find neither a resolved target generation nor a live unexpired build for the same target. Legacy tokens without a timestamp remain ambiguous and cannot use this recovery path. Callers must authorize the target before using recovery; the remote adapter does not automatically invoke it. Publication, abort and reclamation serialize per target; only one terminal outcome can win. Session view expiry does not abort a durable build. Sweeping currently runs on service requests or via the backend reclamation method, not on an independent timer. A backend without persistent storage must persist its in-memory snapshot explicitly to retain state through process loss.

Pages have exclusive continuation keys and encoded-byte bounds; one record that cannot fit fails with `RecordTooLarge`.

Typed record projections now yield an iterator of ordered physical `Put` and
`Delete` mutations from a canonical `D: CRDT` delta, without reconstructing
an Entry from cached records. Cold materialization consumes one
Entry delta and bounded mutation chunks (128 changes or 1 MiB), then publishes
one immutable generation. Explicit legacy history collection still returns `Vec<Entry>`, with finite traversal and encoded-response bounds and explicit size refusal; ordinary query fallback uses bounded canonical source pages.
Typed transaction point reads and physical-order pages resolve a real backend
record view, merge the revisioned local overlay, and reject a page if that
overlay changes while a backend fetch is awaited. On a backend without records,
typed history is folded locally and projected for point reads and scans.
Normal Table queries use this overlay scanner with Store-specific committed fetches;
explicit backend/projection APIs remain available. `Transaction::commit_inner` does
not synthesize Table deltas from cached records.

## Normal document reads

DocStore conveniences and `query(GetValue)`, `query(GetPath)` and `query(GetAll)`
use Store-owned committed-source plans, not projection-generation setup. Nested
staging composes only the relevant top-level value for a point/path read;
`get_all` composes the full Doc, including tombstones. The committed source stays
pinned across concurrent writes. Reserved registry/settings metadata is fixed
JSON folded from canonical history, never a client-private authorization cache.
Encrypted inner Docs validate their protected identity client-side and use the
existing bounded opaque assistance/recovery path under the encrypted outer type.
Inline Derived publication cannot replace a valid read or signed commit result.

The legacy `get_option`, `get_path_option`, `contains_key` and `contains_path`
wrappers cannot report failures and still map them to `None`/`false`. Use fallible
reads for source/auth/key decisions. Fallible read-modify helpers propagate these
failures; only actual missing values or their existing value-type default rule
permit insertion of a default.
