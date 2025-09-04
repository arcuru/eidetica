# Settings Storage Design

## Overview

This document describes how Eidetica stores, retrieves, and tracks settings in databases. Settings are stored exclusively in the `_settings` store and tracked via entry metadata for efficient access.

## Architecture

### Settings Storage

Settings are stored in the `_settings` store (constant `SETTINGS` in `constants.rs`):

```rust,ignore
// Settings structure in _settings store
{
    "auth": {
        "key_name": {
            "key": "...",           // Public key
            "permissions": "...",   // Permission level
            "status": "..."         // Active/Revoked
        }
    }
    // Future: tree_config, replication, etc.
}
```

**Key Properties:**

- **Data Type**: `Doc` CRDT for deterministic merging
- **Location**: Exclusively in `_settings` store
- **Access**: Through `Transaction::get_settings()` method

### Settings Retrieval

`Transaction::get_settings()` provides unified access to settings:

```rust,ignore
pub fn get_settings(&self) -> Result<Doc> {
    // Get historical settings from the database
    let mut historical_settings = self.get_full_state::<Doc>(SETTINGS)?;

    // Get any staged changes to the _settings store in this operation
    let staged_settings = self.get_local_data::<Doc>(SETTINGS)?;

    // Merge using CRDT semantics
    historical_settings = historical_settings.merge(&staged_settings)?;

    Ok(historical_settings)
}
```

The method combines:

- **Historical state**: Computed from all relevant entries in the database
- **Staged changes**: Any modifications to `_settings` in the current operation

### Entry Metadata

Every entry includes metadata tracking settings state:

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntryMetadata {
    /// Tips of the _settings store at the time this entry was created
    settings_tips: Vec<ID>,
    /// Random entropy for ensuring unique IDs for root entries
    entropy: Option<u64>,
}
```

**Metadata Properties:**

- Automatically populated by `Transaction::commit()`
- Used for efficient settings validation in sparse checkouts
- Stored in `TreeNode.metadata` field as serialized JSON

## Data Structures

### Entry Structure

```rust,ignore
pub struct Entry {
    database: TreeNode,              // Main database node with metadata
    stores: Vec<SubTreeNode>,  // Named stores including _settings
    sig: SigInfo,                // Signature information
}
```

### TreeNode Structure

```rust,ignore
struct TreeNode {
    pub root: ID,                   // Root entry ID of the database
    pub parents: Vec<ID>,           // Parent entry IDs in main database history
    pub metadata: Option<RawData>,  // Structured metadata (settings tips, entropy)
}
```

**Note**: `TreeNode` no longer contains a data field - all data is stored in named stores.

### SubTreeNode Structure

```rust,ignore
struct SubTreeNode {
    pub name: String,        // Store name (e.g., "_settings")
    pub parents: Vec<ID>,    // Parent entries in store history
    pub data: RawData,       // Serialized store data
}
```

## Authentication Settings

Authentication configuration is stored in `_settings.auth`:

### AuthSettings Structure

```rust,ignore
pub struct AuthSettings {
    inner: Doc,  // Wraps Doc data from _settings.auth
}
```

**Key Operations:**

- `add_key()`: Add/update authentication keys
- `revoke_key()`: Mark keys as revoked
- `get_key()`: Retrieve specific keys
- `get_all_keys()`: Get all authentication keys

### Authentication Flow

1. **Settings Access**: `Transaction::get_settings()` retrieves current auth configuration
2. **Key Resolution**: `AuthValidator` resolves key names to full key information
3. **Permission Check**: Validates operation against key permissions
4. **Signature Verification**: Verifies entry signatures match configured keys

## Usage Patterns

### Reading Settings

```rust,ignore
// In an Transaction context
let settings = op.get_settings()?;

// Access auth configuration
if let Some(Value::Map(auth_map)) = settings.get("auth") {
    // Process authentication settings
}
```

### Modifying Settings

```rust,ignore
// Get a DocStore handle for the _settings store
let mut settings_store = op.get_subtree::<DocStore>("_settings")?;

// Update a setting
settings_store.set("tree_config.name", "My Database")?;

// Commit the operation
let entry_id = op.commit()?;
```

### Bootstrap Process

When creating a database with authentication:

1. First entry includes auth configuration in `_settings.auth`
2. `Transaction::commit()` detects bootstrap scenario
3. Allows self-signed entry to establish initial auth configuration

## Design Benefits

1. **Single Source of Truth**: All settings in `_settings` store
2. **CRDT Semantics**: Deterministic merge resolution for concurrent updates
3. **Efficient Access**: Metadata tips enable quick settings retrieval
4. **Clean Architecture**: Entry is pure data, Transaction handles business logic
5. **Extensibility**: Easy to add new setting categories alongside `auth`
