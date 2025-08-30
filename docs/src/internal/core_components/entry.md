# Entry

The fundamental building block of Eidetica's data model, representing an immutable, cryptographically-signed unit of data within the Merkle-DAG structure.

## Conceptual Role

Entries serve as the atomic units of both data storage and version history in Eidetica. They combine the functions of:

- **Data Container**: Holding actual application data and metadata
- **Version Node**: Linking to parent entries to form a history DAG
- **Authentication Unit**: Cryptographically signed to ensure integrity and authorization
- **Content-Addressable Object**: Uniquely identified by their content hash for deduplication and verification

## Structural Organization

**Main Tree Data**: Each entry contains data for its parent Tree, stored as serialized CRDT structures that can be merged with concurrent changes.

**Subtree Partitioning**: Entries can contain multiple named subtrees (like DocStore, Table, YDoc), each with independent parent linkage and merge semantics.

**Parent References**: Entries link to previous entries, creating a directed acyclic graph that represents the evolution of data over time.

**Authentication Envelope**: Every entry includes signature information that proves authorization and ensures tamper-detection.

## Identity and Integrity

**Content-Addressable Identity**: Each entry's ID is a SHA-256 hash of its canonical content, making entries globally unique and enabling efficient deduplication.

**Deterministic Hashing**: IDs are computed from a canonical JSON representation, ensuring identical entries produce identical IDs across different systems.

**Immutability Guarantee**: Once created, entries cannot be modified, ensuring the integrity of the historical record and cryptographic signatures.

## Design Benefits

**Distributed Synchronization**: Content-addressable IDs enable efficient sync protocols where systems can identify missing or conflicting entries.

**Cryptographic Verification**: Signed entries provide strong guarantees about data authenticity and integrity.

**Granular History**: The DAG structure enables sophisticated queries like "show me all changes since timestamp X" or "merge these two concurrent branches".

**Efficient Storage**: Identical entries are automatically deduplicated, and metadata can be stored separately from bulk data.

## Data Structure Example

An Entry contains the following core data structure:

```rust
struct Entry {
    // Main tree node containing root ID, parent references, and metadata
    tree: TreeNode {
        root: ID,                    // Root entry ID of the tree
        parents: Vec<ID>,           // Parent entries in main tree history
        metadata: Option<RawData>,  // Optional metadata (not merged)
        data: RawData,             // Serialized CRDT data for main tree
    },

    // Named subtrees with independent histories
    subtrees: Vec<SubTreeNode> {
        name: String,              // Subtree name (e.g., "users", "posts")
        parents: Vec<ID>,          // Parent entries specific to this subtree
        data: RawData,            // Serialized CRDT data for this subtree
    },

    // Authentication and signature information
    sig: SigInfo {
        sig: Option<String>,       // Base64-encoded Ed25519 signature
        key_ref: String,          // Reference to signing key
    },
}

// Where:
type RawData = String;            // JSON-serialized CRDT structures
type ID = String;                // SHA-256 content hash (hex-encoded)
```

This structure enables each Entry to participate in multiple independent histories simultaneously - the main tree history plus any number of named subtree histories.
