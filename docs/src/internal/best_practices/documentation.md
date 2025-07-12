# Documentation Best Practices

This document outlines documentation standards and practices used throughout the Eidetica codebase, focusing on comprehensive, maintainable, and user-focused documentation.

## Documentation Philosophy

### 1. **Documentation as Code**

Treat documentation with the same rigor as source code:

- **Version controlled**: All documentation lives in the repository
- **Reviewed**: Documentation changes go through code review
- **Tested**: Examples and code snippets are validated
- **Maintained**: Documentation is updated with code changes

### 2. **Audience-Focused Writing**

Write for specific audiences with clear purposes:

- **Public API Documentation**: For library users and integrators
- **Internal Documentation**: For contributors and maintainers
- **Architecture Documentation**: For system design understanding
- **Best Practices**: For development guidance and consistency

### 3. **Progressive Disclosure**

Structure information from general to specific:

- **Overview**: High-level purpose and concepts
- **Getting Started**: Quick wins and basic usage
- **Detailed Guides**: Comprehensive feature coverage
- **Reference**: Complete API and implementation details

## API Documentation Standards

### 1. **Module-Level Documentation**

Every module should have comprehensive header documentation:

````rust
//! # Authentication Module
//!
//! This module provides Ed25519-based authentication and authorization
//! for all Eidetica operations.
//!
//! ## Core Functionality
//!
//! - **Key Management**: Generate, store, and manage Ed25519 keypairs
//! - **Digital Signatures**: Sign and verify entry authenticity
//! - **Permission System**: Hierarchical access control (Read/Write/Admin)
//! - **Rate Limiting**: Prevent abuse and ensure fair resource usage
//!
//! ## Usage Examples
//!
//! ### Basic Authentication Setup
//!
//! ```rust
//! use eidetica::auth::*;
//! use eidetica::BaseDB;
//!
//! # fn main() -> eidetica::Result<()> {
//! let mut db = BaseDB::new(backend);
//!
//! // Generate authentication key
//! let key_id = "user_key";
//! db.add_private_key(key_id)?;
//!
//! // Create authenticated tree
//! let tree = db.new_tree(Map::new(), key_id)?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Permission Management
//!
//! ```rust
//! # use eidetica::*;
//! # fn main() -> eidetica::Result<()> {
//! # let mut db = BaseDB::new(backend);
//! # let tree = db.new_tree(Map::new(), "admin_key")?;
//! // Configure permissions for different users
//! tree.configure_permissions("read_user", Permission::Read)?;
//! tree.configure_permissions("write_user", Permission::Write)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Integration Points
//!
//! This module integrates with:
//! - [`BaseDB`](crate::basedb::BaseDB): For key storage and management
//! - [`Tree`](crate::tree::Tree): For per-tree permission configuration
//! - [`AtomicOp`](crate::atomicop::AtomicOp): For operation authentication
//!
//! ## Security Considerations
//!
//! - Private keys are stored separately from synchronized data
//! - All operations require valid authentication
//! - Permission checks occur at operation time, not creation time
//! - Rate limiting prevents brute force attacks
//!
//! ## Performance Notes
//!
//! - Signature verification is CPU-intensive; consider caching for read-heavy workloads
//! - Permission checks are fast HashMap lookups
//! - Key generation uses cryptographically secure randomness
````

### 2. **Function Documentation Standards**

Document all public functions with comprehensive details:

````rust
/// Sets the default authentication key for this tree.
///
/// When set, all operations created via `new_operation()` will automatically
/// use this key for signing unless explicitly overridden.
///
/// # Parameters
/// * `key_id` - Authentication key identifier that will be stored.
///   Accepts any string type (`&str`, `String`, `&String`) for maximum ergonomics.
///
/// # Example
/// ```rust
/// # use eidetica::*;
/// # use eidetica::backend::database::InMemory;
/// # use eidetica::basedb::BaseDB;
/// # use eidetica::crdt::Map;
/// # fn example() -> Result<()> {
/// # let backend = Box::new(InMemory::new());
/// # let db = BaseDB::new(backend);
/// # db.add_private_key("test_key")?;
/// # let mut tree = db.new_tree(Map::new(), "test_key")?;
/// tree.set_default_auth_key("my_key");                    // &str
/// tree.set_default_auth_key(String::from("my_key"));      // String
/// tree.set_default_auth_key(&String::from("my_key"));     // &String
/// # Ok(())
/// # }
/// ```
///
/// # Performance
/// Uses `Into<String>` to avoid double conversion overhead when storing the key ID.
///
/// # Related
/// - [`clear_default_auth_key()`](Self::clear_default_auth_key) - Remove default key
/// - [`new_operation()`](Self::new_operation) - Create authenticated operations
pub fn set_default_auth_key(&mut self, key_id: impl Into<String>) {
    self.default_auth_key = Some(key_id.into());
}
````

**Documentation Elements**:

- **Purpose**: Clear, concise description of what the function does
- **Parameters**: Type information and usage patterns
- **Examples**: Working code that demonstrates usage
- **Performance**: Notes about efficiency and optimization
- **Related**: Links to related functions and concepts
- **Errors**: When the function can fail and why (for Result-returning functions)

### 3. **Type Documentation**

Document structs, enums, and traits with context:

````rust
/// A simple key-value store SubTree providing ergonomic access to Map CRDT data.
///
/// It assumes that the SubTree data is a Map CRDT, which allows for nested map structures.
/// This implementation supports string values, as well as deletions via tombstones.
/// For more complex data structures, consider using the nested capabilities of Map directly.
///
/// # Usage Patterns
///
/// ## Basic Key-Value Operations
///
/// ```rust
/// # use eidetica::*;
/// # use eidetica::subtree::Dict;
/// # fn example() -> Result<()> {
/// # let (db, tree) = setup_test();
/// let mut op = tree.create_operation()?;
/// let dict = op.subtree::<Dict>("users")?;
///
/// // Set values
/// dict.set("user1", "Alice")?;
/// dict.set("user2", "Bob")?;
///
/// // Get values
/// let user1 = dict.get("user1")?;
/// assert_eq!(user1, Some("Alice"));
/// # Ok(())
/// # }
/// ```
///
/// ## Path-Based Operations
///
/// ```rust
/// # use eidetica::*;
/// # use eidetica::subtree::Dict;
/// # fn example() -> Result<()> {
/// # let (db, tree) = setup_test();
/// # let mut op = tree.create_operation()?;
/// # let dict = op.subtree::<Dict>("config")?;
/// // Work with nested structures
/// dict.set_at_path(&["database", "host"], "localhost")?;
/// dict.set_at_path(&["database", "port"], "5432")?;
///
/// let host = dict.get_at_path(&["database", "host"])?;
/// assert_eq!(host, Some("localhost"));
/// # Ok(())
/// # }
/// ```
///
/// # Implementation Notes
///
/// - All operations are atomic and CRDT-compatible
/// - Deletions create tombstones for proper CRDT merge semantics
/// - String values are stored directly without additional serialization
/// - Path operations use dot notation internally for efficient storage
pub struct Dict {
    name: String,
    atomic_op: AtomicOp,
}
````

### 4. **Error Documentation**

Document error types with context and recovery guidance:

```rust
/// Authentication and authorization errors.
///
/// These errors occur during key management, signature verification,
/// and permission checking operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// Authentication key was not found in the key store.
    ///
    /// This usually indicates:
    /// - The key ID was misspelled
    /// - The key was not added to the database
    /// - The key was removed from the key store
    ///
    /// # Recovery
    /// - Verify the key ID is correct
    /// - Add the key using [`BaseDB::add_private_key()`]
    /// - Check if the key was accidentally removed
    #[error("Authentication key not found: {key_id}")]
    KeyNotFound { key_id: String },

    /// Digital signature verification failed.
    ///
    /// This indicates:
    /// - The entry was tampered with after signing
    /// - The signature was created with a different private key
    /// - Corrupted signature or entry data
    ///
    /// # Security Implications
    /// This error suggests potential security issues and should be logged
    /// and investigated thoroughly.
    #[error("Invalid signature for key: {key_id}")]
    InvalidSignature { key_id: String },

    /// Operation requires higher permission level.
    ///
    /// The authenticated key does not have sufficient permissions
    /// to perform the requested operation.
    ///
    /// # Recovery
    /// - Use a key with higher permissions
    /// - Request permission elevation from an admin
    /// - Modify the operation to require lower permissions
    #[error("Permission denied: {operation} requires {required_permission:?}")]
    PermissionDenied {
        operation: String,
        required_permission: Permission,
    },
}
```

## Code Example Standards

### 1. **Complete, Runnable Examples**

All examples should be complete and testable:

````rust
/// Create and manage a collaborative document.
///
/// # Example
/// ```rust
/// use eidetica::*;
/// use eidetica::backend::database::InMemory;
/// use eidetica::basedb::BaseDB;
/// use eidetica::crdt::Map;
/// use eidetica::subtree::Dict;
///
/// fn main() -> Result<()> {
///     // Setup database with authentication
///     let backend = Box::new(InMemory::new());
///     let mut db = BaseDB::new(backend);
///     db.add_private_key("alice")?;
///     db.add_private_key("bob")?;
///
///     // Create collaborative document tree
///     let mut document = db.new_tree(Map::new(), "alice")?;
///     document.configure_permissions("bob", Permission::Write)?;
///
///     // Alice creates initial content
///     {
///         let mut op = document.create_operation()?;
///         let content = op.subtree::<Dict>("content")?;
///         content.set("title", "Collaborative Document")?;
///         content.set("author", "Alice")?;
///         op.commit()?;
///     }
///
///     // Bob adds content
///     document.set_default_auth_key("bob");
///     {
///         let mut op = document.create_operation()?;
///         let content = op.subtree::<Dict>("content")?;
///         content.set("editor", "Bob")?;
///         content.set_at_path(&["metadata", "last_modified"], "2024-01-15")?;
///         op.commit()?;
///     }
///
///     // Read final state
///     let state = document.compute_state::<Dict>("content")?;
///     assert_eq!(state.get("title")?, Some("Collaborative Document"));
///     assert_eq!(state.get("author")?, Some("Alice"));
///     assert_eq!(state.get("editor")?, Some("Bob"));
///
///     Ok(())
/// }
/// ```
pub fn example_function() {}
````

### 2. **Error Handling in Examples**

Show proper error handling patterns:

````rust
/// Handle authentication errors gracefully.
///
/// # Example
/// ```rust
/// use eidetica::*;
/// use eidetica::auth::AuthError;
///
/// fn handle_auth_operation(tree: &Tree) -> Result<()> {
///     match tree.create_operation() {
///         Ok(op) => {
///             // Operation created successfully
///             println!("Operation created");
///             Ok(())
///         }
///         Err(Error::Auth(AuthError::KeyNotFound { key_id })) => {
///             eprintln!("Key not found: {}. Please add the key first.", key_id);
///             Err(Error::Auth(AuthError::KeyNotFound { key_id }))
///         }
///         Err(Error::Auth(AuthError::PermissionDenied { operation, required_permission })) => {
///             eprintln!("Permission denied for {}: requires {:?}", operation, required_permission);
///             Err(Error::Auth(AuthError::PermissionDenied { operation, required_permission }))
///         }
///         Err(other) => {
///             eprintln!("Unexpected error: {}", other);
///             Err(other)
///         }
///     }
/// }
/// ```
pub fn error_handling_example() {}
````

### 3. **Performance-Aware Examples**

Include performance guidance in examples:

````rust
/// Efficiently process large datasets.
///
/// # Performance Example
/// ```rust
/// use eidetica::*;
/// use std::time::Instant;
///
/// fn bulk_operations_example(tree: &Tree) -> Result<()> {
///     let start = Instant::now();
///
///     // ✅ GOOD: Single operation for bulk changes
///     {
///         let mut op = tree.create_operation()?;
///         let dict = op.subtree::<Dict>("bulk_data")?;
///
///         // Process 10,000 items in one operation
///         for i in 0..10_000 {
///             dict.set(format!("item_{}", i), format!("value_{}", i))?;
///         }
///
///         op.commit()?;
///     }
///
///     println!("Bulk insert took: {:?}", start.elapsed());
///
///     // ❌ AVOID: Multiple operations for bulk changes
///     // This would be much slower:
///     // for i in 0..10_000 {
///     //     let mut op = tree.create_operation()?;
///     //     let dict = op.subtree::<Dict>("bulk_data")?;
///     //     dict.set(format!("item_{}", i), format!("value_{}", i))?;
///     //     op.commit()?;
///     // }
///
///     Ok(())
/// }
/// ```
pub fn performance_example() {}
````

## Internal Documentation

### 1. **Architecture Decision Records (ADRs)**

Document significant design decisions:

````markdown
# ADR-001: String Parameter Type Selection

## Status

Accepted

## Context

We need consistent patterns for string parameters across the API to optimize for both performance and ergonomics. Different operations have different string usage patterns - some store strings, others only access them temporarily.

## Decision

Use `impl Into<String>` for parameters that will be stored as owned strings.
Use `impl AsRef<str>` for parameters that are only accessed temporarily.

## Consequences

### Positive

- Optimal performance by avoiding double conversions
- Consistent API patterns across the codebase
- Clear intent about parameter usage in function signatures

### Negative

- Requires developers to understand the distinction
- More complex type signatures than simple `&str`

## Implementation

- Storage operations (set, insert, create): use `Into<String>`
- Lookup operations (get, contains, find): use `AsRef<str>`
- Document the pattern in API documentation

## Examples

```rust
// Storage operation
pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>)

// Lookup operation
pub fn get(&self, key: impl AsRef<str>) -> Option<&Value>
```
````

````

### 2. **Design Rationale Documentation**

Explain the reasoning behind complex implementations:

```rust
/// Compute CRDT state using Lowest Common Ancestor (LCA) algorithm.
///
/// # Algorithm Overview
///
/// This implementation uses a modified LCA algorithm to compute the merged
/// state of a CRDT from a set of tip entries. The algorithm works by:
///
/// 1. **Tip Collection**: Gather all current tip entries for the tree/subtree
/// 2. **Ancestor Traversal**: Walk backwards through the DAG to find common ancestors
/// 3. **LCA Identification**: Find the lowest common ancestor(s) of all tips
/// 4. **Forward Computation**: Compute state forward from LCA to each tip
/// 5. **Merge Resolution**: Apply CRDT merge semantics to resolve conflicts
///
/// # Why LCA-Based Computation?
///
/// We chose LCA-based computation over simpler approaches because:
///
/// - **Efficiency**: Only processes necessary entries, not the entire history
/// - **Correctness**: Guarantees deterministic results regardless of tip order
/// - **Scalability**: Performance scales with conflict depth, not total history
/// - **Caching**: Intermediate states can be cached for better performance
///
/// # Performance Characteristics
///
/// - **Time Complexity**: O(n * log(h)) where n is tip count, h is history depth
/// - **Space Complexity**: O(h) for the ancestor tracking
/// - **Cache Friendly**: Results can be cached by (entry_id, subtree_name)
///
/// # Trade-offs
///
/// - **Memory**: Uses more memory than simple linear computation
/// - **Complexity**: More complex than naive approaches
/// - **Correctness**: Worth the complexity for proper CRDT semantics
impl CRDTStateComputer {
    /// Implementation details...
    pub fn compute_state(&self, tips: &[ID]) -> Result<Map> {
        // Implementation here...
    }
}
````

### 3. **TODO and Future Work Documentation**

Document planned improvements and known limitations:

```rust
/// Current limitations and future improvements.
///
/// # Known Limitations
///
/// - **Memory Usage**: State computation keeps entire entry history in memory
/// - **Network Sync**: No built-in network synchronization (planned for v0.3)
/// - **Storage Backends**: Currently only supports in-memory storage
///
/// # Planned Improvements
///
/// ## Version 0.2
/// - [ ] Persistent storage backend (RocksDB/SQLite)
/// - [ ] State computation optimizations with caching
/// - [ ] Memory usage improvements for large histories
///
/// ## Version 0.3
/// - [ ] Network synchronization protocol
/// - [ ] Incremental state computation
/// - [ ] Multi-backend replication support
///
/// ## Future Considerations
/// - [ ] WebAssembly compilation support
/// - [ ] Zero-copy serialization optimizations
/// - [ ] Distributed consensus integration
///
/// # Contributing
///
/// To contribute to these improvements:
/// 1. Check the GitHub issues for current priorities
/// 2. Review the design documents in `/design/`
/// 3. Implement with comprehensive tests
/// 4. Update documentation with your changes
impl FutureImprovements {
    // TODO: Implement persistent storage backend
    // See design/storage_backends.md for requirements

    // TODO: Add network synchronization
    // Will require protocol design and security review

    // TODO: Optimize memory usage
    // Consider streaming computation for large histories
}
```

## Documentation Testing

### 1. **Doctests for All Examples**

Ensure all documentation examples compile and run:

````rust
/// Documentation with tested examples.
///
/// # Example
/// ```rust
/// use eidetica::*;
/// use eidetica::backend::database::InMemory;
///
/// # fn main() -> Result<()> {
/// let backend = Box::new(InMemory::new());
/// let mut db = BaseDB::new(backend);
/// db.add_private_key("test_key")?;
///
/// let tree = db.new_tree(Map::new(), "test_key")?;
/// assert_eq!(tree.root_id().len(), 64); // SHA-256 hex length
/// # Ok(())
/// # }
/// ```
pub fn documented_function() {}
````

**Testing Commands**:

```bash
# Test all documentation examples
cargo test --doc

# Test specific module documentation
cargo test --doc auth

# Test with verbose output
cargo test --doc -- --nocapture
```

### 2. **Documentation Coverage**

Track documentation coverage and completeness:

```bash
# Generate documentation with warnings for missing docs
RUSTDOCFLAGS="-D missing_docs" cargo doc

# Check for broken internal links
cargo doc --document-private-items

# Generate documentation statistics
cargo doc --open
```

## External Documentation

### 1. **User Guide Structure**

Organize user-facing documentation progressively:

```
docs/
├── user_guide/
│   ├── index.md              # Overview and getting started
│   ├── getting_started.md    # Quick tutorial
│   ├── core_concepts.md      # Fundamental concepts
│   ├── authentication_guide.md # Security and auth
│   ├── tutorial_todo_app.md  # Complete application tutorial
│   └── examples_snippets.md  # Code examples library
├── internal/
│   ├── best_practices/       # This documentation
│   ├── core_components/      # Architecture details
│   └── design/              # Design decisions and ADRs
└── api/                     # Generated API documentation
```

### 2. **Contribution Guidelines**

Document how to contribute to documentation:

````markdown
# Contributing to Documentation

## Documentation Types

### User Documentation (docs/user_guide/)

- Focus on solving user problems
- Include complete, working examples
- Test all code examples
- Use clear, non-technical language

### Internal Documentation (docs/internal/)

- Focus on implementation details
- Explain design decisions and trade-offs
- Include performance considerations
- Document future improvements and limitations

### API Documentation (inline in source)

- Document all public APIs
- Include usage examples for non-trivial functions
- Explain performance characteristics
- Link to related functionality

## Writing Guidelines

### Code Examples

- All examples must compile and run
- Use realistic data and scenarios
- Show error handling patterns
- Include performance notes for expensive operations

### Language and Style

- Use active voice
- Write for your audience (users vs contributors)
- Be concise but comprehensive
- Include context and rationale

## Review Process

1. All documentation changes require review
2. Test that examples compile: `cargo test --doc`
3. Check for broken links and references
4. Verify examples demonstrate best practices
5. Ensure consistency with existing documentation

## Tools and Setup

```bash
# Install documentation tools
cargo install mdbook

# Build and serve documentation locally
mdbook serve docs/

# Test documentation examples
cargo test --doc

# Check documentation coverage
RUSTDOCFLAGS="-D missing_docs" cargo doc
```
````

````

## Common Documentation Anti-Patterns

### ❌ **Outdated Examples**

```rust
// DON'T DO THIS - outdated API usage
/// Example:
/// ```rust
/// let tree = db.create_tree("name");  // Old API that no longer exists
/// ```
pub fn current_function() {}
````

### ❌ **Incomplete Examples**

````rust
// DON'T DO THIS - incomplete, non-compilable example
/// Example:
/// ```rust
/// let result = some_function();  // Missing imports, setup, error handling
/// ```
pub fn example_function() {}
````

### ❌ **Implementation-Focused Documentation**

```rust
// DON'T DO THIS - focuses on implementation instead of usage
/// This function uses a BTreeMap internally and iterates through entries
/// using the IntoIterator trait implementation to find matching keys.
pub fn find_entries() {}
```

### ❌ **Missing Context**

```rust
// DON'T DO THIS - no context about when/why to use
/// Computes the LCA.
pub fn compute_lca() {}
```

### ✅ **Good Documentation Patterns**

````rust
// DO THIS - clear purpose, complete example, proper context
/// Find entries matching a predicate function.
///
/// This function efficiently searches through tree entries and returns
/// those that match the provided criteria. Useful for filtering operations
/// and content queries.
///
/// # Parameters
/// * `predicate` - Function that returns true for entries to include
///
/// # Returns
/// Vector of entry IDs that match the predicate
///
/// # Example
/// ```rust
/// use eidetica::*;
/// use eidetica::backend::database::InMemory;
///
/// # fn main() -> Result<()> {
/// let backend = Box::new(InMemory::new());
/// let mut db = BaseDB::new(backend);
/// db.add_private_key("test_key")?;
/// let tree = db.new_tree(Map::new(), "test_key")?;
///
/// // Find entries modified recently
/// let recent_entries = tree.find_entries(|entry| {
///     entry.timestamp() > recent_cutoff
/// })?;
/// # Ok(())
/// # }
/// ```
///
/// # Performance
/// O(n) where n is the number of entries. Consider caching results
/// for expensive predicates or large entry sets.
pub fn find_entries<F>(&self, predicate: F) -> Result<Vec<ID>>
where
    F: Fn(&Entry) -> bool,
{
    // Implementation
}
````

## Future Documentation Improvements

### Planned Enhancements

- **TODO**: Implement automated documentation testing in CI
- **TODO**: Create interactive documentation with embedded examples
- **TODO**: Add video tutorials for complex workflows
- **TODO**: Generate API documentation with usage analytics
- **TODO**: Implement documentation versioning for API changes

### Documentation Tooling

- **TODO**: Automated link checking and validation
- **TODO**: Documentation coverage reporting
- **TODO**: Integration with code review tools
- **TODO**: Automated example updating with API changes

## Summary

Effective documentation in Eidetica provides:

- **Comprehensive API documentation** with examples and performance notes
- **Clear user guides** that solve real problems
- **Internal documentation** that explains design decisions
- **Tested examples** that compile and demonstrate best practices
- **Progressive disclosure** from overview to detailed reference
- **Audience-focused content** for users vs contributors

Following these patterns ensures documentation remains valuable, accurate, and maintainable as the project evolves.
