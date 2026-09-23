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

Historical `Table` state uses the `eidetica/table/rows/canonical-json:v0` format, with one
record per logical row keyed by its UTF-8 primary key. Opening a Table handle
reads no rows. Point reads fetch one record, and ordered iteration uses bounded
pages with exclusive continuation while transaction-local changes overlay the
published record set.

`PasswordStore<Table<T>>` uses the namespaced
`eidetica/password/eidetica/table/rows/canonical-json:v0` descriptor. Its derived generations
contain keyed 32-byte physical keys and authenticated encrypted row envelopes;
ordering and exclusive cursors use those physical keys. The history fallback
projects, overlays, and pages in the same order. Other wrapped Store types keep
the encrypted opaque whole-state representation. See [Encryption](encryption.md).

Connected instances use the same cached-state path over the service record
protocol. The daemon binds each request to the authenticated session. It narrows a
shared-scope request to the session user, refuses a foreign scope, and falls
back to shared cached state on a user-scope miss. Known plaintext codecs may be
materialized by read-scoped registered server maintenance. Unknown codecs and
recordless backends use typed ordered Entry-history fallback. Password-wrapped
codecs cannot be verified by server maintenance: an unlocked client folds its
read-authorized decrypted history locally when maintenance is unavailable.
Encrypted materializations uploaded by a client are user-scoped; a warm encrypted
Table point read can use point-record requests without scanning the generation
or reconstructing history. Neither that warm path nor the read-only fallback
implies server-side encrypted cold materialization.
