# CLI Reference

The `eidetica` binary provides a server and management commands for inspecting and operating on Eidetica instances.

## Commands

### `serve` (default)

Starts the Eidetica server with HTTP and sync endpoints.

Running `eidetica` with no subcommand is equivalent to `eidetica serve`. This default is likely to change in the future.

```bash
eidetica serve [OPTIONS]
```

| Option           | Short | Default     | Env Var                 | Description                                                     |
| ---------------- | ----- | ----------- | ----------------------- | --------------------------------------------------------------- |
| `--port`         | `-p`  | `3000`      | `EIDETICA_PORT`         | Port to listen on                                               |
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
| `URL`           |       | `http://127.0.0.1:3000` | URL of the server to check (appends `/health` if needed) |
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

| Option           | Short | Default       | Env Var                 | Description                                                     |
| ---------------- | ----- | ------------- | ----------------------- | --------------------------------------------------------------- |
| `--socket`       | `-s`  | auto-detected | `EIDETICA_SOCKET`       | Unix socket path (see [Service Mode](service.md) for defaults)  |
| `--backend`      | `-b`  | `sqlite`      | `EIDETICA_BACKEND`      | Storage backend (`sqlite`, `postgres`, `inmemory`)              |
| `--data-dir`     | `-d`  | current dir   | `EIDETICA_DATA_DIR`     | Data directory for storage files                                |
| `--postgres-url` |       | —             | `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL (required when backend is `postgres`) |

The daemon runs until interrupted with SIGINT or SIGTERM. Clients connect using `Instance::connect(socket_path)`. See [Service (Daemon) Mode](service.md) for full documentation.

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
| `EIDETICA_PORT`         | Port for the HTTP server (`serve`)                 | `3000`            |
| `EIDETICA_HOST`         | Bind address (`serve`)                             | `0.0.0.0`         |
| `EIDETICA_BACKEND`      | Storage backend (`sqlite`, `postgres`, `inmemory`) | `sqlite`          |
| `EIDETICA_DATA_DIR`     | Directory for database and data files              | current directory |
| `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL                          | —                 |

Command-line flags take precedence over environment variables.

## Examples

```bash
# Start server with defaults (sqlite backend, port 3000)
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
eidetica daemon --socket /tmp/eidetica.sock
```
