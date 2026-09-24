# Terminology

Eidetica uses two naming schemes:

## Internal Data Structures

Trees and Subtrees. These align with the names used inside of an Entry:

- **TreeNode**: Main tree node within an Entry (root CID, parent references, metadata)
- **SubTreeNode**: Named subtree nodes within an Entry (name, parents, data payload)
- **ID**: A wrapper around a CID (Content Identifier). Entries are serialized to DAG-CBOR and hashed to produce their CID. The string representation uses multibase base32lower encoding (`bafyr4i...`).

Use these when discussing Entry internals, Merkle-DAG structure, or serialized data format.

## User-Facing Abstractions

- **Database**: Collection of entries with shared authentication and history
- **Store**: Typed data access (DocStore, Table, YDoc) operating on named subtrees
- **Projection**: A Store-defined representation of current CRDT state. The default is one opaque whole-state record; Table projects ordered LwwMap Entry deltas to per-row records.
- **Namespace**: A backend-owned set of ordered opaque Store-state records for one database, Store, projection, source, and trust scope.
- **Derived namespace**: Immutable disposable state materialized from historical Entries.
- **Authoritative namespace**: Durable current Store state that cache clearing cannot select.
- **Staging namespace**: Unpublished records used while building a complete namespace; readers cannot resolve it.

Use these in public APIs, user documentation, and error messages.

A historical Database is an abstraction over a Tree, and Stores are an abstraction over the Subtrees within. Historical reads may resolve a derived Store-state projection that can be cleared and rebuilt from Entries.
