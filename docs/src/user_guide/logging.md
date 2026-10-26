# Logging

Eidetica uses the `tracing` crate for structured logging throughout the library. This provides flexible, performant logging that library users can configure to their needs.

## Quick Start

Eidetica uses the `tracing` crate, which means you can attach any subscriber to capture and format logs. The simplest approach is using `tracing-subscriber`:

```toml
[dependencies]
eidetica = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

```rust,ignore
use tracing_subscriber::EnvFilter;

fn main() -> eidetica::Result<()> {
    // Initialize tracing subscriber to see Eidetica logs
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Now all Eidetica operations will log according to RUST_LOG
    // ...
}
```

You can customize formatting, filtering, and output destinations. See the [tracing-subscriber documentation](https://docs.rs/tracing-subscriber) for advanced configuration options.

## Configuring Log Levels

Control logging verbosity using the `RUST_LOG` environment variable:

```bash
# Show only errors
RUST_LOG=eidetica=error cargo run

# Show info messages and above
RUST_LOG=eidetica=info cargo run

# Show debug messages for sync module
RUST_LOG=eidetica::sync=debug cargo run

# Show all trace messages (very verbose)
RUST_LOG=eidetica=trace cargo run
```

## Log Levels in Eidetica

Eidetica uses the following log levels:

- **ERROR**: Critical errors that prevent operations from completing
  - Failed database operations
  - Network errors during sync
  - Authentication failures
- **WARN**: Important warnings that don't prevent operation
  - Retry operations after failures
  - Invalid configuration detected
  - Deprecated feature usage
- **INFO**: High-level operational messages
  - Sync server started/stopped
  - Successful synchronization with peers
  - Database loaded/saved
- **DEBUG**: Detailed operational information
  - Sync protocol details
  - Database synchronization progress
  - Hook execution
- **TRACE**: Very detailed trace information
  - Individual entry processing
  - Detailed CRDT operations
  - Network protocol messages

## Using Eidetica with Logging

Once you've initialized a tracing subscriber, all Eidetica operations will automatically emit structured logs that you can capture and filter:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let backend = Box::new(Sqlite::in_memory().await?);
// Initialise with alice as the first/admin user — this logs at INFO level.
let (instance, mut user) = Instance::create_backend(
    backend,
    eidetica::NewAccount::passwordless("alice"),
).await?;

// Create a database - this will log at INFO level
let mut settings = Doc::new();
settings.set("name", "my_database");
let default_key = user.get_default_key()?;
let database = user.create_database(settings, &default_key).await?;

// Operations will emit logs at appropriate levels
// Use RUST_LOG to control what you see
# Ok(())
# }
```

## Following a Sync Operation

With `RUST_LOG=eidetica::sync=debug`, outbound sync logs include a
`sync_operation` span with an `operation_id` and `kind`.
The ID is a process-local sequence number, not a database ID or a wire-protocol
field. Use the process identity as well when combining logs from several peers.
Address races, engine-served handshakes and requests, and the subsequent
`sync_tree` lifecycle retain the same operation span. Incoming requests belong
to the remote peer's operation and do not receive its ID.

Each completed outbound operation emits one INFO-level `Sync route outcome`
event. A peer round can exchange several trees under one ID; a new round or
retry gets a new ID. Ticket-route preparation for a remote signer is a separate
operation from submitting the signed request.

The summary reports:

- `routes` and `attempted`: address hints considered and handshake tasks submitted.
  Background selection skips hints with no matching transport (`unsupported`).
- `route_failures`, `route_timeouts`, and `wrong_peer`: results consumed before
  selection or total failure. Timeouts are included in failures.
- `unobserved`: submitted results not consumed when selection ended, including
  cancelled dials and queued results. These are **not** counted as failures.
- `selected`: the route used for the exchange, if any.
- `trees_ok` and `trees_failed`: completed tree exchanges. `outcome` is `partial`
  when a peer round has both successes and failures, and `failed` when all
  attempted trees fail, even if the background round itself returns `Ok`.
- `error` or `last_tree_error`: the operation failure or last failed tree.
  `timeout_phase` and `peer_reachable` retain the existing timeout distinction;
  silence is not evidence of an authorization refusal.

Transport errors remain typed at the public sync API: `SyncError::Timeout`
keeps its phase, while a rejected bootstrap is `SyncError::BootstrapRejected`.
Applications should classify the returned error rather than describing every
failure as a refusal. The summary adds diagnostics; it does not change routing,
retry policy, authentication, or the on-disk database.

## Performance Considerations

The `tracing` crate is designed to have minimal overhead when logging is disabled. Log statements that aren't enabled at the current level are optimized away at compile time.

For performance-critical code paths, Eidetica uses appropriate log levels:

- Hot paths use `trace!` level to avoid overhead in production
- Synchronization operations use `debug!` for detailed tracking
- Only important events use `info!` and above

## Integration with Observability Tools

The `tracing` ecosystem supports various backends for production observability:

- **Console output**: Default, human-readable format
- **JSON output**: For structured logging systems
- **OpenTelemetry**: For distributed tracing
- **Jaeger/Zipkin**: For trace visualization

See the [`tracing` documentation](https://docs.rs/tracing) for more advanced integration options.
