# Module Organization

This document outlines best practices for organizing code modules within the Eidetica codebase, focusing on clear separation of concerns, consistent structure, and maintainable hierarchies.

## Module Hierarchy Principles

### 1. **Domain-Driven Organization**

Organize modules around business domains and functionality rather than technical layers. Each module should have a clear responsibility and evolve independently while maintaining clean boundaries.

### 2. **Consistent Module Structure**

Every module should follow a standard internal structure with `mod.rs` for public API and re-exports, `errors.rs` for module-specific error types, and separate files for implementation logic. Keep related functionality together within the same module.

### 3. **Error Module Standards**

Each module must define its own error type with `#[non_exhaustive]` for future compatibility, semantic helper methods for error classification, transparent delegation for dependency errors, and contextual information in error variants.

## Public API Design

### 1. **Clean Re-exports**

Module `mod.rs` files should provide clean public APIs with clear documentation, selective re-exports of public types, and convenient access to commonly used shared types.

### 2. **Module Documentation Standards**

Every module should have comprehensive documentation including purpose, core functionality, usage examples, integration points, and performance considerations.

## Dependency Management

### 1. **Dependency Direction**

Maintain clear dependency hierarchies where higher-level modules depend on lower-level modules, modules at the same level avoid direct dependencies, and trait abstractions break circular dependencies when needed.

### 2. **Feature Gating**

Use feature flags for optional functionality, gating modules and exports appropriately with `#[cfg(feature = "...")]` attributes.

## Module Communication Patterns

### 1. **Trait-Based Abstractions**

Use traits to define interfaces between modules, allowing implementation modules to depend on abstractions rather than concrete types.

### 2. **Event-Driven Communication**

Consider event patterns for decoupled communication, particularly useful for logging, metrics, or cross-cutting concerns without introducing tight coupling.

## Testing Integration

Integration tests should mirror the module structure with module-specific helpers for each domain. Test organization should follow the same hierarchy as the source modules.

## Common Anti-Patterns to Avoid

- **Circular Dependencies** - Modules depending on each other in cycles
- **God Modules** - Single modules containing unrelated functionality
- **Leaky Abstractions** - Exposing internal implementation details through public APIs
- **Flat Structure** - No hierarchy or organization in module layout
- **Mixed Concerns** - Business logic mixed with infrastructure code

## Migration Guidelines

When restructuring modules: plan the new structure, use deprecation warnings for API changes when needed, create integration tests to verify functionality, update documentation, and consider backward compatibility implications.

## Summary

Good module organization provides:

- **Clear separation of concerns** with well-defined boundaries
- **Predictable structure** that developers can navigate easily
- **Maintainable dependencies** with clear hierarchies
- **Testable interfaces** with appropriate abstractions
- **Extensible design** that can grow with the project

Following these patterns ensures the codebase remains organized and maintainable as it evolves.
