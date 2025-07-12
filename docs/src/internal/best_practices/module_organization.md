# Module Organization

This document outlines best practices for organizing code modules within the Eidetica codebase, focusing on clear separation of concerns, consistent structure, and maintainable hierarchies.

## Module Hierarchy Principles

### 1. **Domain-Driven Organization**

Organize modules around business domains and functionality rather than technical layers:

```
src/
├── lib.rs              # Public API and unified Result type
├── basedb/             # Database management
├── tree.rs             # Tree operations and management
├── entry/              # Entry creation and manipulation
├── backend/            # Storage abstraction layer
├── subtree/            # Specialized data access patterns
├── auth/               # Authentication and authorization
├── crdt/               # CRDT implementations and algorithms
├── atomicop/           # Atomic operations and transactions
└── data/               # Data structures and serialization
```

**Rationale**: Each module has a clear responsibility and can evolve independently while maintaining clean boundaries.

### 2. **Consistent Module Structure**

Every module should follow a standard internal structure:

```
module_name/
├── mod.rs              # Public API, re-exports, module docs
├── errors.rs           # Module-specific error types
├── implementation.rs   # Core implementation logic
├── helpers.rs          # Utility functions (if needed)
└── tests.rs           # Module-specific unit tests (if any)
```

**Key practices**:

- **`mod.rs`** contains only public API definitions, re-exports, and module-level documentation
- **`errors.rs`** defines structured error types with semantic helper methods
- **`implementation.rs`** contains the main implementation logic
- Keep related functionality together within the same module

### 3. **Error Module Standards**

Each module must define its own error type following this pattern:

```rust
// In module_name/errors.rs
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModuleNameError {
    #[error("Specific error message: {details}")]
    SpecificError { details: String },

    #[error("Resource not found: {resource_type}")]
    NotFound { resource_type: String },

    #[error(transparent)]
    DependencyError(#[from] dependency::Error),
}

impl ModuleNameError {
    /// Semantic helper methods for error classification
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    pub fn is_dependency_error(&self) -> bool {
        matches!(self, Self::DependencyError(_))
    }
}
```

**Best practices**:

- Use `#[non_exhaustive]` for future compatibility
- Provide semantic helper methods for common error checking
- Use `#[error(transparent)]` for dependency error delegation
- Include contextual information in error variants

## Public API Design

### 1. **Clean Re-exports**

Module `mod.rs` files should provide clean public APIs:

```rust
// In module_name/mod.rs

//! Module documentation with purpose and usage examples
//!
//! This module provides [clear description of functionality].
//! Key components include:
//! - `MainStruct`: Primary interface for [functionality]
//! - `HelperType`: Support type for [specific use case]

mod errors;
mod implementation;

// Public API
pub use errors::ModuleNameError;
pub use implementation::{MainStruct, PublicHelper};

// Re-export commonly used types for convenience
pub use crate::common::SharedType;
```

### 2. **Module Documentation Standards**

Every module should have comprehensive documentation:

````rust
//! # Module Name
//!
//! Brief description of the module's purpose and scope.
//!
//! ## Core Functionality
//!
//! - **Primary feature**: Description and use cases
//! - **Secondary feature**: Description and use cases
//!
//! ## Usage Examples
//!
//! ```rust
//! use eidetica::module_name::MainStruct;
//!
//! let instance = MainStruct::new()?;
//! instance.primary_operation()?;
//! ```
//!
//! ## Integration Points
//!
//! This module integrates with:
//! - `other_module`: For [specific interaction]
//! - `external_crate`: For [specific functionality]
//!
//! ## Performance Considerations
//!
//! - Hot path operations: [list critical performance paths]
//! - Memory usage: [describe memory allocation patterns]
//! - Concurrency: [describe thread safety guarantees]
````

## Dependency Management

### 1. **Dependency Direction**

Maintain clear dependency hierarchies to avoid circular dependencies:

```
High-level modules (depend on lower-level)
├── basedb/     ← depends on tree, backend, auth
├── tree.rs     ← depends on entry, atomicop, crdt
├── subtree/    ← depends on crdt, atomicop
└── Low-level modules (minimal dependencies)
    ├── entry/      ← depends on data, auth
    ├── backend/    ← depends on data
    ├── crdt/       ← depends on data (minimal)
    ├── auth/       ← depends on data (minimal)
    └── data/       ← no internal dependencies
```

**Rules**:

- Higher-level modules can depend on lower-level modules
- Modules at the same level should avoid direct dependencies
- Use trait abstractions to break circular dependencies when needed

### 2. **Feature Gating**

Use feature flags for optional functionality:

```rust
// In Cargo.toml
[features]
default = ["std"]
std = []
y-crdt = ["yrs"]
benchmarks = ["criterion"]

// In source code
#[cfg(feature = "y-crdt")]
pub mod yrs_store;

#[cfg(feature = "y-crdt")]
pub use yrs_store::YrsStore;
```

## Module Communication Patterns

### 1. **Trait-Based Abstractions**

Use traits to define interfaces between modules:

```rust
// In backend/mod.rs
pub trait Database {
    fn store_entry(&mut self, entry: &Entry) -> Result<()>;
    fn get_entry(&self, id: &ID) -> Result<Option<Entry>>;
    // ...
}

// Implementation modules can then depend on the trait rather than concrete types
```

### 2. **Event-Driven Communication**

For decoupled communication, consider event patterns:

```rust
// TODO: Investigate event-driven patterns for module communication
// This could be useful for logging, metrics, or cross-cutting concerns
// without introducing tight coupling between modules
```

## Testing Integration

### 1. **Test Module Organization**

Integration tests should mirror the module structure:

```
tests/it/
├── mod.rs              # Test helpers and shared utilities
├── basedb/             # BaseDB integration tests
├── tree/               # Tree operation tests
├── auth/               # Authentication tests
└── helpers.rs          # Shared test utilities
```

### 2. **Module-Specific Helpers**

Each test module should provide helpers for that domain:

```rust
// In tests/it/auth/helpers.rs
pub fn setup_authenticated_tree() -> (BaseDB, Tree) {
    // Setup code specific to auth testing
}

pub fn create_test_keys(count: usize) -> Vec<String> {
    // Key generation for testing
}
```

## Future Improvements

### Planned Enhancements

- **TODO**: Investigate dependency injection patterns for better testability
- **TODO**: Consider module-level configuration patterns
- **TODO**: Evaluate async/await integration points for future async support
- **TODO**: Design patterns for plugin-like module extensions

### Architecture Evolution

As the codebase grows, consider these patterns:

1. **Plugin Architecture**: Allow external modules to extend core functionality
2. **Service Layer**: Abstract business logic from data access patterns
3. **Event Sourcing**: Consider event-driven architecture for audit trails
4. **Module Versioning**: Plan for internal API versioning between modules

## Common Anti-Patterns to Avoid

### ❌ **Circular Dependencies**

```rust
// DON'T DO THIS
mod a {
    use crate::b::BType;  // A depends on B
}

mod b {
    use crate::a::AType;  // B depends on A - CIRCULAR!
}
```

### ❌ **God Modules**

```rust
// DON'T DO THIS - everything in one module
mod everything {
    pub struct Database { /* ... */ }
    pub struct Entry { /* ... */ }
    pub struct Authentication { /* ... */ }
    pub struct CRDT { /* ... */ }
    // 1000+ lines of unrelated code
}
```

### ❌ **Leaky Abstractions**

```rust
// DON'T DO THIS - exposing internal details
pub mod internal_implementation {
    pub struct InternalState { /* ... */ }  // Should be private
}

pub fn public_api() -> internal_implementation::InternalState {
    // Exposing internal types through public API
}
```

### ✅ **Correct Patterns**

```rust
// DO THIS - clean abstractions
pub trait PublicInterface {
    fn operation(&self) -> Result<PublicResult>;
}

mod internal_implementation {
    struct InternalState { /* ... */ }  // Private

    pub struct PublicImplementation {
        internal: InternalState,  // Hidden
    }

    impl super::PublicInterface for PublicImplementation {
        fn operation(&self) -> Result<super::PublicResult> {
            // Implementation hidden behind interface
        }
    }
}
```

## Migration Guidelines

When restructuring existing modules:

1. **Plan the new structure** before making changes
2. **Use deprecation warnings** for public API changes (when compatibility becomes a concern)
3. **Create integration tests** that verify the restructuring doesn't break functionality
4. **Update documentation** to reflect the new organization
5. **Consider backward compatibility** implications for public APIs

## Summary

Good module organization provides:

- **Clear separation of concerns** with well-defined boundaries
- **Predictable structure** that developers can navigate easily
- **Maintainable dependencies** with clear hierarchies
- **Testable interfaces** with appropriate abstractions
- **Extensible design** that can grow with the project

Following these patterns ensures the codebase remains organized and maintainable as it evolves.
