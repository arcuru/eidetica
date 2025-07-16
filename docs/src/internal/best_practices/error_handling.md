# Error Handling Best Practices

This document outlines the error handling patterns and practices used throughout the Eidetica codebase, focusing on structured errors, ergonomic APIs, and maintainable error propagation.

## Core Error Architecture

### 1. **Unified Result Type**

Eidetica uses a unified `Result<T>` type across the entire codebase with automatic conversion between module-specific errors and the main error type. This provides consistent error handling and a single import for Result type throughout the codebase.

### 2. **Module-Specific Error Types**

Each module defines its own structured error type with semantic helpers. Error types include contextual information and helper methods for classification (e.g., `is_authentication_error()`, `is_permission_denied()`).

## Error Design Patterns

### 1. **Semantic Error Classification**

Provide helper methods that allow callers to handle errors semantically, such as `is_not_found()`, `is_storage_error()`, or `is_corruption_error()`. This enables clean error handling based on error semantics rather than type matching.

### 2. **Contextual Error Information**

Include relevant context in error variants, such as available options when something is not found, or specific reasons for failures. This debugging information helps users understand what went wrong and can assist in error recovery.

### 3. **Error Conversion Patterns**

Use `#[from]` and `#[error(transparent)]` for zero-cost error conversion between module boundaries. This allows wrapping errors with additional context or passing them through directly.

### 4. **Non-Exhaustive Error Enums**

Use `#[non_exhaustive]` on error enums to allow adding new error variants in the future without breaking existing code.

## Error Handling Strategies

### 1. **Early Return with `?` Operator**

Use the `?` operator for clean error propagation, validating preconditions early and returning errors as soon as they occur.

### 2. **Error Context Enhancement**

Add context when propagating errors up the call stack by wrapping lower-level errors with higher-level context that explains what operation failed.

### 3. **Fallible Iterator Patterns**

Handle errors in iterator chains gracefully by either failing fast on the first error or collecting all results before handling errors, depending on the use case.

## Authentication Error Patterns

### 1. **Permission-Based Errors**

Structure authentication errors to be actionable by including what permission was required, what the user had, and potentially what options are available.

### 2. **Security Error Handling**

Be careful not to leak sensitive information in error messages. Reference resources by name or ID rather than content, and avoid exposing internal system details.

## Performance Considerations

### 1. **Error Allocation Optimization**

Minimize allocations in error creation by using static strings for fixed messages and avoiding unnecessary string formatting in hot paths.

### 2. **Error Path Optimization**

Keep error paths simple and fast by deferring error creation until actually needed, using closures with `ok_or_else()` rather than `ok_or()`.

## Testing Error Conditions

### 1. **Error Testing Patterns**

Test both error conditions and error classification helpers. Verify that error context is preserved through the error chain and that error messages contain expected information.

### 2. **Error Helper Testing**

Test semantic error classification helpers to ensure they correctly identify error categories and that the classification logic remains consistent as error types evolve.

## Common Anti-Patterns

- **String-based errors** - Avoid unstructured string errors that lack context
- **Generic error types** - Don't use overly generic errors that lose type information
- **Panic on recoverable errors** - Return Result instead of using unwrap() or expect()
- **Leaking sensitive information** - Don't expose internal details in error messages

## Migration Guidelines

When updating error handling, maintain error semantics, add context gradually, test error paths thoroughly, and keep documentation current.

## Summary

Effective error handling in Eidetica provides:

- **Structured error types** with rich context and classification
- **Consistent error patterns** across all modules
- **Semantic error helpers** for easy error handling in calling code
- **Zero-cost error conversion** between module boundaries
- **Performance-conscious** error creation and propagation
- **Testable error conditions** with comprehensive coverage

Following these patterns ensures errors are informative, actionable, and maintainable throughout the codebase evolution.
