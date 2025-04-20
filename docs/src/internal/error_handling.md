## Error Handling

The database uses a custom `Result` (`crate::Result`) and `Error` (`crate::Error`) type hierarchy defined in [`src/lib.rs`](../../src/lib.rs). Errors are typically propagated up the call stack using `Result`.

The `Error` enum variants include:

- `NotFound`: Entry or resource not found.
- `AlreadyExists`: Attempting to create something that already exists.
- `Io(#[from] std::io::Error)`: Wraps underlying I/O errors from backend operations or file system access.
- `Serialize(#[from] serde_json::Error)`: Wraps errors occurring during JSON serialization or deserialization.

The use of `#[from]` allows for ergonomic conversion from standard I/O and Serde JSON errors into `crate::Error` using the `?` operator.

<!-- TODO: Verify the exact variants in `crate::Error` enum and provide links to the source code definition. -->
<!-- TODO: Explain the error handling strategy (e.g., bubbling up Result, specific recovery mechanisms). -->
