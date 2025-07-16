# API Design Patterns

This document outlines established patterns for API design within the Eidetica codebase, with particular emphasis on string parameter handling, conversion patterns, and performance considerations.

## String Parameter Guidelines

One of the most important API design decisions in Rust is choosing the right parameter types for string data. Eidetica follows specific patterns to optimize performance while maintaining ergonomic APIs.

### Core Principle: Storage vs Lookup Pattern

The fundamental rule for string parameters in Eidetica:

- **Use `Into<String>`** for parameters that will be **stored** (converted to owned `String`)
- **Use `AsRef<str>`** for parameters that are only **accessed** temporarily (lookup, comparison)

### When to Use `Into<String>`

Use `impl Into<String>` when the function will store the parameter as an owned `String`. This avoids double conversion and is more efficient for storage operations while still accepting `&str`, `String`, and `&String` transparently.

### When to Use `AsRef<str>`

Use `impl AsRef<str>` when the function only needs to read the string temporarily for lookups, comparisons, or validation. This provides maximum flexibility with no unnecessary allocations and clearly indicates the parameter is not stored.

### Anti-Patterns to Avoid

Never use `AsRef<str>` followed by immediate `.to_string()` - this causes double conversion. Instead, use `Into<String>` for direct conversion when storing the value.

## Common Conversion Patterns

### ID Types

For ID parameters, prefer `Into<ID>` when working with ID-typed fields for clear intent and type safety.

### Path Segments

For path operations, use `Into<String>` with `Clone` bounds when segments will be stored as keys.

## Performance Guidelines

### Hot Path Optimizations

For performance-critical operations:

1. **Bulk Operations**: Convert all parameters upfront to avoid per-iteration conversions
2. **Iterator Chains**: Prefer direct loops over complex iterator chains in hot paths

## API Documentation Standards

Always document the expected usage pattern for string parameters, indicating whether the parameter will be stored or just accessed, and which string types are accepted.

## Testing Patterns

Ensure APIs work with all string types (`&str`, `String`, `&String`) by testing conversion compatibility.

## API Evolution Guidelines

During development, APIs can be freely changed to follow best practices. Update methods directly with improved parameter types, add comprehensive tests, update documentation, and consider performance impact. Breaking changes are acceptable when they improve performance, ergonomics, or consistency.

## Summary

Following these patterns ensures:

- **Optimal performance** through minimal conversions
- **Consistent APIs** across the codebase
- **Clear intent** about parameter usage
- **Maximum flexibility** for API consumers
- **Maintainable code** for future development

When in doubt, ask: "Is this parameter stored or just accessed?" The answer determines whether to use `Into<String>` or `AsRef<str>`.
