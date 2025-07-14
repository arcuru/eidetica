# Testing Best Practices

This document outlines testing patterns and practices used in the Eidetica codebase, focusing on integration testing, test organization, and comprehensive validation strategies.

## Testing Architecture

### 1. **Integration-First Testing Strategy**

Eidetica uses a single integration test binary approach rather than unit tests:

```
tests/it/
├── mod.rs              # Main test module and shared imports
├── helpers.rs          # Cross-cutting test utilities
├── auth/               # Authentication testing
│   ├── mod.rs
│   ├── helpers.rs      # Auth-specific test utilities
│   └── integration.rs  # Auth integration tests
├── backend/            # Backend testing
├── basedb/             # BaseDB testing
├── crdt/               # CRDT testing
├── subtree/            # Subtree testing
└── tree/               # Tree operation testing
```

**Key principle**: Test through public interfaces to validate real-world usage patterns.

### 2. **Test Module Organization**

Each test module mirrors the main codebase structure:

```rust
// tests/it/auth/mod.rs
mod helpers;
mod integration;

pub use helpers::*;  // Re-export helpers for other test modules
```

**Benefits**:

- Clear organization that matches source structure
- Shared helpers available across related test modules
- Integration tests validate component interactions

### 3. **Comprehensive Test Helpers**

Provide rich helper functions to reduce test boilerplate:

```rust
// tests/it/helpers.rs

/// Create a test database with in-memory backend
pub fn setup_test_db() -> BaseDB {
    let backend = Box::new(InMemory::new());
    BaseDB::new(backend)
}

/// Create a database with pre-configured authentication keys
pub fn setup_db_with_keys(key_names: &[&str]) -> BaseDB {
    let mut db = setup_test_db();
    for key_name in key_names {
        db.add_private_key(key_name).expect("Failed to add test key");
    }
    db
}

/// Create an authenticated tree with test data
pub fn setup_authenticated_tree() -> (BaseDB, Tree) {
    let db = setup_db_with_keys(&["test_key"]);
    let tree = db.new_tree(Map::new(), "test_key")
        .expect("Failed to create test tree");
    (db, tree)
}

/// Assert that a Dict contains expected value at key
pub fn assert_dict_value(dict: &Dict, key: &str, expected: &str) {
    let value = dict.get(key).expect("Key should exist");
    match value {
        Value::String(s) => assert_eq!(s, expected),
        other => panic!("Expected string value, got: {:?}", other),
    }
}
```

## Authentication Testing Patterns

### 1. **Authentication-Specific Helpers**

Provide helpers for common authentication scenarios:

```rust
// tests/it/auth/helpers.rs

/// Create test key pairs for authentication testing
pub fn create_test_keypair() -> (String, String) {
    // Generate test public/private key pair
    // Return (public_key, private_key)
}

/// Setup a tree with specific permission levels
pub fn setup_tree_with_permissions(permissions: Vec<(String, Permission)>) -> (BaseDB, Tree) {
    let mut db = setup_test_db();

    // Add keys with specified permissions
    for (key_name, permission) in permissions {
        db.add_private_key(&key_name).expect("Failed to add key");
        // Configure permissions in settings
    }

    let tree = db.new_tree(Map::new(), &permissions[0].0)
        .expect("Failed to create tree");
    (db, tree)
}

/// Verify that an operation requires specific permission
pub fn assert_requires_permission<F>(operation: F, expected_permission: Permission)
where
    F: FnOnce() -> Result<()>,
{
    match operation() {
        Err(Error::Auth(AuthError::PermissionDenied { required_permission, .. })) => {
            assert_eq!(required_permission, expected_permission);
        }
        Ok(_) => panic!("Expected permission denied error"),
        Err(other) => panic!("Expected permission denied, got: {:?}", other),
    }
}
```

### 2. **Permission Testing Patterns**

Test authentication and authorization systematically:

```rust
#[test]
fn test_permission_levels() {
    let (db, tree) = setup_tree_with_permissions(vec![
        ("admin_key".to_string(), Permission::Admin),
        ("write_key".to_string(), Permission::Write),
        ("read_key".to_string(), Permission::Read),
    ]);

    // Test admin can do everything
    tree.set_default_auth_key("admin_key");
    assert!(tree.create_operation().is_ok());

    // Test write key can write but not admin operations
    tree.set_default_auth_key("write_key");
    assert!(tree.create_operation().is_ok());
    assert_requires_permission(
        || tree.configure_permissions("new_key", Permission::Write),
        Permission::Admin
    );

    // Test read key cannot write
    tree.set_default_auth_key("read_key");
    assert_requires_permission(
        || tree.create_operation(),
        Permission::Write
    );
}
```

## CRDT Testing Patterns

### 1. **CRDT State Testing**

Test CRDT merge semantics and state consistency:

```rust
#[test]
fn test_crdt_merge_semantics() {
    let (db, tree) = setup_authenticated_tree();

    // Create two parallel operations
    let mut op1 = tree.create_operation().expect("Failed to create op1");
    let mut op2 = tree.create_operation().expect("Failed to create op2");

    // Modify same data in both operations
    let dict1 = op1.subtree::<Dict>("test").expect("Failed to get dict1");
    dict1.set("key1", "value1").expect("Failed to set key1");

    let dict2 = op2.subtree::<Dict>("test").expect("Failed to get dict2");
    dict2.set("key2", "value2").expect("Failed to set key2");

    // Commit both operations
    let entry1 = op1.commit().expect("Failed to commit op1");
    let entry2 = op2.commit().expect("Failed to commit op2");

    // Verify both values are preserved after merge
    let merged_state = tree.compute_state().expect("Failed to compute state");
    assert!(merged_state.contains_key("key1"));
    assert!(merged_state.contains_key("key2"));
}
```

### 2. **Conflict Resolution Testing**

Test CRDT conflict resolution for concurrent modifications:

```rust
#[test]
fn test_concurrent_map_modifications() {
    let (db, tree) = setup_authenticated_tree();

    // Create concurrent operations modifying the same key
    let mut op1 = tree.create_operation().expect("Failed to create op1");
    let mut op2 = tree.create_operation().expect("Failed to create op2");

    let dict1 = op1.subtree::<Dict>("test").expect("Failed to get dict1");
    dict1.set("conflicted_key", "value_from_op1").expect("Failed to set in op1");

    let dict2 = op2.subtree::<Dict>("test").expect("Failed to get dict2");
    dict2.set("conflicted_key", "value_from_op2").expect("Failed to set in op2");

    // Commit both operations
    let entry1 = op1.commit().expect("Failed to commit op1");
    let entry2 = op2.commit().expect("Failed to commit op2");

    // Verify deterministic conflict resolution
    let final_state = tree.compute_state().expect("Failed to compute final state");
    let final_value = final_state.get("conflicted_key").expect("Key should exist");

    // The result should be deterministic (based on entry ID ordering)
    assert!(final_value == "value_from_op1" || final_value == "value_from_op2");

    // Verify the same operation sequence produces the same result
    let (db2, tree2) = setup_authenticated_tree();
    // Repeat same operations...
    let final_state2 = tree2.compute_state().expect("Failed to compute final state");
    assert_eq!(final_state, final_state2, "CRDT merge should be deterministic");
}
```

## Performance Testing

### 1. **Benchmark Integration**

Use criterion for performance testing alongside integration tests:

```rust
// benches/integration_benchmarks.rs
use criterion::{criterion_group, criterion_main, Criterion};
use eidetica_tests_it::helpers::*;

fn benchmark_bulk_operations(c: &mut Criterion) {
    let (db, tree) = setup_authenticated_tree();

    c.bench_function("bulk_dict_inserts", |b| {
        b.iter(|| {
            let mut op = tree.create_operation().expect("Failed to create operation");
            let dict = op.subtree::<Dict>("test").expect("Failed to get dict");

            for i in 0..1000 {
                dict.set(format!("key_{}", i), format!("value_{}", i))
                    .expect("Failed to set value");
            }

            op.commit().expect("Failed to commit");
        });
    });
}

criterion_group!(benches, benchmark_bulk_operations);
criterion_main!(benches);
```

### 2. **Memory Usage Testing**

Test memory allocation patterns in critical paths:

```rust
#[test]
fn test_memory_efficient_operations() {
    let (db, tree) = setup_authenticated_tree();

    // Measure memory usage for large operations
    let initial_memory = get_memory_usage();  // Platform-specific helper

    {
        let mut op = tree.create_operation().expect("Failed to create operation");
        let dict = op.subtree::<Dict>("test").expect("Failed to get dict");

        // Perform memory-intensive operation
        for i in 0..10_000 {
            dict.set(format!("key_{}", i), format!("value_{}", i))
                .expect("Failed to set value");
        }

        op.commit().expect("Failed to commit");
    }

    // Force garbage collection and measure memory
    std::mem::drop(tree);
    let final_memory = get_memory_usage();

    // Verify reasonable memory usage
    let memory_increase = final_memory - initial_memory;
    assert!(memory_increase < 100_000_000, "Memory usage too high: {} bytes", memory_increase);
}
```

## Error Condition Testing

### 1. **Comprehensive Error Testing**

Test all error conditions systematically:

```rust
#[test]
fn test_authentication_error_conditions() {
    let (db, tree) = setup_authenticated_tree();

    // Test missing key
    tree.set_default_auth_key("nonexistent_key");
    let result = tree.create_operation();
    assert!(matches!(result, Err(Error::Auth(AuthError::KeyNotFound { .. }))));

    // Test invalid permissions
    db.add_private_key("read_only_key").expect("Failed to add key");
    // Configure read-only permission...
    tree.set_default_auth_key("read_only_key");

    let result = tree.create_operation();
    assert!(matches!(result, Err(Error::Auth(AuthError::PermissionDenied { .. }))));
}

#[test]
fn test_backend_error_conditions() {
    // Test with a backend that simulates failures
    let backend = Box::new(FailingBackend::new());
    let db = BaseDB::new(backend);

    // Test storage failure handling
    let result = db.new_tree(Map::new(), "test_key");
    assert!(matches!(result, Err(Error::Backend(DatabaseError::StorageError { .. }))));
}
```

### 2. **Error Recovery Testing**

Test error recovery and system resilience:

```rust
#[test]
fn test_partial_failure_recovery() {
    let (db, tree) = setup_authenticated_tree();

    // Create operation that will partially fail
    let mut op = tree.create_operation().expect("Failed to create operation");
    let dict = op.subtree::<Dict>("test").expect("Failed to get dict");

    // Set some values successfully
    dict.set("good_key1", "value1").expect("Failed to set good_key1");
    dict.set("good_key2", "value2").expect("Failed to set good_key2");

    // Simulate failure during commit
    // (This would require a backend that can simulate partial failures)

    // Verify system can recover and continue operating
    let mut op2 = tree.create_operation().expect("Failed to create recovery operation");
    let dict2 = op2.subtree::<Dict>("test").expect("Failed to get dict for recovery");
    dict2.set("recovery_key", "recovery_value").expect("Failed to set recovery value");
    op2.commit().expect("Failed to commit recovery operation");

    // Verify recovery operation succeeded
    let state = tree.compute_state().expect("Failed to compute state after recovery");
    assert_dict_value(&state, "recovery_key", "recovery_value");
}
```

## Test Data Management

### 1. **Test Data Patterns**

Create realistic test data for comprehensive testing:

```rust
// tests/it/data.rs

pub struct TestDataBuilder {
    entries: Vec<Entry>,
    relationships: Vec<(String, String)>,  // (parent, child) relationships
}

impl TestDataBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            relationships: Vec::new(),
        }
    }

    pub fn add_entry(mut self, id: &str, data: Map) -> Self {
        // Create entry with specified data
        self
    }

    pub fn add_relationship(mut self, parent: &str, child: &str) -> Self {
        self.relationships.push((parent.to_string(), child.to_string()));
        self
    }

    pub fn build(self) -> Vec<Entry> {
        // Build connected entry graph
        self.entries
    }
}

// Usage in tests
#[test]
fn test_complex_entry_relationships() {
    let test_data = TestDataBuilder::new()
        .add_entry("root", Map::new())
        .add_entry("child1", Map::new())
        .add_entry("child2", Map::new())
        .add_relationship("root", "child1")
        .add_relationship("root", "child2")
        .build();

    // Test with complex data structure
}
```

### 2. **Property-Based Testing Integration**

Consider property-based testing for CRDT operations:

```rust
// TODO: Integrate proptest for property-based testing
// This would be valuable for testing CRDT properties like:
// - Commutativity: A ⊕ B = B ⊕ A
// - Associativity: (A ⊕ B) ⊕ C = A ⊕ (B ⊕ C)
// - Idempotency: A ⊕ A = A

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;

    // TODO: Implement property tests for CRDT operations
    // proptest! {
    //     #[test]
    //     fn test_crdt_commutativity(
    //         ops1 in prop::collection::vec(operation_strategy(), 1..10),
    //         ops2 in prop::collection::vec(operation_strategy(), 1..10)
    //     ) {
    //         // Test that ops1 ⊕ ops2 = ops2 ⊕ ops1
    //     }
    // }
}
```

## Test Organization Patterns

### 1. **Test Categorization**

Organize tests by functionality and complexity:

```rust
// Fast unit-style tests
mod unit_style_tests {
    #[test]
    fn test_entry_id_generation() {
        // Fast, isolated tests
    }
}

// Integration tests requiring database setup
mod integration_tests {
    #[test]
    fn test_tree_operations() {
        // Tests requiring full system setup
    }
}

// Performance and stress tests
mod performance_tests {
    #[test]
    #[ignore]  // Run with --ignored for performance testing
    fn test_large_scale_operations() {
        // Heavy tests for performance validation
    }
}
```

### 2. **Test Configuration**

Use configuration for test environment setup:

```rust
// tests/it/config.rs

pub struct TestConfig {
    pub enable_logging: bool,
    pub backend_type: BackendType,
    pub performance_testing: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            enable_logging: std::env::var("EIDETICA_TEST_LOGGING").is_ok(),
            backend_type: BackendType::InMemory,
            performance_testing: std::env::var("EIDETICA_PERF_TESTS").is_ok(),
        }
    }
}

pub fn setup_test_environment() -> TestConfig {
    let config = TestConfig::default();

    if config.enable_logging {
        // Initialize test logging
    }

    config
}
```

## Common Testing Anti-Patterns

### ❌ **Overly Complex Test Setup**

```rust
// DON'T DO THIS
#[test]
fn test_complex_scenario() {
    // 50+ lines of setup code
    let db = setup_complex_db_with_many_configs();
    let tree1 = create_tree_with_specific_config();
    let tree2 = create_another_tree_with_different_config();
    // ... more setup

    // Actual test is buried in setup
    assert_eq!(result, expected);
}
```

### ❌ **Testing Implementation Details**

```rust
// DON'T DO THIS
#[test]
fn test_internal_state_details() {
    let tree = setup_tree();

    // Testing internal implementation instead of behavior
    assert_eq!(tree.internal_cache.len(), 5);
    assert!(tree.private_field.is_some());
}
```

### ❌ **Flaky Tests with Timing Dependencies**

```rust
// DON'T DO THIS
#[test]
fn test_async_operation() {
    start_async_operation();
    std::thread::sleep(Duration::from_millis(100));  // Flaky timing
    assert!(operation_completed());
}
```

### ✅ **Good Testing Patterns**

```rust
// DO THIS
#[test]
fn test_user_workflow() {
    // Clean setup with helper
    let (db, tree) = setup_authenticated_tree();

    // Test actual user workflow
    let mut op = tree.create_operation().expect("Should create operation");
    let dict = op.subtree::<Dict>("data").expect("Should get dict");
    dict.set("user_data", "test_value").expect("Should set value");
    op.commit().expect("Should commit");

    // Verify end-to-end behavior
    let state = tree.compute_state().expect("Should compute state");
    assert_dict_value(&state, "user_data", "test_value");
}
```

## Future Testing Improvements

### Planned Enhancements

- **TODO**: Implement property-based testing for CRDT operations
- **TODO**: Add fuzzing tests for entry parsing and validation
- **TODO**: Create performance regression testing suite
- **TODO**: Develop test data generation for realistic scenarios
- **TODO**: Add integration tests with external storage backends

### Testing Infrastructure Evolution

- **TODO**: Consider test parallelization strategies
- **TODO**: Evaluate test coverage reporting and metrics
- **TODO**: Design test data versioning for compatibility testing
- **TODO**: Plan for end-to-end testing with real network conditions

## Summary

Effective testing in Eidetica provides:

- **Integration-focused approach** that tests real-world usage patterns
- **Comprehensive helpers** that reduce test boilerplate and improve maintainability
- **Authentication testing** that validates security and permission systems
- **CRDT testing** that ensures merge semantics and conflict resolution work correctly
- **Performance testing** that validates system behavior under load
- **Error condition testing** that ensures robust error handling and recovery

Following these patterns ensures the codebase maintains high quality and reliability as it evolves.
