# Introduction

Welcome to Eidetica, a Rust database library designed for applications that need structured data storage with built-in history tracking.

## What is Eidetica?

Eidetica is a specialized database library that combines concepts from modern distributed systems with traditional database design to create a unique approach to data management. The name "Eidetica" relates to eidetic memory (the ability to recall images, sounds, or objects with extreme accuracy) - reflecting the database's comprehensive history-tracking capabilities.

## Key Features

- **History Tracking**: Every change is preserved, allowing you to view the state of your data at any point in time.
- **Structured Data**: Type-safe storage for different kinds of data through specialized subtrees.
- **Atomic Operations**: Complex changes across multiple data structures are committed as a single transaction.
- **Content-Addressable**: Data is identified by its content, ensuring integrity and enabling efficient synchronization.
- **Designed for Distribution**: Architecture supports eventual consistency and conflict resolution (future capability).

## When to Use Eidetica

Eidetica is particularly well-suited for applications that:

- Need audit trails or comprehensive history of all data changes
- Require structured, type-safe data storage
- Want atomic operations across different data structures
- Need eventual consistency in distributed environments (future capability)
- Value data integrity and verifiability

## Library Status

Eidetica is currently under active development. While the core functionality described in this guide is working, APIs may evolve as the project matures.

## How This Guide is Organized

This User Guide is organized to help you learn and use Eidetica effectively:

- [Getting Started](getting_started.md): Quick setup and basic operations
- [Core Concepts](core_concepts.md): Understanding Eidetica's architecture
- Subtopic pages on specific concepts:
  - [Entries & Trees](concepts/entries_trees.md)
  - [Backends](concepts/backends.md)
  - [Subtrees](concepts/subtrees.md)
- [Examples](examples_snippets.md): Real-world usage examples

We recommend starting with the [Getting Started](getting_started.md) guide to set up your first Eidetica instance, then exploring the [Core Concepts](core_concepts.md) to better understand the unique capabilities of this database system.
