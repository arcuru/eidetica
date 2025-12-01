# Terminology

Eidetica uses two naming schemes:

## Internal Data Structures

Trees and Subtrees. These align with the names used inside of an Entry:

- **TreeNode**: Main tree node within an Entry (root ID, parent references, metadata)
- **SubTreeNode**: Named subtree nodes within an Entry (name, parents, data payload)

Use these when discussing Entry internals, Merkle-DAG structure, or serialized data format.

## User-Facing Abstractions

- **Database**: Collection of entries with shared authentication and history
- **Store**: Typed data access (DocStore, Table, YDoc) operating on named subtrees

Use these in public APIs, user documentation, and error messages.

A Database is an abstraction over a Tree, and Stores are an abstraction over the Subtrees within.
