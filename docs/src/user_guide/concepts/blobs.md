# Blob Storage

Blobs are content-addressed bytes stored outside the Entry DAG.
Use them for binary content and embed an optional `BlobRef { cid, size }` or a bare CID in an ordinary Store value.
There is no separate blob Store type.
Committing a reference does not copy the bytes into the DAG or retain them against garbage collection.

## Local byte access

```rust,no_run
# async fn example(instance: &eidetica::Instance) -> eidetica::Result<()> {
let reference = instance.put_blob_ref(b"attachment".to_vec()).await?;
assert_eq!(reference.size(), 10);
assert_eq!(instance.get_blob_ref(&reference).await?, Some(b"attachment".to_vec()));
assert_eq!(instance.get_blob_range(reference.cid(), 2..5).await?, Some(b"tac".to_vec()));
# Ok(())
# }
```

`put_blob` returns only the CID; `put_blob_ref` also returns the byte length.
Identical bytes produce the same raw-codec BLAKE3-256 CIDv1, regardless of backend or physical layout.
`get_blob`, `get_blob_local`, `get_blob_ref` and `get_blob_range` read local storage only, never peers.
For a connected Instance, “local” means the daemon's storage.
A missing blob returns `Ok(None)`; invalid addresses, corrupt bytes and missing backing files return errors instead.
Whole reads verify the hash and ranges verify the requested window against its Bao tree.
Ranges are half-open and clamped to the blob length; empty, reversed and past-EOF ranges return empty bytes for a held blob.
A partial range is not a complete local copy.

Puts are limited to 64 MiB.
A `BlobRef` declaring more is refused before reading; a declared length that disagrees with the stored bytes is also refused.
A CID establishes content identity, not confidentiality or permission to execute its contents.

## Storage and persistence

InMemory keeps blob bytes in memory and includes them in its saved snapshots.
SQLite and PostgreSQL persist blob metadata and Bao outboards in SQL.
SQL backends with a blob directory keep bytes up to 16 KiB inline and larger blobs in content-addressed files.
File publication writes and syncs a temporary file, atomically renames it and syncs the containing directory before publishing its SQL row.
Back up the SQL database and its blob directory together while the backend is stopped.
Opening one persistent backend from multiple Instances is unsupported; use the daemon for shared access.

`Sqlite::open(path)` uses `<path>.blobs`; URL connections store bytes inline unless configured with `with_blob_dir`.
The CLI uses `<data-dir>/blobs` for SQL backends; `--blob-dir` / `EIDETICA_BLOB_DIR` overrides it.
InMemory has no disk tier and ignores that option.
Clearing derived Store-state caches does not delete blobs or Entry data.

## Explicit cache collection

A local put creates unowned cache bytes.
There is no application retention API: storing a reference in a Store does not pin the content.
Trusted backend retention records are internal GC substrate, not another application ownership registry.
GC keeps the union of those backend roots and recently accessed blobs.

```rust,no_run
# async fn example(instance: &eidetica::Instance) -> eidetica::Result<()> {
let report = instance.gc_blobs(eidetica::GcOptions {
    max_total_bytes: Some(32 * 1024 * 1024),
    min_age_ms: 60_000,
}).await?;
println!("{} bytes reclaimed", report.reclaimed_bytes);
# Ok(())
# }
```

`max_total_bytes: None` collects every eligible blob.
Otherwise GC evicts eligible blobs in least-recently-used order until the total is at or below the target, or only protected blobs remain.
The default grace window is 60 seconds since the last put or successful read.
GC, reads, publication and access stamping share a lock so a sweep cannot collect an in-flight operation's bytes.
Reports contain aggregate counts and byte totals, never a hosted CID inventory.
Collection is explicit; it does not scan for or remove orphan files left by interrupted writes/deletes.

## Daemon access

Authenticated Unix-socket clients can put, read whole bytes or ranges, and run GC through the same Instance methods.
These operations are by CID and have no Database scope or per-blob authorization.
The existing authenticated-session requirement remains; no blob inventory or retention mutation is exposed over the socket.
The service frame limit remains 64 MiB, independently of the blob limit.
JSON encoding can make a blob below the byte cap too large for one frame: such a response returns an error without hanging or closing the connection.
Read smaller ranges when a whole response does not fit.
These Instance methods never fetch from peers.
Use the Database read interface below for an explicit network read.

## Verified peer reads

`Database::get_blob_range(cid, range)` checks the caller's Database Read permission, reads local storage first, then asks only that Database's active sync peers.
It tries at most 16 providers and 16 usable addresses per provider, using the registered transports and falling back after address failures.
The provider receives only the CID and byte range, not the requesting Database or Account.
HTTP and Iroh both return Bao-verified ranges; requesting `0..u64::MAX` receives the whole blob within the unchanged 64 MiB limit.
Empty and past-EOF ranges follow the local range semantics.
Neither partial nor whole network reads publish bytes to local storage or establish retention.
An application can explicitly store received whole bytes with `Instance::put_blob`.

```rust,no_run
# async fn example(database: &eidetica::Database, cid: &eidetica::entry::ID) -> eidetica::Result<()> {
let bytes = database.get_blob_range(cid, 4096..8192).await?;
# let _ = bytes;
# Ok(())
# }
```

`Ok(None)` means every queried provider answered absent.
With sync enabled, no eligible providers, unreachable or stalled peers, unsupported transports and invalid streams are not absence: an unsuccessful read returns `SyncError::BlobUnavailable` with per-provider `BlobFetchAttempt` counters.
Without sync, a local miss returns `SyncError::SyncNotEnabled`.
Providers outside the bounded round are not covered by that observation.
A connected Database performs the same read on its daemon and preserves those counters over the socket.
Oversized service responses still fail within the existing 64 MiB frame cap; request smaller ranges when JSON encoding does not fit.

For an explicit peer address, `Sync::fetch_blob_range_from_peer` receives bytes without a Database relationship, and `Sync::query_blob_availability` checks up to `MAX_BLOB_AVAILABILITY_BATCH` (64) named CIDs.
Availability returns one boolean per requested CID in the same order, including duplicates; no other CID is returned.
The answer is advisory, not a lease or a promise that a later read will succeed.
Malformed or oversized requests and responses are errors, never fabricated absence.
Peer serving reads only local bytes: it cannot recurse to another provider, reserve storage, or enumerate hosted content.
Naming a CID needs no peer-side blob or Database authorization; Database and Entry authorization is unchanged.

Blob commands run outside the Database sync command loop, with at most four active outbound blob commands per Instance.
Capacity exhaustion is an error, not absence; a stalled transfer does not hold the sync engine.
Transport connect and read deadlines and bounded response buffers remain in force.
There is no automatic fetching, wanted-set declaration, application retention, provider advertisement or inventory discovery API.
