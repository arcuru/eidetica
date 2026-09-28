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

### Optional web dashboard

`eidetica daemon --dashboard` also hosts the existing login/dashboard, health,
and stats pages. It listens on exactly the address set by `--dashboard-host`
and `--dashboard-port` (default **127.0.0.1:3000**). The default daemon remains
socket-only. Both listeners and the Iroh sync listener use the **same
Instance/backend**, whether SQLite, PostgreSQL, or in-memory; no separate
`serve` process is needed. The dashboard does not expose the trusted Unix
service RPC or `/api/v0` HTTP peer-sync endpoint. Peer sync continues over
Iroh. If either listener stops unexpectedly, the daemon stops both, flushes
sync, and exits with an error. Shutdown removes its socket.

The dashboard speaks plain HTTP. For remote access, terminate HTTPS at a
reverse proxy (for example, Traefik) and forward to the dashboard's bind
address, preserving the browser's `Host` and `Origin` headers. Who can reach
the HTTP listener directly is up to the bind address and firewall; with a
proxy on the same host, bind to loopback and leave the port closed.

```nix
services.eidetica = {
  enable = true;
  daemon = true;
  dashboard = true;
  host = "127.0.0.1";
  port = 3000;
  initialPasswordFile = "/run/agenix/eidetica-admin-password";
};
```

Browser sessions persist in daemon memory until logout or restart; cookies are
HttpOnly, SameSite=Strict and host-only, and Secure when the browser origin is
HTTPS. Unlike socket-only mode, dashboard logins decrypt signing keys inside
the daemon's web session memory; do not enable the dashboard if the daemon
must never hold plaintext keys. Every web form POST requires an `Origin` that
matches the request's `Host`; requests without one are rejected.

A passwordless account can be logged into by **anyone who can reach the
dashboard**: other local users, other hosts if the bind allows it, and web
pages open in a local browser through DNS rebinding. Use passwords for any
account a dashboard can reach. The separately launched `serve` command remains
available for legacy HTTP sync and web deployments.

NixOS and Home Manager modules preserve their `serve` default. Set
`services.eidetica.daemon = true;` to switch to the socket daemon and
`services.eidetica.dashboard = true;` to opt into its dashboard; `host` and
`port` set its bind address and port. A dashboard requires daemon mode. The
NixOS module opens the port when `openFirewall = true` and a web listener
(legacy `serve` or the dashboard) is running. The NixOS daemon puts its socket
at `dataDir/service.sock` (not in systemd's private `/tmp`); client processes
need owner-approved access to that directory.

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
// Binding completes only after the socket is ready to accept clients.
let server = ServiceServer::bind(instance, eidetica::service::default_socket_path()).await?;

// Serve until shutdown_tx is dropped.
server.run(shutdown_rx).await?;
```

## Connecting Clients

Clients reach the daemon by passing a `unix://` URL to `Instance::connect`:

<!-- Code block ignored: Requires a running daemon -->

```rust,ignore
use eidetica::Instance;

// Connect to a running daemon
let instance = Instance::connect(eidetica::service::default_socket_url()).await?;

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

- **Socket-client keys and passwords stay client-side.** The socket daemon sees only encrypted key material and signed entries from those clients. Password verification and key derivation (Argon2id) happen in the client process. Optional dashboard sessions instead hold decrypted keys in daemon memory (see above).
- **No plaintext secrets cross the socket.** Authentication operations (user creation, login, key management) run locally in the client. PasswordStore decryption and encrypted-cache materialization are also client-side; a warm encrypted Table point read uses only encrypted point-record requests. Only storage operations (get, put, tips, etc.) are forwarded to the daemon.
- **The socket is a local Unix domain socket.** Access is controlled by filesystem permissions on the socket file. Only processes that can reach the socket path can connect.
- **The socket directory defines who is trusted.** Missing directories are created with mode `0700`; the socket is mode `0660`. An existing parent must be owned by the daemon user, must not be writable by group or others, and cannot be reached through a symlink. Its group, setgid bit, and traversal permissions may grant trusted Unix-group members access.

Any process that can connect gets the existing fully trusted service API.
For cross-user clients, pre-provision an owner-controlled directory with the intended group and setgid/traversal bits; the daemon does not change its ownership or mode.
Shared-writable, sticky, and symlinked layouts are rejected.
An adjacent lock coordinates cooperating daemons, stale sockets are recovered, and graceful or dropped servers remove the socket they bound.

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
>   is combined with a web listener: legacy `serve` or the dashboard).
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
let instance1 = Instance::connect(eidetica::service::default_socket_url()).await?;
let admin = instance1.login_user("ops", None).await?;
admin.admin().await?.create_user(eidetica::NewUser::passwordless("alice")).await?;

// Client 2 (separate process or task): log in as the user that was just created.
let instance2 = Instance::connect(eidetica::service::default_socket_url()).await?;
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

| Option / Env Var                               | Description                                                                              | Default                                         |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------- | ----------------------------------------------- |
| `--dashboard` / `EIDETICA_DASHBOARD`           | Serve web dashboard alongside socket (boolean)                                           | disabled                                        |
| `--dashboard-host` / `EIDETICA_DASHBOARD_HOST` | Dashboard bind address                                                                   | `127.0.0.1`                                     |
| `--dashboard-port` / `EIDETICA_DASHBOARD_PORT` | Dashboard port                                                                           | `3000`                                          |
| `--socket` / `EIDETICA_SOCKET`                 | Unix socket path                                                                         | See [Default Socket Path](#default-socket-path) |
| `--sync-ticket` / `EIDETICA_SYNC_TICKETS`      | Bootstrap/reconcile a database from a native ticket (repeatable; env is comma-separated) | --                                              |
| `--backend`                                    | Storage backend (`sqlite`, `postgres`, `inmemory`)                                       | `sqlite`                                        |
| `--data-dir`                                   | Data directory for storage files                                                         | Current directory                               |
| `--postgres-url`                               | PostgreSQL connection URL                                                                | --                                              |

## Limitations

- **Sync management is server-side.** The daemon always loads persisted sync state, starts its Iroh listener, and keeps syncing without connected service clients. Use `--sync-ticket <TICKET>` for owner-side bootstrap and peer setup; the resulting relationships persist across restart. A connected client can't drive that lifecycle over the wire; `enable_sync()` on a remote Instance remains a no-op. User tracking changes made through the socket are reconciled by the daemon's Instance callbacks.
- **Unix-only.** The service module requires Unix domain sockets and is not available on Windows.
- **Feature flag required.** The `service` feature must be enabled (included in the default `full` feature set).

For the `Database::on_write` semantics on a connected Instance — including
the deliberate trade that callbacks fire asynchronously to `commit()` in
exchange for a single daemon-canonical event ordering — see [Write
Callbacks](transactions.md#write-callbacks).
