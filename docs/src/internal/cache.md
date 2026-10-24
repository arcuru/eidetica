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

Derived materializations use domain-separated BLAKE3-256 digests of complete
canonical Snapshots, within separate database, Store, lifecycle, projection and
trust-scope namespaces. Frontier width does not enlarge the SQL unique key.
Old full-frontier keys are disposable cold rows, not relabelled digests.
Verification status is not a key: the same immutable Snapshot has the same
materialization before or after a local status transition. A cached value is
never an authorization or verification proof.

The defaults are **1024 live derived namespaces**, with **268435456 bytes
(256 MiB)** of live key/value payload for InMemory and in-memory SQLite, or
**4294967296 bytes (4 GiB)** for persistent SQL. Constructors accept a
`DerivedCachePolicy`; CLI backend options are `--cache-bytes` and
`--cache-namespaces` (`EIDETICA_CACHE_BYTES`, `EIDETICA_CACHE_NAMESPACES`).
The owning daemon selects its policy before opening storage; clients cannot
raise it. A zero bound disables retention. A candidate larger than the byte
budget is not published and does not evict useful state. All derived state,
including the current frontier, is evictable.

SQL publication, sizing, admission and eviction commit in one transaction.
A successful commit has no later fallible maintenance step. A confirmed
admission rollback can report `StoreStateNotRetained`; an uncertain commit or
rollback remains an error, never a cache miss. Same-target builders adopt a
winner only after validating identity under the publication locks.

Live access ticks are monotonic (saturating at the SQL signed-integer maximum).
SQL flushes after 64 distinct dirty namespaces, or inside admission, in chunks
of at most 256. Batches copy pending ticks and acknowledge only committed ticks;
failed/cancelled batches and newer hits survive. Eviction compares the maximum
of live and durable ticks, including in-flight batches. Optional recency-flush
failure does not fail a successful data read. Authoritative reads do not enter
derived recency. Crashes may lose recent unflushed order, not cached values.

Evicted namespaces stop resolving immediately but remain readable through existing
views until the next eviction pass or clear. Clear-unlinked namespaces instead
remain readable until the following clear. Exclusive SQL startup reclaims obsolete
retired generations and abandoned builds, seeds the clock, and enforces a lowered
policy before exposing the backend. An inability to establish the cap visibly refuses open
rather than exposing an over-budget cache.

Row, opaque and authorization-floor callers fall back to captured history only
for explicit optional-cache refusal, unsupported storage or invalidated views.
After one stale-view retry, a row read uses history; a partial page is discarded
and restarted from its original cursor. Opaque results already computed are
returned without retention, and disabled admission skips cache encoding/upload.
Authentication, proof, decryption, corruption and history/read errors still fail.
No cache GC removes authoritative Entries or records.

These are logical live-payload limits, not RSS, physical disk or file-high-water
quotas. Metadata, staging, two separately retained reader generations, database
workspace and authoritative history have separate costs. This implementation
still folds whole history and may build whole logical/physical row maps on a
cold read or bypass. It is not incremental materialization, bounded-history
streaming, or a whole-SDK memory bound. Existing service frame and single-record
limits remain; old SDKs can surface quota refusal as an error rather than
transparently bypassing it. No deployment/support-version promise follows from
these local storage changes.
