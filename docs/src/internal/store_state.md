# Store state

Stores define how their CRDT state is represented for reading.
The default `StoreStateModel` caches state in one opaque record: it folds ordered Store deltas with the Store's `CRDT` implementation and stores the serialized result under a reserved key.
This default applies to any Store data type and does not assume `Doc`.

Backends persist each Store state as an opaque byte-keyed record set.
Keys use unsigned lexicographic byte order, point reads address one key, and scans use half-open ranges with an exclusive continuation key.
The backend does not parse record keys or values and does not know Store types.

`Table` uses the cached record format named `eidetica/table/rows` version 0.
UTF-8 primary keys are record keys and each JSON row is one record value.
Building historical cached state streams canonical `Doc` Entry deltas into a
private build; it does not reconstruct a whole Table. Table mutations are converted
back into the existing canonical `Doc` delta at historical commit, so Entry
payload and wire semantics are unchanged.

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
Tables use the same point and page operations. Custom backends without cached-record
support keep the existing whole-state behavior.

A record set has one lifecycle:

- **Derived** record sets materialize historical Entries at fixed Store tips, are immutable after publication, and can be cleared and rebuilt.
- **Authoritative** record sets contain durable current state and are not eligible for cache clearing.
- **Staging** builds are unpublished private state and are invisible to record readers.

Clearing derived state unlinks published record sets and reclaims the generation unlinked by the previous clear. An active reader keeps its view, while a new lookup misses and rebuilds. Authoritative record sets are never selected.

A builder creates private state, writes record chunks, then publishes atomically.
Aborting records a terminal token outcome and removes private records. A failed publication leaves the private build invisible; a caller may correct the error or abort it. An abandoned build remains private until lease-based reclamation. Two builders can derive the same target concurrently. Publication resolves that race to one shared record set and records `Adopted` for the loser.
Format descriptors identify the Store-owned record format and version, so cached state from different formats or historical sources cannot collide.

An explicit ordered staging chunk applies physical `Put` and `Delete` mutations in message order. Deleting a missing key succeeds; deleting a previously staged key removes its row rather than storing a null marker. A later put resurrects it, including across chunks. An empty private namespace can publish a resolvable empty generation. The existing Doc-backed Table still uses its collapsed overlay and legacy tombstone validation; the new physical path does not switch Table's format.

Remote record operations use session-scoped read views and backend-owned opaque staging tokens. The latter retain `Active`, `Published`, `Adopted`, `Aborted`, or `Expired` status across socket reconnects and service restarts when the backend is persisted. Chunk sequence and the digest of the last encoded wire chunk are stored with the backend token; an identical immediate retry is acknowledged without replay, while gaps, older retries and conflicting digests fail. SQL stores the token atomically with its namespace, and in-memory snapshots include both. A client must retain exact encoded chunks after ambiguous responses; the high-level remote upload adapter still keeps its sequence cursor only in the current handle and does not yet recover a lost acknowledgement automatically.

Each accepted chunk renews a five-minute lease. Expiry stops staging or publication; a sweeper marks an unpublished build `Expired` and removes its private records only after five minutes of additional grace. It keeps terminal token rows rather than silently forgetting outcomes (no bounded retention window has been implemented yet). Publication, abort and reclamation serialize per target; only one terminal outcome can win. Session view expiry does not abort a durable build. Sweeping currently runs on service requests or via the backend reclamation method, not on an independent timer. A backend without persistent storage must persist its in-memory snapshot explicitly to retain state through process loss.

Pages have exclusive continuation keys and encoded-byte bounds; one record that cannot fit fails with `RecordTooLarge`.
