# Entry

The fundamental building block of Eidetica's data model, representing an immutable, cryptographically-signed unit of data within the Merkle-DAG structure.

## Conceptual Role

Entries serve as the atomic units of both data storage and version history in Eidetica. They combine the functions of:

- **Data Container**: Holding actual application data and metadata
- **Version Node**: Linking to parent entries to form a history DAG
- **Authentication Unit**: Cryptographically signed to ensure integrity and authorization
- **Content-Addressable Object**: Uniquely identified by their content hash for deduplication and verification

## Internal Data Structure

Entry contains two fundamental internal data structures that form the Merkle-DAG:

**TreeNode**: The main tree node containing:

- Root ID of the tree this entry belongs to
- Parent entry references for the main tree history
- Optional metadata (not merged with other entries)

**SubTreeNodes**: Named subtree nodes, each containing:

- Subtree name (analogous to store/table names)
- Parent entry references specific to this subtree's history
- Optional serialized CRDT data payload for this subtree

**Authentication Envelope**: Every entry includes signature information that proves authorization and ensures tamper-detection.

## Relationship to User Abstractions

While entries internally use TreeNode and SubTreeNode structures, users interact with higher-level abstractions:

- **Database**: Provides operations over the tree of entries (uses TreeNode data)
- **Stores**: Typed access patterns (DocStore, Tables, etc.) over subtree data (uses SubTreeNode data)

This separation allows the internal Merkle-DAG structures to remain efficient and correct while providing user-friendly APIs.

## Identity and Integrity

**Content-Addressable Identity**: Each entry's ID is a SHA-256 hash of its canonical content, making entries globally unique and enabling efficient deduplication.

**Deterministic Hashing**: IDs are computed from a canonical JSON representation, ensuring identical entries produce identical IDs across different systems.

**Immutability Guarantee**: Once created, entries cannot be modified, ensuring the integrity of the historical record and cryptographic signatures.

## Design Benefits

**Distributed Synchronization**: Content-addressable IDs enable efficient sync protocols where systems can identify missing or conflicting entries.

**Cryptographic Verification**: Signed entries provide strong guarantees about data authenticity and integrity.

**Granular History**: The DAG structure enables sophisticated queries like "show me all changes since timestamp X" or "merge these two concurrent branches".

**Efficient Storage**: Identical entries are automatically deduplicated, and metadata can be stored separately from bulk data.

## ID Format Requirements

All IDs in Eidetica must be valid SHA-256 hashes represented as 64-character lowercase hexadecimal strings. This includes:

- **Tree root IDs**: The ID of the root entry of a tree
- **Main tree parent IDs**: Parent entries in the main tree
- **Subtree parent IDs**: Parent entries within specific subtrees
- **Entry IDs**: Content-addressable identifiers for entries themselves

### Valid ID Format

- **Length**: Exactly 64 characters
- **Characters**: Only lowercase hexadecimal (0-9, a-f)
- **Example**: `a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456`

### Invalid ID Examples

```text
❌ "parent_id"           # Too short, not hex
❌ "ABCD1234..."         # Uppercase letters
❌ "abcd-1234-..."       # Contains hyphens
❌ "12345678901234567890123456789012345678901234567890123456789012345"  # 63 chars (too short)
```

## Internal Data Structure Detail

An Entry contains the following internal data structures:

<!-- Code block ignored: Conceptual struct diagram showing internal data structures, not actual compilable code -->

```rust,ignore
struct Entry {
    // Main tree node - the core Merkle-DAG structure
    tree: TreeNode {
        root: ID,                    // Root entry ID of the tree
        parents: Vec<ID>,           // Parent entries in main tree history
        metadata: Option<RawData>,  // Optional metadata (not merged)
    },

    // Named subtree nodes - independent data partitions
    subtrees: Vec<SubTreeNode> {
        name: String,                 // Subtree name (e.g., "users", "posts")
        parents: Vec<ID>,             // Parent entries specific to this subtree
        data: Option<RawData>,        // Optional serialized CRDT data
    },

    // Authentication and signature information
    sig: SigInfo {
        sig: Option<String>,       // Base64-encoded Ed25519 signature
        key: SigKey,              // Reference to signing key
    },
}

// Where:
type RawData = String;            // JSON-serialized CRDT structures
type ID = String;                // SHA-256 content hash (hex-encoded)
```

### Key Design Points

- **TreeNode**: Represents the entry's position in the main Merkle-DAG tree structure
- **SubTreeNodes**: Enable independent histories for different data partitions within the same entry
- **Separation**: The tree structure (TreeNode) is separate from the data partitions (SubTreeNodes)
- **Multiple Histories**: Each entry can participate in one main tree history plus multiple independent subtree histories
- **Optional Data**: SubTreeNode.data is Option<RawData> containing the data for the underlying CRDT

### SubTreeNode Data Semantics

The `data` field in SubTreeNode is `Option<RawData>` to support different participation modes:

**None (No Data)**: Subtree appears in the Entry without providing data. This is used when:

- The `_index` is updated for a subtree (so that updates for the subtree's config are contained within it's DAG)
- Establishing parent relationships without modifying data
- The subtree participates in the Entry purely for structural reasons

**Some(data) (Actual Data)**: Contains the serialized CRDT data for this subtree
