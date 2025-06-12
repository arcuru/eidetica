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

The module structure within `crates/lib/tests/it/` mirrors the main library structure from `crates/lib/src/`; `crates/lib/tests/it/subtree.rs` contains tests for `crates/lib/src/subtree.rs`, etc.

### Example Applications as Tests

The `examples/` directory contains standalone applications that demonstrate library features. While not traditional tests, these examples serve as pragmatic validation of the API's usability and functionality in real-world scenarios.

For instance, the `examples/todo/` directory contains a complete Todo application that demonstrates practical usage of Eidetica, effectively acting as both documentation and functional validation.

## Test Coverage Goals

Eidetica maintains ambitious test coverage targets:

- **Core Data Types**: 95%+ coverage for all core data types (`Entry`, `Tree`, `SubTree`)
- **CRDT Implementations**: 100% coverage for all CRDT implementations
- **Backend Implementations**: 90%+ coverage, including error cases
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

Eidetica provides a comprehensive set of test helpers in the `crates/lib/tests/it/helpers.rs` module to simplify test setup and common assertions:

- **Tree Setup Helpers**:

  - `setup_tree()`: Creates a basic tree with an InMemoryBackend
  - `setup_tree_with_settings()`: Creates a tree with initial settings
  - `setup_tree_with_multiple_kvstores()`: Creates a tree with multiple KVStore subtrees and preset values

- **Data Structure Helpers**:

  - `create_kvnested()`: Creates a KVNested with specified key-value pairs
  - `create_nested_kvnested()`: Creates a nested KVNested structure
  - `create_kvoverwrite()`: Creates a KVOverWrite with initial data

- **Assertion Helpers**:
  - `assert_kvstore_value()`: Verifies a KVStore contains an expected string value
  - `assert_key_not_found()`: Verifies a key doesn't exist in a store
  - `assert_nested_value()`: Checks deep nested values inside a KVNested structure
  - `assert_path_deleted()`: Validates that a path is deleted (has tombstone or is missing)

Using these helpers improves test readability, reduces code duplication, and ensures consistent test setup across the codebase.

### Standard Test Structure

Most tests follow this pattern:

```rust
#[test]
fn test_component_functionality() {
    // Setup - prepare the test environment
    let tree = setup_tree(); // Using a test helper

    // Action - perform the operation being tested
    let operation = tree.new_operation().expect("Failed to create operation");
    let store = KVStore::new(&operation, "data").expect("Failed to create store");
    store.set("key", "value").expect("Failed to set value");

    // Assertion - verify the expected outcome using a helper
    assert_kvstore_value(&store, "key", "value");
}
```

### Error Case Testing

We ensure tests cover error conditions, not just the happy path:

```rust
#[test]
fn test_error_handling() {
    // Setup
    // ...

    // Verify error behavior
    match operation_that_should_fail() {
        Err(Error::ExpectedErrorType) => (), // Expected
        other => panic!("Expected specific error, got {:?}", other),
    }
}
```

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
cargo nextest run tests::it::subtree
```

To run tests for specific modules or parts of the codebase, you can target the integration test binary and specify the test path:

```bash
# Run all tests within the integration test binary
cargo test --test it

# Run specific tests within a module in the integration test suite (e.g., entry tests)
cargo nextest run tests::it::entry
# or, if you have test functions directly in tests/it/subtree.rs:
cargo nextest run tests::it::subtree
```

### Coverage Analysis

Eidetica uses [tarpaulin](https://github.com/xd009642/tarpaulin) for code coverage analysis:

```bash
# Run with coverage analysis
task coverage
# or
cargo tarpaulin --workspace --skip-clean --include-tests --all-features --output-dir coverage --out lcov
```

## Contributing New Tests

When adding features or fixing bugs:

1. Add focused tests to the appropriate module within the `crates/lib/tests/it/` directory. These tests should cover:
   - Specific functionality of the component or module being changed through its public interface.
   - Interactions between the component and other parts of the system.
2. Consider adding example code in the `examples/` directory for significant new features to demonstrate usage and provide further validation.
3. Test both normal operation ("happy path") and error cases.
4. Use the test helpers in `crates/lib/tests/it/helpers.rs` to simplify test setup and assertions. Consider adding new helpers for common patterns.

## Best Practices

- **Descriptive Test Names**: Use `test_<component>_<functionality>` or `test_<functionality>_<scenario>` naming pattern
- **Self-Documenting Tests**: Write clear test code with useful comments
- **Isolation**: Ensure tests don't interfere with each other
- **Speed**: Keep tests fast to encourage frequent test runs
- **Determinism**: Avoid flaky tests that intermittently fail
