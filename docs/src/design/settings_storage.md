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

Settings can be accessed through two primary interfaces:

#### SettingsStore API (Recommended)

`SettingsStore` provides a type-safe, high-level interface for settings management:

```rust,ignore
use eidetica::store::SettingsStore;

// Create a SettingsStore from a transaction
let settings_store = transaction.get_settings()?;

// Type-safe access to common settings
let database_name = settings_store.get_name()?;
let auth_settings = settings_store.get_auth_settings()?;
```

#### Transaction API

`Transaction::get_settings()` returns a SettingsStore that handles:

- **Historical state**: Computed from all relevant entries in the database
- **Staged changes**: Any modifications to `_settings` in the current transaction

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

## SettingsStore API

### Overview

`SettingsStore` provides a specialized, type-safe interface for managing the `_settings` subtree. It wraps `DocStore` to offer convenient methods for common settings operations while maintaining proper CRDT semantics and transaction boundaries.

### Key Benefits

- **Type Safety**: Eliminates raw CRDT manipulation for common operations
- **Convenience**: Direct methods for authentication key management
- **Atomicity**: Closure-based updates ensure atomic multi-step operations
- **Validation**: Built-in validation for authentication configurations
- **Abstraction**: Hides implementation details while providing escape hatch via `as_doc_store()`

### Primary Methods

```rust,ignore
impl SettingsStore {
    // Core settings management
    fn get_name(&self) -> Result<String>;
    fn set_name(&self, name: &str) -> Result<()>;

    // Authentication key management
    fn set_auth_key(&self, key_name: &str, key: AuthKey) -> Result<()>;
    fn get_auth_key(&self, key_name: &str) -> Result<AuthKey>;
    fn revoke_auth_key(&self, key_name: &str) -> Result<()>;

    // Complex operations via closure
    fn update_auth_settings<F>(&self, f: F) -> Result<()>
    where F: FnOnce(&mut AuthSettings) -> Result<()>;

    // Advanced access
    fn as_doc_store(&self) -> &DocStore;
    fn validate_entry_auth(&self, sig_key: &SigKey, backend: Option<&Arc<dyn BackendDB>>) -> Result<ResolvedAuth>;
}
```

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
// In a Transaction context
let settings_store = transaction.get_settings()?;

// Access database name
let name = settings_store.get_name()?;

// Access auth configuration
let auth_settings = settings_store.get_auth_settings()?;
```

### Modifying Settings

#### Using SettingsStore

```rust,ignore
use eidetica::store::SettingsStore;
use eidetica::auth::{AuthKey, Permission};

// Get a SettingsStore handle for type-safe operations
let settings_store = transaction.get_settings()?;

// Update database name
settings_store.set_name("My Database")?;

// Set authentication keys with validation (upsert behavior)
let auth_key = AuthKey::active(
    "ed25519:user_public_key",
    Permission::Write(10),
)?;
settings_store.set_auth_key("alice", auth_key)?;

// Perform complex auth operations atomically
settings_store.update_auth_settings(|auth| {
    auth.overwrite_key("bob", bob_key)?;
    auth.revoke_key("old_user")?;
    Ok(())
})?;

// Commit the transaction
transaction.commit()?;
```

#### Using DocStore Directly (Low-Level)

```rust,ignore
// Get a DocStore handle for the _settings store
let mut settings_store = transaction.get_store::<DocStore>("_settings")?;

// Update a setting
settings_store.set("name", "My Database")?;

// Commit the transaction
transaction.commit()?;
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
