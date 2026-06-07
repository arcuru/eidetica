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

The recommended way to open an Instance is by URL — `Instance::connect(url)` and `Instance::connect_or_create(url, NewUser::…)` dispatch to the right backend based on the URL scheme:

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

<!-- Code block ignored: Requires async runtime context -->

```rust,ignore
use eidetica::{Instance, NewUser};

// Recommended: by URL.
let (instance, _) = Instance::connect_or_create(
    "sqlite://./my_data.db",
    NewUser::passwordless("alice"),
).await?;

// Escape-hatch: build the backend manually for custom configuration.
use eidetica::backend::database::Sqlite;
let backend = Sqlite::open("my_data.db").await?;
let instance = Instance::open_backend(Box::new(backend)).await?;
```

### PostgreSQL

PostgreSQL provides production-grade persistent storage for larger deployments. Enabled with the `postgres` feature.

<!-- Code block ignored: Requires PostgreSQL server -->

```rust,ignore
use eidetica::{Instance, NewUser};

// By URL — passed through to sqlx unchanged, so any sqlx-accepted query
// string works (sslmode, application_name, etc.).
let (instance, _) = Instance::connect_or_create(
    "postgres://user:pass@localhost/mydb?sslmode=require",
    NewUser::passwordless("alice"),
).await?;
```

### InMemory

The in-process backend is intended for tests, examples, and embedded apps that don't need durable storage. It can optionally serialize to and load from a JSON file via the `memory:///abs/path` URL form.

**Prefer sqlite's in-memory mode when the `sqlite` feature is enabled.** The `sqlite:file::memory:?cache=shared` URL exercises the same backend used in production, so integration tests stay closer to deployed behaviour. Reach for `memory://` only when you've built without the `sqlite` feature or specifically want the JSON-snapshot story.

**Not for production.** Prefer `sqlite://` (file-backed) or `postgres://` for any deployed workload. Specifically:

- The JSON snapshot format is unstable (`_v: 0`); compatibility may break between versions.
- The snapshot serializes the device signing key **in plaintext**. For `memory:///path.json`, that key ends up on disk in cleartext — fine for tests, unsafe for production.
- Every `flush()` rewrites the full backend state; there's no incremental persistence, MVCC, or multi-process access.

The ephemeral sqlite form uses `Instance::connect_or_create` like every other URL:

<!-- Code block ignored: Requires async runtime context -->

```rust,ignore
use eidetica::{Instance, NewUser};

let (instance, _) = Instance::connect_or_create(
    "sqlite:file::memory:?cache=shared",
    NewUser::passwordless("alice"),
).await?;
```

The `memory://` URL forms remain useful for tests and short-lived embedded scenarios:

<!-- Code block ignored: Requires file system access during testing -->

```rust,ignore
use eidetica::{Instance, NewUser};

// Ephemeral (state dies with the process).
let (instance, _) =
    Instance::connect_or_create("memory://", NewUser::passwordless("alice")).await?;

// With a JSON snapshot — loaded on construction, written by flush() or
// (best-effort) by Drop. The snapshot path must be absolute.
let (instance, _) = Instance::connect_or_create(
    "memory:///var/lib/myapp/snap.json",
    NewUser::passwordless("alice"),
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
