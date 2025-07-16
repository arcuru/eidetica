# Documentation Best Practices

This document outlines documentation standards and practices used throughout the Eidetica codebase.

## Documentation Philosophy

### Documentation as Code

Documentation receives the same rigor as source code - version controlled, reviewed, tested, and maintained alongside code changes.

### Audience-Focused Writing

Each documentation type serves specific audiences: public API docs for library users, internal docs for contributors, architecture docs for design understanding, and best practices for development consistency.

### Progressive Disclosure

Information flows from general to specific: overview to getting started to detailed guides to reference documentation.

## API Documentation Standards

### Module-Level Documentation

Every module requires comprehensive header documentation including core functionality, integration points, security considerations, and performance notes. Module docs should provide an overview of the module's purpose and how it fits into the larger system.

### Function Documentation Standards

Document all public functions with: purpose description, parameter details, performance notes, related functions, and error conditions. Focus on what the function does and when to use it, not implementation details.

### Type Documentation

Document structs, enums, and traits with context about their purpose, usage patterns, and implementation notes. Focus on when and why to use the type, not just what it does.

### Error Documentation

Document error types with context about when they occur, what they mean, and how to recover from them. Include security implications where relevant.

## Code Example Standards

All documentation examples must be complete, runnable, and testable. Examples should demonstrate proper error handling patterns and include performance guidance where relevant. Use realistic scenarios and show best practices.

## Internal Documentation

### Architecture Decision Records (ADRs)

Document significant design decisions with status, context, decision rationale, and consequences. ADRs help future contributors understand why specific choices were made.

### Design Rationale Documentation

Complex implementations should include explanations of algorithm choices, performance characteristics, and trade-offs. Focus on the "why" behind implementation decisions.

### TODO and Known Limitations

Document current limitations and planned improvements with clear categorization. Include guidance for contributors who want to help address these items.

## Documentation Testing

### Doctests

All documentation examples must compile and run. Use `cargo test --doc` to verify examples work correctly. Examples should include proper imports and error handling.

### Documentation Coverage

Track coverage with `RUSTDOCFLAGS="-D missing_docs" cargo doc` to ensure all public APIs are documented. Check for broken links and maintain comprehensive documentation coverage.

## External Documentation

### User Guide Structure

Organize documentation progressively from overview to detailed reference. Structure includes user guides for problem-solving, internal docs for implementation details, and generated API documentation.

### Contribution Guidelines

Different documentation types serve different purposes: user docs focus on solving problems with clear examples, internal docs explain implementation decisions, and API docs provide comprehensive reference material. All examples must compile and demonstrate best practices.

## Common Documentation Anti-Patterns

Avoid outdated examples that no longer work with current APIs, incomplete examples missing imports or setup, implementation-focused documentation that explains how rather than what and why, and missing context about when to use functionality.

Good documentation provides clear purpose, complete examples, proper context, parameter descriptions, return value information, and performance characteristics.

## Summary

Effective documentation in Eidetica treats documentation as code, focuses on specific audiences, uses progressive disclosure, maintains comprehensive API documentation, provides clear user guides, explains design decisions, ensures all examples are tested and working, and follows consistent standards. These practices ensure documentation remains valuable, accurate, and maintainable as the project evolves.
