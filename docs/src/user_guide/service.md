# Service (Daemon) Mode

Eidetica can run as a local daemon that serves an Instance to multiple client processes over a Unix domain socket. This allows multiple CLI tools and applications to operate on the same Eidetica data without each process opening its own backend.

## When to Use Daemon Mode

Daemon mode is useful when:

- **Multiple processes** need concurrent access to the same Eidetica data
- **CLI tools** want fast startup without opening a storage backend each time
- **Background sync** should persist across short-lived client sessions
- A **long-running process** manages storage while lightweight clients connect on demand

For single-process applications, `Instance::open_backend()` with a local backend is simpler and has no IPC overhead.

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
use eidetica::Instance;
use eidetica::service::ServiceServer;
use tokio::sync::watch;

// `Instance::connect` accepts a URL describing the backend; here the
// daemon serves a sqlite file. See the rustdoc for the full URL grammar.
let instance = Instance::connect("sqlite://./my_data.db").await?;

let (shutdown_tx, shutdown_rx) = watch::channel(());
let server = ServiceServer::new(instance, "/tmp/eidetica.sock");

// Run the server (blocks until shutdown)
// Drop shutdown_tx to trigger graceful shutdown
server.run(shutdown_rx).await?;
```

## Connecting Clients

Clients reach the daemon by passing a `unix://` URL to `Instance::connect`:

<!-- Code block ignored: Requires a running daemon -->

```rust,ignore
use eidetica::Instance;

// Connect to a running daemon
let instance = Instance::connect("unix:///tmp/eidetica.sock").await?;

// Use it exactly like a local Instance. The daemon was initialised with
// an initial admin user via `eidetica daemon init --username ops`
// (see the CLI reference); log in as that user, then create application
// users via the admin path.
let admin = instance.login_user("ops", None).await?;
admin.admin().await?.create_user(eidetica::NewUser::passwordless("alice")).await?;
let mut user = instance.login_user("alice", None).await?;

let default_key = user.get_default_key()?;
let db = user.create_database(eidetica::crdt::Doc::new(), &default_key).await?;
```

The returned Instance is fully transparent -- all downstream code (Database, Transaction, Store, User) works identically whether the Instance is local or connected to a daemon.

## Security Model

- **Keys and passwords stay client-side.** The daemon sees only encrypted key material and signed entries. Password verification and key derivation (Argon2id) happen in the client process.
- **No plaintext secrets cross the socket.** Authentication operations (user creation, login, key management) run locally in the client. Only storage operations (get, put, tips, etc.) are forwarded to the daemon.
- **The socket is a local Unix domain socket.** Access is controlled by filesystem permissions on the socket file. Only processes that can reach the socket path can connect.

> ⚠️ **The deployment bootstrap fails closed.** Both the NixOS module and
> the published container image refuse to start on a fresh backend unless
> the operator supplies a credential source for the initial admin user.
> This avoids silently creating a passwordless admin that any reachable
> client could use.
>
> **NixOS module** — set exactly one of:
>
> - `services.eidetica.initialPasswordFile = "/path/to/password-file";`
>   (recommended; read via systemd `LoadCredential` so the service user
>   never needs read access to the file itself), or
> - `services.eidetica.allowPasswordlessAdmin = true;` (INSECURE; trusted
>   or LAN deployments only — the module warns at rebuild time when this
>   is combined with a non-loopback `host`).
>
> **Container image** — provide one of, in priority order:
>
> 1. A password file mounted at `/run/secrets/admin_password` (preferred;
>    keeps the password off the process table and out of
>    `docker inspect`).
> 2. `EIDETICA_ADMIN_PASSWORD` env.
> 3. `EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1` env (INSECURE; local/dev only).
>
> Without any of the above the container entrypoint exits 1 with an
> actionable error. To bootstrap your own admin with a password manually,
> run `eidetica daemon init --username <NAME>` against the data directory
> before first start.

## Multiple Clients

Multiple clients can connect to the same daemon simultaneously. Each client maintains its own connection and User session:

<!-- Code block ignored: Requires a running daemon -->

```rust,ignore
// Client 1: an admin session creates the new user via the InstanceAdmin path.
let instance1 = Instance::connect("unix:///tmp/eidetica.sock").await?;
let admin = instance1.login_user("ops", None).await?;
admin.admin().await?.create_user(eidetica::NewUser::passwordless("alice")).await?;

// Client 2 (separate process or task): log in as the user that was just created.
let instance2 = Instance::connect("unix:///tmp/eidetica.sock").await?;
let user = instance2.login_user("alice", None).await?;
```

All clients share the same underlying storage through the daemon's backend.

**Entry verification is owned by the daemon.** Clients do not run their own
verification pass — `update_verification_status` and the verification-status
queries are not exposed over the socket. The daemon's Instance stores synced
entries as `Unverified`, runs `Database::verify()`, and serves reads from the
resulting **Verified frontier**. Every connected client therefore sees the
same verified view; there is no per-client `allow_unverified()` toggle over
the wire. (See [Core Concepts](core_concepts.md) for the verification model.)

## Configuration Reference

| Option / Env Var               | Description                                        | Default                                         |
| ------------------------------ | -------------------------------------------------- | ----------------------------------------------- |
| `--socket` / `EIDETICA_SOCKET` | Unix socket path                                   | See [Default Socket Path](#default-socket-path) |
| `--backend`                    | Storage backend (`sqlite`, `postgres`, `inmemory`) | `sqlite`                                        |
| `--data-dir`                   | Data directory for storage files                   | Current directory                               |
| `--postgres-url`               | PostgreSQL connection URL                          | --                                              |

## Limitations

- **No server-push notifications.** Clients see the latest state on each request but are not proactively notified when the daemon receives entries from sync peers. Polling or re-reading is required to observe external changes.
- **Sync management is server-side.** Sync runs in the daemon's process and a connected client can't drive its lifecycle from over the wire. `enable_sync()` on a remote Instance returns `Ok(())` as a no-op so calling code that wraps it doesn't error out; to actually enable sync, configure it on the daemon's Instance before clients connect, or use the daemon CLI with sync options. A future admin-gated `EnableSync` RPC would let a client ask the daemon to enable its sync subsystem remotely.
- **Unix-only.** The service module requires Unix domain sockets and is not available on Windows.
- **Feature flag required.** The `service` feature must be enabled (included in the default `full` feature set).
