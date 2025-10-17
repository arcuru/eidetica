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
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
#
# fn main() -> eidetica::Result<()> {
let backend = Box::new(InMemory::new());
let db = Instance::open(backend)?;

// Add private key first
db.add_private_key("my_key")?;

// Create a database - this will log at INFO level
let mut settings = Doc::new();
settings.set("name", "my_database");
let database = db.new_database(settings, "my_key")?;

// Operations will emit logs at appropriate levels
// Use RUST_LOG to control what you see
# Ok(())
# }
```

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
