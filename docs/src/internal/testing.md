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
task test              # Run all tests with nextest
cargo test --test it   # Run integration tests
cargo test auth::      # Run specific module tests
```

## Writing Tests

1. Add tests to appropriate module in `tests/it/`
2. Test both happy path and error cases
3. Use helpers from `tests/it/helpers.rs`
4. Follow `test_<component>_<functionality>` naming
