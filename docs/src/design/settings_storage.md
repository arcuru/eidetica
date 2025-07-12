# Settings Storage Design

## Overview

This document describes how Eidetica stores, retrieves, and tracks settings in trees. Settings are stored exclusively in the `_settings` subtree and tracked via entry metadata for efficient access.

## Architecture

### Settings Storage

Settings are stored in the `_settings` subtree (constant `SETTINGS` in `constants.rs`):

```rust
// Settings structure in _settings subtree
{
    "auth": {
        "key_id": {
            "key": "...",           // Public key
            "permissions": "...",   // Permission level
            "status": "..."         // Active/Revoked
        }
    }
    // Future: tree_config, replication, etc.
}
```

**Key Properties:**

- **Data Type**: `Map` CRDT for deterministic merging
- **Location**: Exclusively in `_settings` subtree
- **Access**: Through `AtomicOp::get_settings()` method

### Settings Retrieval

`AtomicOp::get_settings()` provides unified access to settings:

```rust
pub fn get_settings(&self) -> Result<Map> {
    // Get historical settings from the tree
    let mut historical_settings = self.get_full_state::<Map>(SETTINGS)?;

    // Get any staged changes to the _settings subtree in this operation
    let staged_settings = self.get_local_data::<Map>(SETTINGS)?;

    // Merge using CRDT semantics
    historical_settings = historical_settings.merge(&staged_settings)?;

    Ok(historical_settings)
}
```

The method combines:

- **Historical state**: Computed from all relevant entries in the tree
- **Staged changes**: Any modifications to `_settings` in the current operation

### Entry Metadata

Every entry includes metadata tracking settings state:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntryMetadata {
    /// Tips of the _settings subtree at the time this entry was created
    settings_tips: Vec<ID>,
    /// Random entropy for ensuring unique IDs for root entries
    entropy: Option<u64>,
}
```

**Metadata Properties:**

- Automatically populated by `AtomicOp::commit()`
- Used for efficient settings validation in sparse checkouts
- Stored in `TreeNode.metadata` field as serialized JSON

## Data Structures

### Entry Structure

```rust
pub struct Entry {
    tree: TreeNode,              // Main tree node with metadata
    subtrees: Vec<SubTreeNode>,  // Named subtrees including _settings
    sig: SigInfo,                // Signature information
}
```

### TreeNode Structure

```rust
struct TreeNode {
    pub root: ID,                   // Root entry ID of the tree
    pub parents: Vec<ID>,           // Parent entry IDs in main tree history
    pub metadata: Option<RawData>,  // Structured metadata (settings tips, entropy)
}
```

**Note**: `TreeNode` no longer contains a `data` field - all data is stored in named subtrees.

### SubTreeNode Structure

```rust
struct SubTreeNode {
    pub name: String,        // Subtree name (e.g., "_settings")
    pub parents: Vec<ID>,    // Parent entries in subtree history
    pub data: RawData,       // Serialized subtree data
}
```

## Authentication Settings

Authentication configuration is stored in `_settings.auth`:

### AuthSettings Structure

```rust
pub struct AuthSettings {
    inner: Map,  // Wraps Map data from _settings.auth
}
```

**Key Operations:**

- `add_key()`: Add/update authentication keys
- `revoke_key()`: Mark keys as revoked
- `get_key()`: Retrieve specific keys
- `get_all_keys()`: Get all authentication keys

### Authentication Flow

1. **Settings Access**: `AtomicOp::get_settings()` retrieves current auth configuration
2. **Key Resolution**: `AuthValidator` resolves key IDs to full key information
3. **Permission Check**: Validates operation against key permissions
4. **Signature Verification**: Verifies entry signatures match configured keys

## Usage Patterns

### Reading Settings

```rust
// In an AtomicOp context
let settings = op.get_settings()?;

// Access auth configuration
if let Some(Value::Map(auth_map)) = settings.get("auth") {
    // Process authentication settings
}
```

### Modifying Settings

```rust
// Get a Dict handle for the _settings subtree
let mut settings_store = op.get_subtree::<Dict>("_settings")?;

// Update a setting
settings_store.set("tree_config.name", "My Tree")?;

// Commit the operation
let entry_id = op.commit()?;
```

### Bootstrap Process

When creating a tree with authentication:

1. First entry includes auth configuration in `_settings.auth`
2. `AtomicOp::commit()` detects bootstrap scenario
3. Allows self-signed entry to establish initial auth configuration

## Design Benefits

1. **Single Source of Truth**: All settings in `_settings` subtree
2. **CRDT Semantics**: Deterministic merge resolution for concurrent updates
3. **Efficient Access**: Metadata tips enable quick settings retrieval
4. **Clean Architecture**: Entry is pure data, AtomicOp handles business logic
5. **Extensibility**: Easy to add new setting categories alongside `auth`
