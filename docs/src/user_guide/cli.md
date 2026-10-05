# CLI Reference

The `eidetica` binary provides a server and management commands for inspecting and operating on Eidetica instances.

## Commands

### `serve` (default)

Starts the Eidetica server with HTTP and sync endpoints.

Running `eidetica` with no subcommand is equivalent to `eidetica serve`. This default is likely to change in the future. Do not run `serve` alongside `daemon` on the same SQLite backend.

```bash
eidetica serve [OPTIONS]
```

| Option           | Short | Default     | Env Var                 | Description                                                     |
| ---------------- | ----- | ----------- | ----------------------- | --------------------------------------------------------------- |
| `--port`         | `-p`  | `5942`      | `EIDETICA_PORT`         | Port to listen on                                               |
| `--host`         |       | `0.0.0.0`   | `EIDETICA_HOST`         | Bind address                                                    |
| `--backend`      | `-b`  | `sqlite`    | `EIDETICA_BACKEND`      | Storage backend (`sqlite`, `postgres`, `inmemory`)              |
| `--data-dir`     | `-d`  | current dir | `EIDETICA_DATA_DIR`     | Data directory for storage files                                |
| `--postgres-url` |       | —           | `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL (required when backend is `postgres`) |

### `health`

Checks the health of a running Eidetica server by querying its `/health` endpoint.

```bash
eidetica health [URL] [OPTIONS]
```

| Argument/Option | Short | Default                 | Description                                              |
| --------------- | ----- | ----------------------- | -------------------------------------------------------- |
| `URL`           |       | `http://127.0.0.1:5942` | URL of the server to check (appends `/health` if needed) |
| `--timeout`     | `-t`  | `5`                     | Timeout in seconds                                       |

Both `http://` and `https://` URLs are supported. If the URL doesn't already end with `/health`, it is appended automatically.

Exits with code 0 on success, code 1 on failure.

### `info`

Displays instance information: device ID, storage backend, user count, and database count.

```bash
eidetica info [OPTIONS]
```

| Option           | Short | Default     | Env Var                 | Description                      |
| ---------------- | ----- | ----------- | ----------------------- | -------------------------------- |
| `--backend`      | `-b`  | `sqlite`    | `EIDETICA_BACKEND`      | Storage backend                  |
| `--data-dir`     | `-d`  | current dir | `EIDETICA_DATA_DIR`     | Data directory for storage files |
| `--postgres-url` |       | —           | `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL        |

Example output:

```text
Device ID:   a1b2c3d4-...
Backend:     sqlite (./eidetica.db)
Users:       2
Databases:   5
```

### `daemon init`

Initialises a fresh Eidetica instance on the chosen backend with an initial admin user. The first user created on an instance is automatically granted Admin on the system databases. Fails if the backend already has an instance on it.

```bash
eidetica daemon [BACKEND OPTIONS] init --username <NAME> [--password <PASS> | --passwordless]
```

| Option           | Default | Env Var                   | Description                                           |
| ---------------- | ------- | ------------------------- | ----------------------------------------------------- |
| `--username`     | —       | —                         | **Required.** Initial admin username. No default.     |
| `--password`     | —       | `EIDETICA_ADMIN_PASSWORD` | Optional. Prompted twice on stdin if not provided.    |
| `--passwordless` | off     | —                         | Skip the password (mutually exclusive with the flag). |

`--username` has no default: operators must spell it out so no static credential ships by accident. `--passwordless` is intentionally a separate opt-in (rather than just leaving `--password` unset) — pick it only for embedded or single-user development; production deployments should set a password.

Examples:

```bash
# Interactive password prompt:
eidetica daemon --data-dir /var/lib/eidetica init --username ops

# Non-interactive (e.g. CI provisioning):
EIDETICA_ADMIN_PASSWORD=… eidetica daemon --data-dir /var/lib/eidetica init --username ops

# Embedded / single-user dev workflow:
eidetica daemon --data-dir ~/.local/share/eidetica init --username me --passwordless
```

Backend options (`--backend`, `--data-dir`, `--postgres-url`) go before the `init` subcommand and are shared with `daemon` (see below).

### `daemon`

Runs the Eidetica service daemon against an already-initialised backend. Fails with a pointer at `daemon init` if the backend hasn't been initialised yet. Multiple client processes can connect to the running daemon over the Unix socket to share the same backend storage.

```bash
eidetica daemon [OPTIONS]
```

| Option             | Short | Default       | Env Var                   | Description                                                     |
| ------------------ | ----- | ------------- | ------------------------- | --------------------------------------------------------------- |
| `--dashboard`      |       | off           | `EIDETICA_DASHBOARD`      | Enable web dashboard (never service RPC)                        |
| `--dashboard-host` |       | `127.0.0.1`   | `EIDETICA_DASHBOARD_HOST` | Dashboard bind address (only with `--dashboard`)                |
| `--dashboard-port` |       | `5942`        | `EIDETICA_DASHBOARD_PORT` | Dashboard port (only with `--dashboard`)                        |
| `--socket`         | `-s`  | auto-detected | `EIDETICA_SOCKET`         | Unix socket path (see [Service Mode](service.md) for defaults)  |
| `--backend`        | `-b`  | `sqlite`      | `EIDETICA_BACKEND`        | Storage backend (`sqlite`, `postgres`, `inmemory`)              |
| `--data-dir`       | `-d`  | current dir   | `EIDETICA_DATA_DIR`       | Data directory for storage files                                |
| `--postgres-url`   |       | —             | `EIDETICA_POSTGRES_URL`   | PostgreSQL connection URL (required when backend is `postgres`) |

The daemon runs until interrupted with SIGINT or SIGTERM. Clients connect using `Instance::connect("unix://...")`. See [Service (Daemon) Mode](service.md) for full documentation.

### `db list`

Lists all user-created databases with their root IDs and tip counts. System databases are excluded.

```bash
eidetica db list [OPTIONS]
```

| Option           | Short | Default     | Env Var                 | Description                      |
| ---------------- | ----- | ----------- | ----------------------- | -------------------------------- |
| `--backend`      | `-b`  | `sqlite`    | `EIDETICA_BACKEND`      | Storage backend                  |
| `--data-dir`     | `-d`  | current dir | `EIDETICA_DATA_DIR`     | Data directory for storage files |
| `--postgres-url` |       | —           | `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL        |

Example output:

```text
ROOT ID         TIPS
abc123def456    5
xyz789uvw012    2
```

## Global Flags

| Flag     | Description                                          |
| -------- | ---------------------------------------------------- |
| `--json` | Output in JSON format instead of human-readable text |

The `--json` flag works with `info` and `db list`.

## Storage Backends

| Backend    | Description                     | Storage Location                  |
| ---------- | ------------------------------- | --------------------------------- |
| `sqlite`   | SQLite database (default)       | `eidetica.db` in data directory   |
| `postgres` | PostgreSQL database             | Specified by `--postgres-url`     |
| `inmemory` | In-memory with JSON persistence | `eidetica.json` in data directory |

## Environment Variables

| Variable                | Description                                        | Default           |
| ----------------------- | -------------------------------------------------- | ----------------- |
| `EIDETICA_SOCKET`       | Unix socket path for daemon mode (`daemon`)        | auto-detected     |
| `EIDETICA_PORT`         | Port for the HTTP server (`serve`)                 | `5942`            |
| `EIDETICA_HOST`         | Bind address (`serve`)                             | `0.0.0.0`         |
| `EIDETICA_BACKEND`      | Storage backend (`sqlite`, `postgres`, `inmemory`) | `sqlite`          |
| `EIDETICA_DATA_DIR`     | Directory for database and data files              | current directory |
| `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL                          | —                 |

Command-line flags take precedence over environment variables.

## Examples

```bash
# Start server with defaults (sqlite backend, port 5942)
eidetica

# Start with PostgreSQL backend on a custom port
eidetica serve --port 8080 --backend postgres \
  --postgres-url "postgresql://user:pass@host/db"

# Check health of a running server
eidetica health

# Show instance info as JSON
eidetica info --json

# List databases from a specific data directory
eidetica db list --data-dir /var/lib/eidetica

# Start a daemon for shared multi-process access
install -d -m 0700 "$XDG_RUNTIME_DIR/eidetica"
eidetica daemon --socket "$XDG_RUNTIME_DIR/eidetica/service.sock"
```

### `db reset-local-verification` (offline trust reset)

**Before upgrading an existing instance to new delegated-authorization verification
rules**, stop the daemon/server and all writers and readers, back up the database,
then run the reset with the same backend configuration as the instance:

```bash
eidetica db reset-local-verification --backend sqlite --data-dir /var/lib/eidetica --confirm
# Or: --backend inmemory --data-dir <directory containing eidetica.json>
# Or: --backend postgres --postgres-url <instance connection URL>
```

The command does not run during startup or schema migration. **Skipping it can
leave old `Verified` labels trusted under the new rules.** It resets _all_
local statuses (`Verified` and `Failed` included) to `Unverified`, discards
derived and incomplete Store-state namespaces, and keeps every immutable Entry
and authoritative Store state. It does not verify entries itself. Start the new
version only after the command succeeds, and follow the
[re-verification procedure below](#re-verification-after-reset-or-incomplete-proof)
before relying on reads. Access/sync can attempt verification but does not promise
to acquire or settle delegated dependencies automatically. Verification is
prefix-closed: until ancestors verify, descendants remain `Unverified`.
If any reset step fails, leave the service stopped, diagnose and retry the
command; do not trust the old status labels. An in-memory persistence file
must exist and parse successfully; a missing or corrupt file is never treated
as an empty instance by this command.

SQLite and PostgreSQL commit status and cache changes in one transaction.
The persisted in-memory backend writes a replacement JSON snapshot by atomic
rename on POSIX; if writing fails before rename, the old file remains in
place. Run the command with no other process or API user of the backend:
the normal online `clear_derived_store_state` retains a generation for live
readers, whereas the trust reset deliberately drops it. The in-memory JSON
persistence path has no cross-process ownership lock. On platforms without
atomic replacement rename, take an offline backup and verify the reopened
file before starting the service.

### Re-verification after reset or incomplete proof

There is currently **no `db verify` CLI subcommand** and the reset command does
not run verification. Use a local SDK maintenance process with the same backend,
without a concurrent daemon or other backend owner. `Database::verify()` is a
node-local operation, not a client RPC to a connected service instance.

1. After a successful reset, open the backend with the new version. Keep normal
   consumers stopped until you have inspected the rebuilt trust state.
2. Make the immutable main/settings histories and claimed/configured delegated
   snapshots available through ordinary replication or the application's ingest
   API. Reset preserves any Entries already present; it does not fetch missing ones.
3. Explicitly verify delegated databases first, starting with their own
   dependencies, then retry the primary database. Repeat only after a known
   dependency arrives or verifies; running the primary pass alone cannot settle
   a present-but-`Unverified` delegated proof.
4. Inspect the returned `VerifyReport` and the default Verified-frontier reads.
   `failed` and `still_unverified` describe entries considered in that pass,
   not an inventory of all stored failures. A later empty report does not prove
   the whole history is healthy. Record unresolved dependency IDs and rejected
   branches rather than declaring recovery complete from a zero-error return.

For example, on an already opened **local** `Instance`, with the relevant roots
ordered dependency-first:

```rust,no_run
# use eidetica::{Database, Instance, ID};
# async fn reverify(instance: &Instance, roots_dependency_first: &[ID]) -> eidetica::Result<()> {
for root in roots_dependency_first {
    let database = Database::open(instance, root).await?;
    let report = database.verify().await?;
    println!("{root}: {report:?}");
    println!("visible tips: {:?}", database.snapshot().await?);
}
# Ok(())
# }
```

A missing or present-but-`Unverified` dependency leaves the dependent entry
`Unverified` and invisible to default reads. A proven invalid snapshot, signature
or causal pin is `Failed`, not a dependency to fetch; ordinary retry does not
clear that verdict. Operational storage errors should be diagnosed before retry.
Do not manually set statuses to `Verified` or clear only the derived cache to
force progress. An offline trust reset clears previous `Failed` decisions too,
but the same invalid immutable entry will be rejected again under the new rules.

The supported manual sequence is exercised by
`test_delegated_entry_synced_unverified_then_verified`, including reset and
rebuild on the local backend matrix. Automatic dependency acquisition and
retry ordering are deferred to [PR #126](https://github.com/arcuru/eidetica/pull/126).
See the [causal authorization contract](../design/authentication.md#delegated-database-references)
for pointer rewinds, effective removal and the nested-floor recovery limitation.
