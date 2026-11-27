# Errors

The database uses a custom `Result` (`crate::Result`) and `Error` (`crate::Error`) type hierarchy defined in [`crates/lib/src/lib.rs`](https://github.com/arcuru/eidetica/blob/main/crates/lib/src/lib.rs). Errors are typically propagated up the call stack using `Result`.

The `Error` enum uses a modular approach with structured error types from each component:

- `Io(#[from] std::io::Error)`: Wraps underlying I/O errors from backend operations or file system access.
- `Serialize(#[from] serde_json::Error)`: Wraps errors occurring during JSON serialization or deserialization.
- `Auth(auth::AuthError)`: Structured authentication errors with detailed context.
- `Backend(backend::DatabaseError)`: Database storage and retrieval errors.
- `Instance(instance::InstanceError)`: Instance management errors.
- `CRDT(crdt::CRDTError)`: CRDT operation and merge errors.
- `Store(store::StoreError)`: Store data access and validation errors.
- `Transaction(transaction::TransactionError)`: Transaction coordination errors.

The use of `#[error(transparent)]` allows for zero-cost conversion from module-specific errors into `crate::Error` using the `?` operator. Helper methods like `is_not_found()`, `is_permission_denied()`, and `is_authentication_error()` enable categorized error handling without pattern matching on specific variants.

## Database Creation Failures

`Error::DatabaseCreation { root_id, source }` adds context when genesis succeeded but a later creation step failed.
`root_id` is the exact committed root, and `source` is the original typed `Error`, also available through the standard error source chain.
`Error::created_database_id()` returns that ID without destructuring the error.
The wrapper reports module `database`; inspect `source` for the underlying error category.

`Database::create` and `create_with_init` attach this context to connected session-key registration failures after genesis.
`AccountSession::create_database`, `create_database_with_init`, and `DatabaseBuilder::build` also retain it for subsequent local tracking, key installation, and sync setup failures.
The wrapper is constructed locally by the SDK, including on connected clients; it does not add a service request or change the wire format.
Pre-genesis failures keep their original error, without a created ID.
A failed commit acknowledgement can still have an unknown outcome; this is not response-loss recovery.

The ID grants no authority and does not imply that tracking, key registration, or sync setup completed.
Repair must use the retained credentials and normal Auth checks, not create a replacement root.
See [Database Sharing](../user_guide/database_management.md#creation-failures) for caller handling.

## Cross-Process Error Propagation

When using the [service (daemon) mode](service.md), errors must cross a Unix socket boundary. The `ServiceError` wire type captures the originating module, discriminant name, and display message:

```rust,ignore
pub struct ServiceError {
    pub module: String,  // e.g. "backend"
    pub kind: String,    // e.g. "EntryNotFound"
    pub message: String, // Display message
}
```

Invalid staging capabilities and oversized records have explicit backend errors: an expired, foreign, or already-published Store-state token returns `InvalidStoreStateStagingToken`, and a record that cannot fit below the service frame bound returns `RecordTooLarge`.

On the client side, `service_error_to_eidetica_error()` reconstructs the appropriate `crate::Error` variant by matching on `(module, kind)`. Recognized error types are reconstructed precisely (e.g., `BackendError::EntryNotFound`); unrecognized combinations fall back to `Error::Io` with the original message. This means error-handling code using `is_not_found()` and similar helpers works identically for local and remote instances.
