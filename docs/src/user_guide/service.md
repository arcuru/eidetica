# Service (Daemon) Mode

Eidetica can run as a local daemon that serves an Instance to multiple client processes over a Unix domain socket. This allows multiple CLI tools and applications to operate on the same Eidetica data without each process opening its own backend.

## When to Use Daemon Mode

Daemon mode is useful when:

- **Multiple processes** need concurrent access to the same Eidetica data
- **CLI tools** want fast startup without opening a storage backend each time
- **Background sync** should persist across short-lived client sessions
- A **long-running process** manages storage while lightweight clients connect on demand

For single-process applications, `Instance::open()` with a local backend is simpler and has no IPC overhead.

## Starting the Daemon

### CLI

The `eidetica daemon` command starts a service server:

```bash
# Start with default socket path and SQLite backend
eidetica daemon

# Specify a custom socket path
eidetica daemon --socket /tmp/my-eidetica.sock

# Use a specific backend
eidetica daemon --backend postgres --postgres-url "postgresql://user:pass@host/db"
```

The daemon prints its socket path on startup and runs until interrupted (SIGINT/SIGTERM).

### Default Socket Path

If no `--socket` is specified, the daemon uses:

1. `$XDG_RUNTIME_DIR/eidetica/service.sock` (preferred on Linux)
2. `/tmp/eidetica-$USER/service.sock` (fallback)

The `EIDETICA_SOCKET` environment variable can also set the socket path.

### Programmatic

To start a daemon from Rust code:

<!-- Code block ignored: Requires async runtime and Unix socket -->

```rust,ignore
use eidetica::{Instance, backend::database::Sqlite};
use eidetica::service::ServiceServer;
use tokio::sync::watch;

let backend = Sqlite::open("my_data.db").await?;
let instance = Instance::open(Box::new(backend)).await?;

let (shutdown_tx, shutdown_rx) = watch::channel(());
let server = ServiceServer::new(instance, "/tmp/eidetica.sock");

// Run the server (blocks until shutdown)
// Drop shutdown_tx to trigger graceful shutdown
server.run(shutdown_rx).await?;
```

## Connecting Clients

Use `Instance::connect()` to create a client-side Instance that communicates with the daemon:

<!-- Code block ignored: Requires a running daemon -->

```rust,ignore
use eidetica::Instance;

// Connect to a running daemon
let instance = Instance::connect("/tmp/eidetica.sock").await?;

// Use it exactly like a local Instance
instance.create_user("alice", None).await?;
let mut user = instance.login_user("alice", None).await?;

let default_key = user.get_default_key()?;
let db = user.create_database(eidetica::crdt::Doc::new(), &default_key).await?;
```

The returned Instance is fully transparent -- all downstream code (Database, Transaction, Store, User) works identically whether the Instance is local or connected to a daemon.

## Security Model

- **Keys and passwords stay client-side.** The daemon sees only encrypted key material and signed entries. Password verification and key derivation (Argon2id) happen in the client process.
- **No plaintext secrets cross the socket.** Authentication operations (user creation, login, key management) run locally in the client. Only storage operations (get, put, tips, etc.) are forwarded to the daemon.
- **The socket is a local Unix domain socket.** Access is controlled by filesystem permissions on the socket file. Only processes that can reach the socket path can connect.

## Multiple Clients

Multiple clients can connect to the same daemon simultaneously. Each client maintains its own connection and User session:

<!-- Code block ignored: Requires a running daemon -->

```rust,ignore
// Client 1
let instance1 = Instance::connect("/tmp/eidetica.sock").await?;
instance1.create_user("alice", None).await?;

// Client 2 (separate process or task)
let instance2 = Instance::connect("/tmp/eidetica.sock").await?;
let user = instance2.login_user("alice", None).await?;
```

All clients share the same underlying storage through the daemon's backend.

## Configuration Reference

| Option / Env Var               | Description                                        | Default                                         |
| ------------------------------ | -------------------------------------------------- | ----------------------------------------------- |
| `--socket` / `EIDETICA_SOCKET` | Unix socket path                                   | See [Default Socket Path](#default-socket-path) |
| `--backend`                    | Storage backend (`sqlite`, `postgres`, `inmemory`) | `sqlite`                                        |
| `--data-dir`                   | Data directory for storage files                   | Current directory                               |
| `--postgres-url`               | PostgreSQL connection URL                          | --                                              |

## Limitations

- **No server-push notifications.** Clients see the latest state on each request but are not proactively notified when the daemon receives entries from sync peers. Polling or re-reading is required to observe external changes.
- **Sync management is server-side.** Calling `enable_sync()` on a remote Instance creates a client-side sync module, which is not useful for daemon-managed sync. Configure sync on the daemon's Instance before clients connect, or use the daemon CLI with sync options.
- **Unix-only.** The service module requires Unix domain sockets and is not available on Windows.
- **Feature flag required.** The `service` feature must be enabled (included in the default `full` feature set).
