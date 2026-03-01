# Testing

Most tests are in `tests/it/` as a single integration test binary, following the [matklad pattern](https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html). Tests validate behavior through public interfaces only.

Unit tests should only be used when integration tests are not feasible or when testing private implementation details.

## Organization

The module structure in `tests/it/` mirrors `src/`. Each module has:

- `mod.rs` for test declarations
- `helpers.rs` for module-specific utilities
- Common helpers in `tests/it/helpers.rs`

## Running Tests

```bash
just test              # Run all tests with nextest
cargo test --test it   # Run integration tests
cargo test auth::      # Run specific module tests
```

## Backend Matrix Testing

The test suite runs against multiple storage backends via the `TEST_BACKEND` environment variable. The `test_backend()` factory in `helpers.rs` creates the appropriate backend for each test:

| Value      | Backend               | Notes                            |
| ---------- | --------------------- | -------------------------------- |
| (unset)    | InMemory              | Default, fastest                 |
| `inmemory` | InMemory              | Explicit default                 |
| `sqlite`   | SQLite (in-memory)    | Requires `sqlite` feature        |
| `postgres` | PostgreSQL            | Requires `postgres` feature      |
| `service`  | RemoteBackend via RPC | Requires `service` feature, unix |

The `service` backend starts a fresh in-process daemon with an InMemory backend for each `test_backend()` call, routing all operations through the Unix socket RPC layer. This maintains the same isolation semantics as other backends.

## Writing Tests

1. Add tests to appropriate module in `tests/it/`
2. Test both happy path and error cases
3. Use helpers from `tests/it/helpers.rs`
4. Follow `test_<component>_<functionality>` naming
