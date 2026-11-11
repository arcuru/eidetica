# Database Storage

Database storage implementations in Eidetica define how and where data is physically stored.

## The Database Abstraction

The Database trait abstracts the underlying storage mechanism for Eidetica entries. This separation of concerns allows the core database logic to remain independent of the specific storage details.

Key responsibilities of a Database:

- Storing and retrieving entries by their unique IDs
- Tracking relationships between entries
- Calculating tips (latest entries) for databases and stores
- Managing the graph-like structure of entry history

## Selecting a Backend via URL

The recommended way to open an Instance is by URL — `Instance::connect(url)` and `Instance::connect_or_create(url, NewAccount::…)` dispatch to the right backend based on the URL scheme:

| URL form                            | Backend                                                  | Feature flag       |
| ----------------------------------- | -------------------------------------------------------- | ------------------ |
| `sqlite://./my_data.db`             | Embedded SQLite (passed through to `sqlx::sqlite`)       | `sqlite`           |
| `sqlite:file::memory:?cache=shared` | Embedded SQLite, in-memory (see note)                    | `sqlite`           |
| `postgres://user:pwd@host/db`       | Embedded PostgreSQL (passed through to `sqlx::postgres`) | `postgres`         |
| `unix:///run/eidetica/service.sock` | Thin client to a running [daemon](../service.md)         | `service` (unix)   |
| `memory://`                         | Ephemeral in-process backend (tests/dev only)            | (always available) |
| `memory:///abs/path/snap.json`      | In-process backend with a JSON snapshot file (tests/dev) | (always available) |

The in-memory sqlite URL requires sqlx's single-colon URI form (`sqlite:` rather than `sqlite://`) due to limitations in sqlx. The `file:` prefix + `cache=shared` are what make the database visible across the connection pool. Without them each pooled connection gets its own isolated database.

Backends whose Cargo feature isn't compiled in return `InstanceError::BackendUnavailable { scheme, missing_feature }` at runtime — no link-time surprise.

For escape-hatch construction (custom sqlx pool config, custom clock, etc.) the per-backend constructors below remain `pub`; see [`Instance::open_backend`](https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.open_backend), [`connect_or_create_backend`](https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.connect_or_create_backend), and [`create_backend`](https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.create_backend) for the URL-less entry points.

## Available Backend Implementations

### SQLite

SQLite is the default and recommended backend. It provides embedded persistent storage with excellent performance. Enabled with the `sqlite` feature.

A `SqlxBackend` exclusively owns its persistent storage namespace until the backend is dropped. For SQLite, that namespace is the canonical database file. For PostgreSQL, it is the database and active schema. A competing direct open fails immediately with `BackendError::StorageAlreadyOwned`, including another backend in the same process. Clients that need to share the same instance should connect through one Eidetica service daemon instead.

SQLite uses a cooperative advisory lock on a stable `.eidetica-owner` sidecar next to the canonical database path. The backend retains the nonblocking operating-system lock for its lifetime, and leaves the sidecar in place when released; the file's existence alone does not mean the database is owned. Relative, absolute, and existing symlink aliases that resolve to the same canonical path therefore contend; a final-component symlink whose target does not exist is rejected. This protocol requires a local filesystem with reliable advisory locks. Applications must not use hard-link aliases, rename or replace a live database, or delete or replace its sidecar. Unrelated SQLite tools do not participate and can bypass it.

PostgreSQL stores an ownership token in the active schema: every pooled session holds a shared namespace lock and validates that token, while takeover requires the exclusive namespace lock. A surviving old session therefore blocks takeover, and a stale pool cannot reconnect after ownership changes. The operating system or database server releases the locks when their sessions exit or crash. Dropping a PostgreSQL backend closes its sessions without waiting for the server, so an immediate reopen of the same namespace can still briefly fail with `BackendError::StorageAlreadyOwned`.

Both SQL backends use an internal integer schema counter, independent of releases and the InMemory file counter. Mainline schema `0` migrates directly to current `4`; current opens reuse persisted Verified tips and local-account records. The experimental `1` prefix format is unsupported. See the [migration matrix](../../design/verification.md#persistence-restart-and-forward-migration).
Both SQL backends reject a stored schema version newer than the supported version before creating schema tables or indexes or running migrations.
Refusal leaves the version marker, Entries, instance metadata and Store-state records unchanged.
The error names both versions.
This is not a read-only open: SQLite may still set WAL mode and open its ownership sidecar, and PostgreSQL still performs ownership bookkeeping before checking the schema version.

<!-- Code block ignored: Requires async runtime context -->

```rust,ignore
use eidetica::{Instance, NewAccount};

// Recommended: by URL.
let (instance, _) = Instance::connect_or_create(
    "sqlite://./my_data.db",
    NewAccount::passwordless("alice"),
).await?;

// Escape-hatch: build the backend manually for custom configuration.
use eidetica::backend::database::Sqlite;
let backend = Sqlite::open("my_data.db").await?;
let instance = Instance::open_backend(Box::new(backend)).await?;
```

#### Disposable SQLite cache

SQLite keeps immutable Entries, verification state, instance metadata/secrets,
local account credentials and Authoritative Store state in the primary history
file. Only reconstructible Derived records and their unpublished builds use a
second SQLite database. PostgreSQL keeps its own cache in its tables unless a
[separate cache engine](#separate-cache-engine) is selected.

The CLI resolves `--cache-dir` / `EIDETICA_CACHE_DIR` to an application directory:

| Platform           | Default directory                                                                    |
| ------------------ | ------------------------------------------------------------------------------------ |
| Linux / other Unix | `$XDG_CACHE_HOME/eidetica`, or `$HOME/.cache/eidetica` when XDG is unset or relative |
| macOS              | `$HOME/Library/Caches/eidetica`                                                      |
| Windows            | `%LOCALAPPDATA%/eidetica`                                                            |
| Packaged container | `/cache` (explicit environment setting)                                              |

Initialized instances use `<hex-encoded-prefixed-device-public-key>.sqlite`
beneath that directory, with the normal SQLite sidecars and a stable
`.eidetica-owner` sidecar. Independent instances have different existing device
keys and therefore different files under a shared root. Copies retaining the
same device key are the same instance for this purpose: use different cache
roots if running such copies independently. No new storage identifier is saved.

First initialization has no complete instance identity yet. It uses an in-memory
Derived cache until the backend is reopened, then selects the file from complete
persisted metadata. It never saves placeholder metadata to obtain a filename.
The host creates the designated application directory as needed, but does not
change existing ownership/group permissions. That directory and its files must
be writable by the service account. Cache contents may be sensitive, including
plaintext unencrypted Store values; provision access accordingly. No private
permission mode or ownership changes are imposed by the backend.

Library constructors do not inspect host environment variables. `Sqlite::open`
and SQLite Instance URLs default to `<history-filename>.cache` adjacent to history.
`Sqlite::open_with_cache_path` accepts an explicit low-level filename and policy;
`SqlxBackend::open_sqlite_with_cache_dir` accepts an existing directory and policy
with the instance-key/bootstrap behavior above. In-memory SQLite keeps both pools
in memory and refuses a persistent cache location.

The canonical Snapshot digest plus database, Store, projection/version and trust
scope identifies a cache entry. Historical Snapshots remain reusable across
unrelated writes and restarts; there is no global history freshness token.
Verification can change the selected Snapshot, not the hash of an unchanged
Snapshot. Cache storage errors propagate rather than silently becoming misses;
an unrelated, corrupt or unsupported file is refused, never erased on open.

##### Backup and offline recreation

Back up durable history using SQLite's backup API, or stop every owner/reader
and take a consistent primary image including any outstanding WAL. Cache files
are optional backups. Restoring history alone can reuse matching immutable
Snapshot caches; other keys remain separate and cannot stand in for the selected
Snapshot. The cache is not an authorization or verification proof.

To reclaim physical cache space, stop the service and every direct user first.
Remove only the selected instance's `.sqlite` (or explicit low-level cache file)
and its `-wal`, `-shm`, and `-journal` sidecars. Keep the stable ownership sidecar.
Reopening recreates C and rebuilds values on demand without losing history,
credentials or Authoritative state. Never unlink, replace or copy a live SQLite
image, its sidecars or its ownership lock. Do not remove an entire shared XDG root.
Online `clear_derived_store_state` instead preserves a generation for live readers;
its byte limit is live payload, not a physical filesystem quota. WAL/NORMAL
synchronous defaults are unchanged for both files.

Mainline H remains schema **4**, including `local_accounts`. Supported v0/v2/v3
upgrades retain their existing migration/verification rules; SQLite discards
legacy single-file Derived rows on open while retaining Authoritative records.
No extra H schema bump is required. The unreleased `history_identity` H layout
and `cache_binding` C format from an earlier candidate are unsupported and refused,
not silently converted. Preserve those files and use their matching candidate
binary for recovery; this is not approval to migrate live candidate data.

#### Separate cache engine

The Derived cache can use a different engine from primary storage. `SplitBackend`
composes any primary backend with any `DerivedCacheStorage`: `SqliteCache` (a
file or in-memory SQLite database holding only Derived state) or `InMemory`.
PostgreSQL, SQLite and InMemory primaries can therefore use a SQLite or
in-memory cache, and a future engine needs only its own implementation of the
one trait, not a pair-specific integration.

```rust,ignore
use eidetica::backend::{DerivedCachePolicy, SplitBackend};
use eidetica::backend::database::{Postgres, SqliteCache};

let off = DerivedCachePolicy { max_namespaces: 0, max_bytes: 0 };
let primary = Postgres::connect_with_cache_policy(url, off).await?;
let metadata = primary.get_instance_metadata().await?;
let cache = SqliteCache::open_for_instance(cache_dir, metadata.as_ref(),
    DerivedCachePolicy::persistent()).await?;
let backend = SplitBackend::new(Box::new(primary), Box::new(cache));
let instance = Instance::open_backend(Box::new(backend)).await?;
```

Entries, verification, instance metadata/secrets, local credentials and
Authoritative Store state stay in the primary. Only Derived records and their
builds use the cache, with the cache engine's own dialect, schema, startup
validation, publication, bounds/eviction and ownership. Opaque record views and
staging tokens carry their owner and are refused by the other storage. Cache
bounds come from the cache; open a split primary with retention disabled (as
above, or `Sqlite::open_with_memory_cache` for SQLite history), which also
reclaims Derived state it kept under an earlier configuration. Offline trust
reset discards the cache, then resets the primary; a failure can leave a cold
cache, never a reset with old cached state. Other caches holding the same
instance (for example on another host sharing a PostgreSQL primary) are not
reached: stop and recreate them offline before trusting reads there.

Instance isolation follows the SQLite cache rule above. `open_for_instance`
names a file by the instance's existing device key and uses memory before first
initialization completes; `SqliteCache::open` takes an explicit filename, which
must belong to exactly one instance and is exclusively owned while open.
A persistent cache requires a persistent primary: the host refuses
`--backend inmemory --cache-backend sqlite`, and library callers must not pair
a persistent cache with ephemeral primary storage. An ephemeral primary uses an
in-memory cache. The engine shown by `as_any` (and the health endpoint) is the
primary's.

### PostgreSQL

PostgreSQL provides production-grade persistent storage for larger deployments. Enabled with the `postgres` feature.

<!-- Code block ignored: Requires PostgreSQL server -->

```rust,ignore
use eidetica::{Instance, NewAccount};

// By URL — passed through to sqlx unchanged, so any sqlx-accepted query
// string works (sslmode, application_name, etc.).
let (instance, _) = Instance::connect_or_create(
    "postgres://user:pass@localhost/mydb?sslmode=require",
    NewAccount::passwordless("alice"),
).await?;
```

### InMemory

The in-process backend is intended for tests, examples, and embedded apps that don't need durable storage. It can optionally serialize to and load from a JSON file via the `memory:///abs/path` URL form.

**Prefer sqlite's in-memory mode when the `sqlite` feature is enabled.** The `sqlite:file::memory:?cache=shared` URL exercises the same backend used in production, so integration tests stay closer to deployed behaviour. Reach for `memory://` only when you've built without the `sqlite` feature or specifically want the JSON-snapshot story.

**Not for production.** Prefer `sqlite://` (file-backed) or `postgres://` for any deployed workload. Specifically:

- JSON snapshots use internal integer format counter `_v` (currently `3`), not a release version or stability promise. Mainline unversioned/`0` files reconstruct Verified tips on load; version `2` loads without history repair, while experimental `1` and future formats are refused. See the [supported migration matrix](../../design/verification.md#persistence-restart-and-forward-migration).
- The snapshot serializes the device signing key **in plaintext**. For `memory:///path.json`, that key ends up on disk in cleartext — fine for tests, unsafe for production.
- Every `flush()` rewrites the full backend state; there's no incremental persistence, MVCC, or multi-process access.

The ephemeral sqlite form uses `Instance::connect_or_create` like every other URL:

<!-- Code block ignored: Requires async runtime context -->

```rust,ignore
use eidetica::{Instance, NewAccount};

let (instance, _) = Instance::connect_or_create(
    "sqlite:file::memory:?cache=shared",
    NewAccount::passwordless("alice"),
).await?;
```

The `memory://` URL forms remain useful for tests and short-lived embedded scenarios:

<!-- Code block ignored: Requires file system access during testing -->

```rust,ignore
use eidetica::{Instance, NewAccount};

// Ephemeral (state dies with the process).
let (instance, _) =
    Instance::connect_or_create("memory://", NewAccount::passwordless("alice")).await?;

// With a JSON snapshot — loaded on construction, written by flush() or
// (best-effort) by Drop. The snapshot path must be absolute.
let (instance, _) = Instance::connect_or_create(
    "memory:///var/lib/myapp/snap.json",
    NewAccount::passwordless("alice"),
).await?;
// ... do work ...
instance.flush()?; // checkpoint the snapshot to disk atomically; instance keeps running
```

`Instance::flush()` is the canonical way to checkpoint an in-memory snapshot. It's reentrant, sync, and takes `&self`, so call it as often as you like — the snapshot path stays armed and the instance keeps working after every flush. The signature is sync because the write itself is sync (`std::fs::write` + atomic rename); from a tokio task it briefly blocks the worker, negligible for small snapshots. The `Drop` impl falls back to a best-effort save on the last handle (errors are logged via `tracing::error!`, not surfaced) — apps that care about snapshot durability should call `flush()` at checkpoints and inspect the `Result`.

### Remote (Service Daemon)

A `unix://` URL connects to an Eidetica [service daemon](../service.md). All storage operations are forwarded as RPCs to the daemon, which holds the actual backend. This enables multiple client processes to share the same storage.

<!-- Code block ignored: Requires a running daemon -->

```rust,ignore
use eidetica::Instance;

let instance = Instance::connect("unix:///run/eidetica/service.sock").await?;
// Or, with env / default resolution:
let instance = Instance::connect(eidetica::service::default_socket_url()).await?;
```

The `service` feature must be enabled (included in the default `full` feature set). `connect_or_create` against a `unix://` URL degrades to `connect` — the daemon owns its own initialisation. See [Service (Daemon) Mode](../service.md) for setup details.

## Database Trait Responsibilities

The `Database` trait (`eidetica::backend::Database`) defines the core interface required for storage. Beyond simple `get` and `put` for entries, it includes methods crucial for navigating the database's history and structure:

- `snapshot(tree_id)`: Finds the latest entries in a specific `Database`.
- `store_snapshot(tree_id, subtree_name)`: Finds the latest entries _for a specific `Store`_ within a `Database`.
- `all_roots()`: Finds all top-level `Database` roots stored in the database.
- `get_tree(tree_id)` / `get_subtree(...)`: Retrieve all entries for a database/store, typically sorted topologically (required for some history operations, potentially expensive).

Implementing these methods efficiently often requires the database to understand the DAG structure, making the database more than just a simple key-value store.

## Database Performance Considerations

The Database implementation significantly impacts database performance:

- **Entry Retrieval**: How quickly entries can be accessed by ID
- **Graph Traversal**: Efficiency of history traversal and tip calculation
- **Memory Usage**: How entries are stored and whether they're kept in memory
- **Concurrency**: How concurrent operations are handled
