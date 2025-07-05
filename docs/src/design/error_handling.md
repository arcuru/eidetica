# Error Handling Design

## Overview

This document outlines the design philosophy and architecture of error handling in the Eidetica library. The design follows principles from [Error Handling in Rust](https://sabrinajewson.org/blog/errors) with a focus on modularity, locality, and user ergonomics.

## Design Philosophy

### Error Locality

Errors are defined close to their unit of fallibility. Each module owns its error types, making them discoverable alongside the functions that produce them. This approach provides several benefits:

- **Discoverability**: Errors are documented where they occur
- **Modularity**: Each component manages its own failure modes
- **Maintainability**: Error handling evolves with the code that produces errors

### Structured Error Data

Instead of generic string-based errors, the system uses structured data with typed fields. This provides:

- **Pattern Matching**: Enables exhaustive error handling
- **Context Preservation**: All relevant information is captured as typed fields
- **Performance**: No string formatting in hot paths
- **Tooling Support**: IDEs can provide better completion and validation

### Progressive Context

Errors gain context as they bubble up through system layers. Lower layers provide specific technical details, while higher layers add user-facing context and categorization.

## Architecture

### Error Hierarchy

The error system follows a tree structure where each module defines its own error type, and these are aggregated into a top-level `Error` enum:

```
Error (lib.rs)
├── Io(std::io::Error)              # Standard library errors
├── Serialize(serde_json::Error)    # JSON serialization errors
├── Auth(AuthError)                 # Authentication module errors
├── Backend(DatabaseError)          # Backend storage errors
├── Base(BaseError)                 # Base database errors
├── CRDT(CRDTError)                 # CRDT operation errors
├── Subtree(SubtreeError)           # Subtree operation errors
└── AtomicOp(AtomicOpError)         # Atomic operation errors
```

### Module-Specific Errors

Each major component defines its own error enum with variants specific to that domain:

- **AuthError**: Key resolution, permission validation, signature verification
- **DatabaseError**: Storage operations, entry retrieval, integrity violations
- **BaseError**: Tree management, configuration, initialization
- **CRDTError**: Merge conflicts, type mismatches, serialization
- **SubtreeError**: Data access patterns, type validation, key operations
- **AtomicOpError**: Transaction coordination, concurrency, validation

### Transparent Conversion

The `#[error(transparent)]` attribute ensures zero-cost conversion between module-specific errors and the top-level `Error` type. The `?` operator works seamlessly across module boundaries.

## Error Categories

### By Error Nature

**Not Found Errors**: Each module defines specific "not found" variants rather than using a generic error. This provides better context about what wasn't found:

- `AuthError::KeyNotFound` - specific authentication key
- `DatabaseError::EntryNotFound` - database entry by ID
- `SubtreeError::KeyNotFound` - data key within a subtree

**Permission Errors**: Authentication and authorization failures are handled through structured variants that capture the security context and attempted operation.

**Validation Errors**: Input validation and state consistency errors provide detailed information about what validation failed and why.

**Operation Errors**: Business logic and operational constraint violations include context about the failed operation and system state.

### By System Layer

**Core Errors** (`Io`, `Serialize`): Fundamental system operations that can fail across all components.

**Storage Layer** (`Backend`, `Base`): Database and persistence operations, including storage backend failures and tree management.

**Data Layer** (`CRDT`, `Subtree`): Data structure operations, type validation, and access pattern enforcement.

**Application Layer** (`Auth`, `AtomicOp`): High-level operations that coordinate multiple subsystems.

## Error Handling Patterns

### Contextual Error Propagation

Errors preserve context as they move up the stack. Lower-level technical details remain accessible while higher-level categorization enables appropriate handling:

```rust
// Low-level: Specific technical failure
DatabaseError::EntryNotFound { id: "abc123" }

// High-level: Categorized for application logic
if error.is_not_found() {
    // Handle any "not found" scenario uniformly
}
```

### Helper Methods for Classification

The top-level `Error` type provides classification methods that abstract over the specific error variants:

- `is_not_found()`: Resource lookup failures
- `is_permission_denied()`: Authorization failures
- `is_authentication_error()`: Authentication-related failures
- `is_operation_error()`: Business logic constraint violations
- `is_validation_error()`: Input or state validation failures

This enables applications to handle broad categories of errors without needing to know about every specific variant.

### Non-Exhaustive Enums

All error enums use `#[non_exhaustive]` to enable future extension without breaking changes. Applications should use helper methods rather than exhaustive pattern matching.

## Performance Characteristics

### Zero-Cost Abstractions

- `#[error(transparent)]` eliminates wrapper overhead
- Structured fields avoid string formatting until display
- No heap allocations in common error paths
- Compile-time validation of error propagation

### Efficient Propagation

The `?` operator works seamlessly across module boundaries with automatic conversion. Error context is preserved without runtime overhead.

## Working with Errors

### For Library Users

Use the helper methods on the top-level `Error` type for most error handling. These provide stable APIs that won't break when new error variants are added:

```rust
match operation() {
    Ok(result) => handle_success(result),
    Err(e) if e.is_not_found() => handle_not_found(),
    Err(e) if e.is_permission_denied() => handle_auth_failure(),
    Err(e) => handle_generic_error(e),
}
```

### For Library Developers

Define new error variants in the appropriate module's error enum. Use structured fields to capture all relevant context:

```rust
#[error("Invalid tree configuration: {reason}")]
InvalidTreeConfiguration { reason: String },
```

Add helper methods to both the module-specific error type and the top-level `Error` type for common classification needs.

## Future Extensibility

### Adding New Errors

New error variants can be added to any module's error enum without breaking existing code, thanks to `#[non_exhaustive]`. The module's `From` implementation automatically makes new variants available through the top-level `Error` type.

### Cross-Module Error Coordination

When operations span multiple modules, errors can be wrapped or converted to provide appropriate context. The atomic operation system demonstrates this pattern by coordinating errors from auth, backend, and CRDT modules.

### Error Recovery

Structured error data enables sophisticated error recovery. Applications can inspect specific error conditions and attempt alternative strategies based on the exact failure mode.

## Conclusion

The error handling design prioritizes developer experience through structured data, clear categorization, and zero-cost abstractions. The modular architecture ensures errors remain maintainable as the system grows, while the helper methods provide stable APIs for common error handling patterns.

The system balances specificity (detailed error variants for precise handling) with usability (broad categories for common patterns), enabling both robust error handling and clear application logic.
