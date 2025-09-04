# Architectural Terminology

This document clarifies the important distinction between internal data structure names and user-facing API abstractions in Eidetica's architecture.

## Overview

Eidetica uses two parallel naming schemes that serve different purposes:

1. **Internal Data Structures**: TreeNode/SubTreeNode - the actual Merkle-DAG data structures
2. **User-Facing Abstractions**: Database/Store - high-level views over these structures

Understanding this distinction is crucial for maintaining consistency in code, documentation, and APIs.

## Internal Data Structures

### TreeNode and SubTreeNode

These are the fundamental building blocks of the Merkle-DAG, defined within the `Entry` module:

- **`TreeNode`**: The internal representation of the main tree node within an Entry

  - Contains the root ID, parent references, and metadata
  - Represents the core structural data of the Merkle-DAG
  - Always singular per Entry

- **`SubTreeNode`**: The internal representation of named subtree nodes within an Entry
  - Contains subtree name, parent references, and data payload
  - Multiple SubTreeNodes can exist per Entry
  - Each represents a named partition of data (analogous to tables)

### When to Use Tree/SubTree Terminology

- When discussing the actual data structures within Entry
- In Entry module documentation and implementation
- When describing the Merkle-DAG at the lowest level
- In comments that deal with the serialized data format
- When explaining parent-child relationships in the DAG

## User-Facing Abstractions

### Database and Store

These represent the current high-level abstraction layer that users interact with:

- **`Database`**: A collection of related entries with shared authentication and history

  - Provides a view over a tree of entries
  - Manages operations, authentication, and synchronization
  - What users think of as a "database" or "collection"

- **`Store`**: Typed data access patterns within a database
  - DocStore, Table, YDoc are concrete Store implementations
  - Provide familiar APIs (key-value, document, collaborative editing)
  - Each Store operates on a named subtree within entries

### When to Use Database/Store Terminology

- In all public APIs and user-facing documentation
- In user guides, tutorials, and examples
- When describing current application-level concepts
- In error messages shown to users
- In logging that users might see

### Future Abstraction Layers

Database/Store represents the current abstraction over TreeNode/SubTreeNode structures, but it is not the only possible abstraction. Future versions of Eidetica may introduce alternative abstraction layers that provide different views or APIs over the same underlying layered Merkle-DAG structures.

The key principle is that TreeNode/SubTreeNode remain the stable internal representation, while various abstractions can be built on top to serve different use cases or API paradigms.

## The Relationship

```text
User Application
       ↓
    Database  ←─ User-facing abstraction
       ↓
   Transaction ←─ Operations layer
       ↓
     Entry    ←─ Contains TreeNode + SubTreeNodes
       ↓         (internal data structures)
   Backend    ←─ Storage layer
```

- A `Database` provides operations over a tree of `Entry` objects
- Each `Entry` contains one `TreeNode` and multiple `SubTreeNode` structures
- `Store` implementations provide typed access to specific `SubTreeNode` data
- Users never directly interact with TreeNode/SubTreeNode

## Code Guidelines

### Internal Implementation

```rust,ignore
// Correct - dealing with Entry internals
entry.tree.root  // TreeNode field
entry.subtrees.iter()  // SubTreeNode collection
builder.set_subtree_data_mut()  // Working with subtree data structures
```

### Public APIs

```rust,ignore
// Correct - user-facing abstractions
database.new_operation()  // Database operations
transaction.get_store::<DocStore>("users")  // Store access
instance.create_database("mydata")  // Database management
```

### Documentation

- **Internal docs**: Can reference both levels, explaining their relationship
- **User guides**: Only use Database/Store terminology
- **API docs**: Use Database/Store exclusively
- **Code comments**: Use appropriate terminology for the level being discussed

## Rationale

This dual naming scheme serves several important purposes:

1. **Separation of Concerns**: Internal structures focus on correctness and efficiency, while abstractions focus on usability

2. **API Stability**: Users interact with stable Database/Store concepts, while internal TreeNode/SubTreeNode structures can evolve

3. **Conceptual Clarity**: Users think in terms of databases and data stores, not Merkle-DAG nodes

4. **Implementation Flexibility**: Internal refactoring doesn't affect user-facing terminology

5. **Domain Appropriateness**: Tree/Subtree accurately describes the Merkle-DAG structure, while Database/Store matches user mental models
