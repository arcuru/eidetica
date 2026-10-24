# Historical Store-state cache

Historical current-state reads use derived record sets in the Store-state cache.
Immutable Store tips identify the source, and the format descriptor identifies the representation.

The default opaque builder folds Store deltas in canonical historical order, serializes the Store's CRDT type, and publishes one record at the reserved opaque key.
A cold read builds and publishes the record set; a warm read resolves the published record set and reads that record.
The runtime is generic over the Store CRDT type and does not assume `Doc`.

Cached-state construction writes into a private build first.
A failed fold, serialization, private write, or publish leaves no partial published record set.
Published derived record sets are immutable.

Clearing selects only the `Derived` lifecycle, so authoritative records remain byte-for-byte unchanged.
Clearing is two-phase: it unlinks every published derived record set and reclaims the generation unlinked by the previous clear.
An unlinked record set is no longer resolvable, so the next read rebuilds from immutable Entries, while a reader that resolved its view before the clear keeps reading the generation it is walking until the following clear reclaims it.

The legacy SQL `crdt_cache_v2` table and the in-memory LRU are removed. Local and connected reads use Store-state records.

Historical `Table` state uses the `eidetica/table/rows` format, with one
record per logical row keyed by its UTF-8 primary key. Opening a Table handle
reads no rows. Point reads fetch one record, and ordered iteration uses bounded
pages with exclusive continuation while transaction-local changes overlay the
published record set.

`PasswordStore<Table<T>>` uses the namespaced
`eidetica/password/eidetica/table/rows` descriptor. Its derived generations
contain keyed 32-byte physical keys and authenticated encrypted row envelopes;
ordering and exclusive cursors use those physical keys. The history fallback
projects, overlays, and pages in the same order. Other wrapped Store types keep
the encrypted opaque whole-state representation. See [Encryption](encryption.md).

Connected instances use the same cached-state path over the service record
protocol. The daemon binds each request to the authenticated session. It narrows a
shared-scope request to the session user, refuses a foreign scope, and falls
back to shared cached state on a user-scope miss. Encrypted materializations are
client-computed and user-scoped; a warm encrypted Table point read uses point-record
requests without scanning the generation or reconstructing history.

## Retention and verification boundaries

Derived materializations use canonical Snapshot keys within database, Store,
projection and trust-scope namespaces. Verification status is not a cache key:
the same immutable Snapshot has the same materialization before or after a local
status transition. A cached value is never an authorization or verification proof.

The default backend-wide budget is 1024 live derived namespaces and 256 MiB of
record key/value bytes. Publication evicts other least-recently-used namespaces;
a single protected oversized publication may exceed the byte budget until a later
publication can evict it. Live access ticks are monotonic; SQL flushes a batch
when 64 distinct namespaces have pending hits, or during eviction. Repeated hits
on fewer namespaces stay in memory. Eviction also considers hits whose SQL
flushes are still in flight. Reopening SQL retains warm records, but may lose
recent unflushed recency.

Evicted namespaces stop resolving immediately but remain readable through existing
views until the next eviction pass or clear. This is separate from clear-unlinked
namespaces, which eviction leaves alone until the next clear.

These limits do not bound authoritative history/Store records, namespace metadata,
staging data, temporarily retained reader generations, or whole-tree repair memory.
Verified-tip storage is separate from this LRU and proportional to frontier width,
not history length. Repair/demotion update that frontier, not immutable cached values;
the offline full trust reset also clears disposable materializations.
