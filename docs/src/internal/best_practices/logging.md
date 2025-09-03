# Logging Best Practices

This guide documents best practices for using the `tracing` crate within the Eidetica codebase.

## Overview

Eidetica uses the `tracing` crate for all logging needs. This provides:

- Structured logging with minimal overhead
- Compile-time optimization for disabled log levels
- Span-based context for async operations
- Integration with external observability tools

## Log Level Guidelines

Choose log levels based on the importance and frequency of events:

### ERROR (`tracing::error!`)

Use for unrecoverable errors that prevent operations from completing:

```rust,ignore
tracing::error!("Failed to store entry {}: {}", entry.id(), error);
```

**When to use:**

- Database operation failures
- Network errors that can't be retried
- Authentication/authorization failures
- Corrupted data detection

### WARN (`tracing::warn!`)

Use for important warnings that don't prevent operation:

```rust,ignore
tracing::warn!("Failed to send to {}: {}. Adding to retry queue.", peer, error);
```

**When to use:**

- Retryable failures
- Invalid configuration (with fallback)
- Deprecated feature usage
- Performance degradation detected

### INFO (`tracing::info!`)

Use for high-level operational messages:

```rust,ignore
tracing::info!("Sync server started on {}", address);
```

**When to use:**

- Service lifecycle events (start/stop)
- Successful major operations
- Configuration changes
- Important state transitions

### DEBUG (`tracing::debug!`)

Use for detailed operational information:

```rust,ignore
tracing::debug!("Syncing {} trees with peer {}", tree_count, peer_id);
```

**When to use:**

- Detailed operation progress
- Protocol interactions
- Algorithm steps
- Non-critical state changes

### TRACE (`tracing::trace!`)

Use for very detailed trace information:

```rust,ignore
tracing::trace!("Processing entry {} with {} parents", entry_id, parent_count);
```

**When to use:**

- Individual item processing
- Detailed algorithm execution
- Network packet contents
- Frequent operations in hot paths

## Performance Considerations

### Hot Path Optimization

For performance-critical code paths, follow these guidelines:

1. **Use appropriate levels**: Hot paths should use `trace!` to avoid overhead
2. **Avoid string formatting**: Use structured fields instead
3. **Check before complex operations**: Use `tracing::enabled!` for expensive log data

```rust,ignore
// Good: Structured fields, minimal overhead
tracing::trace!(entry_id = %entry.id(), parent_count = parents.len(), "Processing entry");

// Bad: String formatting in hot path
tracing::debug!("Processing entry {} with {} parents", entry.id(), parents.len());

// Good: Check before expensive operation
if tracing::enabled!(tracing::Level::TRACE) {
    let debug_info = expensive_debug_calculation();
    tracing::trace!("Debug info: {}", debug_info);
}
```

### Async and Background Operations

Use spans to provide context for async operations:

```rust,ignore
use tracing::{info_span, Instrument};

async fn sync_with_peer(peer_id: &str) {
    async {
        tracing::debug!("Starting sync");
        // ... sync logic ...
        tracing::debug!("Sync complete");
    }
    .instrument(info_span!("sync", peer_id = %peer_id))
    .await;
}
```

## Module-Specific Guidelines

### Sync Module

- Use `info!` for server lifecycle and peer connections
- Use `debug!` for sync protocol operations
- Use `trace!` for individual entry transfers
- Use spans for peer-specific context

### Backend Module

- Use `error!` for storage failures
- Use `debug!` for cache operations
- Use `trace!` for individual entry operations

### Authentication Module

- Use `error!` for signature verification failures
- Use `error!` for permission violations
- Use `debug!` for key operations
- Never log private keys or sensitive data

### CRDT Module

- Use `debug!` for merge operations
- Use `trace!` for individual CRDT operations
- Include operation type in structured fields

## Structured Logging

Prefer structured fields over string interpolation:

```rust,ignore
// Good: Structured fields
tracing::info!(
    tree_id = %tree.id(),
    entry_count = entries.len(),
    peer = %peer_address,
    "Synchronizing tree"
);

// Bad: String interpolation
tracing::info!(
    "Synchronizing tree {} with {} entries to peer {}",
    tree.id(), entries.len(), peer_address
);
```

## Error Context

When logging errors, include relevant context:

```rust,ignore
// Good: Includes context
tracing::error!(
    error = %e,
    entry_id = %entry.id(),
    tree_id = %tree.id(),
    "Failed to store entry during sync"
);

// Bad: Missing context
tracing::error!("Failed to store entry: {}", e);
```

## Testing with Logs

### Automatic Test Logging Setup

Eidetica uses a global test setup with the `ctor` crate to automatically initialize tracing for all tests. This is configured in `tests/it/main.rs`:

This means **all tests automatically have tracing enabled at INFO level** without any setup code needed in individual test functions.

### Viewing Test Logs

By default, Rust's test harness captures log output and only shows it for failing tests:

```bash
# Normal test run - only see logs from failing tests
cargo test

# See logs from ALL tests (passing and failing)
cargo test -- --nocapture

# Control log level with environment variable
RUST_LOG=eidetica=debug cargo test -- --nocapture

# See logs from specific test
cargo test test_sync_operations -- --nocapture

# Trace level for specific module during tests
RUST_LOG=eidetica::sync=trace cargo test -- --nocapture
```

### Writing Tests with Logging

Tests should use `println!` for outputs.

### Key Benefits

- **Zero setup**: No initialization code needed in individual tests
- **Environment control**: Use `RUST_LOG` to control verbosity per test run
- **Clean output**: Logs only appear when tests fail or with `--nocapture`
- **Proper isolation**: `with_test_writer()` ensures logs don't mix between parallel tests
- **Library visibility**: See internal library operations during test execution

## Common Patterns

### Operation Success/Failure

```rust,ignore
match operation() {
    Ok(result) => {
        tracing::debug!("Operation succeeded");
        result
    }
    Err(e) => {
        tracing::error!(error = %e, "Operation failed");
        return Err(e);
    }
}
```

### Retry Logic

```rust,ignore
for attempt in 1..=max_attempts {
    match try_operation() {
        Ok(result) => {
            if attempt > 1 {
                tracing::info!("Operation succeeded after {} attempts", attempt);
            }
            return Ok(result);
        }
        Err(e) if attempt < max_attempts => {
            tracing::warn!(
                error = %e,
                attempt,
                max_attempts,
                "Operation failed, retrying"
            );
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                attempts = max_attempts,
                "Operation failed after all retries"
            );
            return Err(e);
        }
    }
}
```

## Anti-Patterns to Avoid

1. **Don't log sensitive data**: Never log private keys, passwords, or PII
2. **Don't use println/eprintln**: Always use tracing macros
3. **Don't log in tight loops**: Use trace level or aggregate logging
4. **Don't format strings unnecessarily**: Use structured fields
5. **Don't ignore log levels**: Use appropriate levels for context

## Future Considerations

As the codebase grows, consider:

- Adding custom tracing subscribers for specific subsystems
- Implementing trace sampling for high-volume operations
- Adding metrics collection alongside tracing
- Creating domain-specific span attributes
