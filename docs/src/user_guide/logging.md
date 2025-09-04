# Logging

Eidetica uses the `tracing` crate for structured logging throughout the library. This provides flexible, performant logging that library users can configure to their needs.

## Quick Start

To enable logging in your Eidetica application, add `tracing-subscriber` to your dependencies and initialize it in your main function:

```toml
[dependencies]
eidetica = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

```rust,ignore
use tracing_subscriber::EnvFilter;

fn main() {
    // Initialize tracing with environment filter
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("eidetica=info".parse().unwrap())
        )
        .init();

    // Your application code here
}
```

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

## Examples

### Basic Application Logging

```rust,ignore
use eidetica::Instance;
use eidetica::backend::database::InMemory;
use tracing_subscriber::EnvFilter;

fn main() -> eidetica::Result<()> {
    // Set up logging with default info level
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("eidetica=info".parse().unwrap())
        )
        .init();

    let backend = Box::new(InMemory::new());
    let db = Instance::new(backend);

    // Library operations will now emit log messages
    let database = db.new_tree_default("my_key")?;

    Ok(())
}
```

### Custom Logging Configuration

```rust,ignore
use tracing_subscriber::{fmt, EnvFilter};
use tracing_subscriber::prelude::*;

fn main() {
    // Configure logging with custom format and filtering
    let filter = EnvFilter::try_new(
        "eidetica=debug,eidetica::sync=trace"
    ).unwrap();

    tracing_subscriber::registry()
        .with(fmt::layer()
            .with_target(false)  // Don't show target module
            .compact()            // Use compact formatting
        )
        .with(filter)
        .init();

    // Your application code here
}
```

### Logging in Tests

For tests, you can use `tracing-subscriber`'s test utilities:

```rust
#[cfg(test)]
mod tests {
    use tracing_subscriber::EnvFilter;

    #[test]
    fn test_with_logging() {
        // Initialize logging for this test
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();

        // Test code here - logs will be captured with test output
    }
}
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
