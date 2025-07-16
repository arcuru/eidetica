# Error Handling Design

## Overview

Error handling in Eidetica follows principles of modularity, locality, and user ergonomics using structured error types with zero-cost conversion.

## Design Philosophy

**Error Locality**: Each module owns its error types, keeping them discoverable alongside functions that produce them.

**Structured Error Data**: Uses typed fields instead of string-based errors for pattern matching, context preservation, and performance.

**Progressive Context**: Errors gain context moving up the stack - lower layers provide technical details, higher layers add user-facing categorization.

## Architecture

**Error Hierarchy**: Tree structure where modules define error types aggregated into top-level `Error` enum with variants for Io, Serialize, Auth, Backend, Base, CRDT, Subtree, and AtomicOp errors.

**Module-Specific Errors**: Each component has domain-specific error enums covering key resolution, storage operations, tree management, merge conflicts, data access, and transaction coordination.

**Transparent Conversion**: `#[error(transparent)]` enables zero-cost conversion between module errors and top-level type using `?` operator.

## Error Categories

**By Nature**: Not found errors (module-specific variants), permission errors (authentication/authorization), validation errors (input/state consistency), operation errors (business logic violations).

**By Layer**: Core errors (fundamental operations), storage layer (database/persistence), data layer (CRDT/subtree operations), application layer (high-level coordination).

## Error Handling Patterns

**Contextual Propagation**: Errors preserve context while moving up the stack, maintaining technical details and enabling categorization.

**Classification Helpers**: Top-level `Error` provides methods like `is_not_found()`, `is_permission_denied()`, `is_authentication_error()` for broad category handling.

**Non-Exhaustive Enums**: All error enums use `#[non_exhaustive]` for future extension without breaking changes.

## Performance

**Zero-Cost Abstractions**: Transparent errors eliminate wrapper overhead, structured fields avoid string formatting until display, no heap allocations in common paths.

**Efficient Propagation**: Seamless `?` operator across module boundaries with automatic conversion and preserved context.

## Usage Patterns

**Library Users**: Use helper methods for stable APIs that won't break with new error variants.

**Library Developers**: Define new variants in appropriate module enums with structured fields for context, add helper methods for classification.

## Extensibility

New error variants can be added without breaking existing code. Operations spanning modules can wrap/convert errors for appropriate context. Structured data enables sophisticated error recovery based on specific failure modes.
