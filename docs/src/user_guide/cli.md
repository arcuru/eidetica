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

| Argument/Option | Short | Default                  | Description                                             |
| --------------- | ----- | ------------------------ | ------------------------------------------------------- |
| `URL`           |       | `http://127.0.0.1:3000`  | URL of the server to check (appends `/health` if needed) |
| `--timeout`     | `-t`  | `5`                      | Timeout in seconds                                      |

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

| Variable                | Description                                        | Default                                 |
| ----------------------- | -------------------------------------------------- | --------------------------------------- |
| `EIDETICA_PORT`         | Port for the HTTP server (`serve`)                 | `3000`                                  |
| `EIDETICA_HOST`         | Bind address (`serve`)                             | `0.0.0.0`                               |
| `EIDETICA_BACKEND`      | Storage backend (`sqlite`, `postgres`, `inmemory`) | `sqlite`                                |
| `EIDETICA_DATA_DIR`     | Directory for database and data files              | current directory                       |
| `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL                          | —                                       |

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
```
