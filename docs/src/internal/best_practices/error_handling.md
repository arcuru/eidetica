# Error Handling Best Practices

This document outlines the error handling patterns and practices used throughout the Eidetica codebase, focusing on structured errors, ergonomic APIs, and maintainable error propagation.

## Core Error Architecture

### 1. **Unified Result Type**

Eidetica uses a unified `Result<T>` type across the entire codebase:

```rust
// In lib.rs
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Auth(#[from] auth::AuthError),

    #[error(transparent)]
    Backend(#[from] backend::DatabaseError),

    #[error(transparent)]
    Base(#[from] basedb::BaseError),

    #[error(transparent)]
    CRDT(#[from] crdt::CRDTError),

    #[error(transparent)]
    Subtree(#[from] subtree::SubtreeError),
}
```

**Benefits**:

- Consistent error handling across all modules
- Automatic conversion between module-specific errors and the main error type
- Single import for Result type throughout the codebase

### 2. **Module-Specific Error Types**

Each module defines its own structured error type with semantic helpers:

```rust
// Example: auth/errors.rs
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    #[error("Authentication key not found: {key_name}")]
    KeyNotFound { key_name: String },

    #[error("Invalid signature for key: {key_name}")]
    InvalidSignature { key_name: String },

    #[error("Permission denied: {operation} requires {required_permission:?}")]
    PermissionDenied {
        operation: String,
        required_permission: Permission,
    },

    #[error("Invalid key format: {details}")]
    InvalidKeyFormat { details: String },
}

impl AuthError {
    /// Check if error indicates missing authentication
    pub fn is_authentication_error(&self) -> bool {
        matches!(self,
            Self::KeyNotFound { .. } |
            Self::InvalidSignature { .. } |
            Self::InvalidKeyFormat { .. }
        )
    }

    /// Check if error indicates insufficient permissions
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied { .. })
    }
}
```

## Error Design Patterns

### 1. **Semantic Error Classification**

Provide helper methods that allow callers to handle errors semantically:

```rust
// Backend error classification
impl DatabaseError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::EntryNotFound { .. })
    }

    pub fn is_storage_error(&self) -> bool {
        matches!(self, Self::StorageError { .. })
    }

    pub fn is_corruption_error(&self) -> bool {
        matches!(self, Self::CorruptedData { .. })
    }
}

// Usage in calling code
match tree.get_entry(id) {
    Ok(entry) => process_entry(entry),
    Err(e) if e.is_not_found() => handle_missing_entry(),
    Err(e) if e.is_storage_error() => retry_operation(),
    Err(e) => handle_unexpected_error(e),
}
```

### 2. **Contextual Error Information**

Include relevant context in error variants:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SubtreeError {
    #[error("Subtree '{name}' not found in entry {entry_id}")]
    SubtreeNotFound {
        name: String,
        entry_id: String,
        available_subtrees: Vec<String>,  // Helpful context
    },

    #[error("Invalid path '{path}' in subtree '{subtree_name}': {reason}")]
    InvalidPath {
        path: String,
        subtree_name: String,
        reason: String,
    },
}
```

**Benefits**:

- Debugging information included in error messages
- Context helps users understand what went wrong
- Additional fields can assist in error recovery

### 3. **Error Conversion Patterns**

Use `#[from]` and `#[error(transparent)]` for zero-cost error conversion:

```rust
#[derive(Debug, thiserror::Error)]
pub enum BaseError {
    #[error("Tree creation failed: {tree_name}")]
    TreeCreationFailed {
        tree_name: String,
        #[source]
        source: backend::DatabaseError,  // Wrapped error
    },

    #[error(transparent)]
    AuthError(#[from] auth::AuthError),  // Direct passthrough

    #[error(transparent)]
    BackendError(#[from] backend::DatabaseError),
}
```

### 4. **Non-Exhaustive Error Enums**

Use `#[non_exhaustive]` for future compatibility:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]  // Allows adding variants without breaking changes
pub enum CRDTError {
    #[error("Merge conflict in {operation}: {details}")]
    MergeConflict { operation: String, details: String },

    #[error("Invalid CRDT state: {reason}")]
    InvalidState { reason: String },

    // Can add more variants in the future without breaking compatibility
}
```

## Error Handling Strategies

### 1. **Early Return with `?` Operator**

Use the `?` operator for clean error propagation:

```rust
pub fn create_authenticated_tree(&mut self, data: Map, key_name: &str) -> Result<Tree> {
    // Validate key exists - early return if not
    self.validate_key_exists(key_name)?;

    // Create tree - early return on failure
    let tree = self.create_tree(data)?;

    // Set authentication - early return on failure
    tree.set_default_auth_key(key_name)?;

    Ok(tree)
}
```

### 2. **Error Context Enhancement**

Add context when propagating errors up the call stack:

```rust
impl Tree {
    pub fn create_operation(&self) -> Result<AtomicOp> {
        let entry_builder = EntryBuilder::new(&self.root_id)
            .map_err(|e| BaseError::TreeCreationFailed {
                tree_name: self.name.clone(),
                source: e,
            })?;

        Ok(AtomicOp::new(entry_builder))
    }
}
```

### 3. **Fallible Iterator Patterns**

Handle errors in iterator chains gracefully:

```rust
pub fn get_multiple_entries<I>(&self, entry_ids: I) -> Result<Vec<Entry>>
where
    I: IntoIterator<Item = ID>,
{
    let mut entries = Vec::new();

    for id in entry_ids {
        let entry = self.backend.get(&id)
            .map_err(|e| BaseError::EntryRetrievalFailed {
                entry_id: id.to_string(),
                source: e,
            })?;
        entries.push(entry);
    }

    Ok(entries)
}

// Alternative: Collect results and handle errors
pub fn get_multiple_entries_alt<I>(&self, entry_ids: I) -> Result<Vec<Entry>>
where
    I: IntoIterator<Item = ID>,
{
    entry_ids
        .into_iter()
        .map(|id| self.backend.get(&id))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| BaseError::MultipleEntryRetrievalFailed { source: e })
}
```

## Authentication Error Patterns

### 1. **Permission-Based Errors**

Structure authentication errors to be actionable:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Operation '{operation}' requires {required:?} permission, but key '{key_name}' has {actual:?}")]
    InsufficientPermission {
        operation: String,
        key_name: String,
        required: Permission,
        actual: Permission,
    },

    #[error("Key '{key_name}' is not authorized for tree '{tree_name}'")]
    UnauthorizedForTree {
        key_name: String,
        tree_name: String,
        available_trees: Vec<String>,  // Help user understand options
    },
}
```

### 2. **Security Error Handling**

Be careful not to leak sensitive information in error messages:

```rust
// ✅ GOOD: Generic error that doesn't leak key details
#[error("Authentication failed")]
AuthenticationFailed,

// ❌ BAD: Leaks information about key existence
#[error("Private key '{private_key_content}' not found")]
PrivateKeyNotFound { private_key_content: String },

// ✅ GOOD: References key name without exposing content
#[error("Key '{key_name}' not found")]
KeyNotFound { key_name: String },
```

## Performance Considerations

### 1. **Error Allocation Optimization**

Minimize allocations in error creation:

```rust
// ✅ GOOD: Use &str for static messages
#[derive(Debug, thiserror::Error)]
pub enum OptimizedError {
    #[error("Invalid state")]  // No allocation
    InvalidState,

    #[error("Not found: {resource}")]  // Single allocation for context
    NotFound { resource: String },
}

// ❌ LESS EFFICIENT: Unnecessary string formatting in hot paths
pub fn create_error_inefficient(details: &str) -> OptimizedError {
    OptimizedError::NotFound {
        resource: format!("Resource: {}", details),  // Extra allocation
    }
}

// ✅ BETTER: Direct string usage
pub fn create_error_efficient(resource: String) -> OptimizedError {
    OptimizedError::NotFound { resource }  // Direct move
}
```

### 2. **Error Path Optimization**

Keep error paths simple and fast:

```rust
// Hot path function
pub fn get_cached_value(&self, key: &str) -> Result<&Value> {
    // Fast path - no error allocation unless needed
    self.cache.get(key).ok_or_else(|| {
        // Only allocate error when needed
        CacheError::NotFound {
            key: key.to_string()
        }
    })
}
```

## Testing Error Conditions

### 1. **Error Testing Patterns**

Test both error conditions and error details:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authentication_error_classification() {
        let error = AuthError::KeyNotFound {
            key_name: "test_key".to_string()
        };

        assert!(error.is_authentication_error());
        assert!(!error.is_permission_denied());
    }

    #[test]
    fn test_error_context_preservation() {
        let backend_error = DatabaseError::EntryNotFound {
            id: "test_id".to_string()
        };

        let base_error = BaseError::EntryRetrievalFailed {
            entry_id: "test_id".to_string(),
            source: backend_error,
        };

        // Verify error chain is preserved
        assert!(base_error.source().is_some());
        assert!(base_error.to_string().contains("test_id"));
    }
}
```

### 2. **Error Helper Testing**

Test semantic error classification helpers:

```rust
#[test]
fn test_error_classification_helpers() {
    let auth_errors = vec![
        AuthError::KeyNotFound { key_name: "key1".to_string() },
        AuthError::InvalidSignature { key_name: "key2".to_string() },
    ];

    for error in auth_errors {
        assert!(error.is_authentication_error());
        assert!(!error.is_permission_denied());
    }

    let permission_error = AuthError::PermissionDenied {
        operation: "write".to_string(),
        required_permission: Permission::Write,
    };

    assert!(!permission_error.is_authentication_error());
    assert!(permission_error.is_permission_denied());
}
```

## Common Anti-Patterns

### ❌ **String-Based Errors**

```rust
// DON'T DO THIS
pub fn bad_function() -> Result<String> {
    Err("Something went wrong".to_string())  // No structure, no context
}
```

### ❌ **Generic Error Types**

```rust
// DON'T DO THIS
#[derive(Debug)]
pub enum GenericError {
    SomethingWentWrong(String),  // Too generic
    Error(Box<dyn std::error::Error>),  // Loses type information
}
```

### ❌ **Panic on Recoverable Errors**

```rust
// DON'T DO THIS
pub fn risky_function(id: &str) -> Entry {
    self.backend.get(id).unwrap()  // Panic on missing entry
}

// ✅ DO THIS INSTEAD
pub fn safe_function(&self, id: &str) -> Result<Entry> {
    self.backend.get(id)
        .map_err(|e| BaseError::EntryRetrievalFailed {
            entry_id: id.to_string(),
            source: e,
        })
}
```

## Future Improvements

### Planned Enhancements

- **TODO**: Investigate structured logging integration with error context
- **TODO**: Consider error recovery strategies for transient failures
- **TODO**: Evaluate async error handling patterns for future async support
- **TODO**: Design error aggregation patterns for bulk operations

### Error Reporting Evolution

- **TODO**: Consider error telemetry and metrics collection
- **TODO**: Evaluate user-facing error message localization
- **TODO**: Design error documentation generation from error types

## Migration Guidelines

When updating error handling:

1. **Maintain error semantics** - preserve the meaning of existing error classifications
2. **Add context gradually** - enhance error information without breaking existing error handling
3. **Test error paths** - ensure new error handling doesn't break error recovery logic
4. **Update documentation** - keep error handling examples current

## Summary

Effective error handling in Eidetica provides:

- **Structured error types** with rich context and classification
- **Consistent error patterns** across all modules
- **Semantic error helpers** for easy error handling in calling code
- **Zero-cost error conversion** between module boundaries
- **Performance-conscious** error creation and propagation
- **Testable error conditions** with comprehensive coverage

Following these patterns ensures errors are informative, actionable, and maintainable throughout the codebase evolution.
