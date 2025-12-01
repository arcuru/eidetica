# Errors

The database uses a custom `Result` (`crate::Result`) and `Error` (`crate::Error`) type hierarchy defined in [`src/lib.rs`](../../src/lib.rs). Errors are typically propagated up the call stack using `Result`.

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
