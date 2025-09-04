# Testing Architecture

Eidetica employs a comprehensive testing strategy to ensure reliability and correctness. This document outlines our testing approach, organization, and best practices for developers working with or contributing to the codebase.

## Test Organization

Eidetica centralizes all its tests into a unified integration test binary located in the `tests/it/` directory. All testing is done through public interfaces, without separate unit tests, promoting interface stability.

The main categories of testing activities are:

### Comprehensive Integration Tests

All tests for the Eidetica crate are located in the `crates/lib/tests/it/` directory. These tests verify both:

- **Component behavior**: Validating individual components through their public interfaces
- **System behavior**: Ensuring different components interact correctly when used together

This unified suite is organized as a single integration test binary, following the pattern described by [matklad](https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html).

The module structure within `crates/lib/tests/it/` mirrors the main library structure from `crates/lib/src/`. Each major component has its own test module directory.

### Example Applications as Tests

The `examples/` directory contains standalone applications that demonstrate library features. While not traditional tests, these examples serve as pragmatic validation of the API's usability and functionality in real-world scenarios.

For instance, the `examples/todo/` directory contains a complete Todo application that demonstrates practical usage of Eidetica, effectively acting as both documentation and functional validation.

## Test Coverage Goals

Eidetica maintains ambitious test coverage targets:

- **Core Data Types**: 95%+ coverage for all core data types (`Entry`, `Database`, `SubTree`)
- **CRDT Implementations**: 100% coverage for all CRDT implementations
- **Database Implementations**: 90%+ coverage, including error cases
- **Public API Methods**: 100% coverage

## Testing Patterns and Practices

### Test-Driven Development

For new features, we follow a test-driven approach:

1. Write tests defining expected behavior
2. Implement features to satisfy those tests
3. Refactor while maintaining test integrity

### Interface-First Testing

We exclusively test through public interfaces. This approach ensures API stability.

### Test Helpers

Eidetica provides test helpers organized into main helpers (`crates/lib/tests/it/helpers.rs`) for common database and database setup, and module-specific helpers for specialized testing scenarios. Each test module has its own `helpers.rs` file with utilities specific to that component's testing needs.

### Standard Test Structure

Tests follow a consistent setup-action-assertion pattern, utilizing test helpers for environment preparation and result verification.

### Error Case Testing

Tests cover both successful operations and error conditions to ensure robust error handling throughout the system.

## CRDT-Specific Testing

Given Eidetica's CRDT foundation, special attention is paid to testing CRDT properties:

1. **Merge Semantics**: Validating that merge operations produce expected results
2. **Conflict Resolution**: Ensuring conflicts resolve according to CRDT rules
3. **Determinism**: Verifying that operations are commutative when required

## Running Tests

### Basic Test Execution

Run all tests with:

```bash
cargo test
# Or using the task runner
task test
```

Eidetica uses [nextest](https://nexte.st/) for test execution, which provides improved test output and performance:

```bash
cargo nextest run --workspace --all-features
```

### Targeted Testing

Run specific test categories:

```bash
# Run all integration tests
cargo test --test it

# Run specific integration tests
cargo nextest run tests::it::store
```

Run tests using `cargo test --test it` for all integration tests, or target specific modules with patterns like `cargo test --test it auth::`. The project also supports `cargo nextest` for improved test output and performance.

### Coverage Analysis

Eidetica uses [tarpaulin](https://github.com/xd009642/tarpaulin) for code coverage analysis:

```bash
# Run with coverage analysis
task coverage
# or
cargo tarpaulin --workspace --skip-clean --include-tests --all-features --output-dir coverage --out lcov
```

## Module Test Organization

Each test module follows a consistent structure with `mod.rs` for declarations, `helpers.rs` for module-specific utilities, and separate files for different features or aspects being tested.

## Contributing New Tests

When adding features or fixing bugs:

1. Add focused tests to the appropriate module within the `crates/lib/tests/it/` directory. These tests should cover:
   - Specific functionality of the component or module being changed through its public interface.
   - Interactions between the component and other parts of the system.
2. Consider adding example code in the `examples/` directory for significant new features to demonstrate usage and provide further validation.
3. Test both normal operation ("happy path") and error cases.
4. Use the test helpers in `crates/lib/tests/it/helpers.rs` for general setup, and module-specific helpers for specialized scenarios.
5. If you need common test utilities for a new pattern, add them to the appropriate helpers.rs file.

## Best Practices

- **Descriptive Test Names**: Use `test_<component>_<functionality>` or `test_<functionality>_<scenario>` naming pattern
- **Self-Documenting Tests**: Write clear test code with useful comments
- **Isolation**: Ensure tests don't interfere with each other
- **Speed**: Keep tests fast to encourage frequent test runs
- **Determinism**: Avoid flaky tests that intermittently fail
