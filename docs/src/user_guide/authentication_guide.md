# Authentication Guide

How to use Eidetica's authentication system for securing your data.

## Quick Start

Every Eidetica database requires authentication. Here's the minimal setup:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite};
# use eidetica::crdt::Doc;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Sqlite::in_memory().await?;
# let instance = Instance::open(Box::new(backend)).await?;
#
// Create and login a passwordless user (generates Ed25519 keypair automatically)
instance.create_user("alice", None).await?;
let mut user = instance.login_user("alice", None).await?;

// Create a database using the user's default key
let mut settings = Doc::new();
settings.set("name", "my_database");
let default_key = user.get_default_key()?;
let database = user.create_database(settings, &default_key).await?;

// All operations are now authenticated
let op = database.new_transaction().await?;
// ... make changes ...
op.commit().await?;  // Automatically signed
# Ok(())
# }
```

## Key Concepts

**Mandatory Authentication**: Every entry must be signed - no exceptions.

**Permission Levels**:

- **Admin**: Can modify settings and manage keys
- **Write**: Can read and write data
- **Read**: Can only read data

**Key Storage**: Private keys are stored in Instance, public keys in database settings.

## Common Tasks

### Adding Users

Give other users access to your database:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "auth_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let transaction = database.new_transaction().await?;
# // Generate a keypair for the new user
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
# let alice_public_key = format_public_key(&alice_verifying_key);
let settings_store = transaction.get_settings()?;

// Add a user with write access (indexed by pubkey, name is optional metadata)
let user_key = AuthKey::active(Some("alice_laptop"), Permission::Write(10));
settings_store.set_auth_key(&alice_public_key, user_key).await?;

transaction.commit().await?;
# Ok(())
# }
```

### Making Data Public (Read-Only)

Allow anyone to read your database:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::crdt::Doc;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Global permission for public read access
// The "*" key means any valid signature is accepted
let public_key = AuthKey::active(None::<String>, Permission::Read);
settings_store.set_auth_key("*", public_key).await?;

transaction.commit().await?;
# Ok(())
# }
```

### Collaborative Databases (Read-Write)

Create a collaborative database where anyone can read and write without individual key management:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::crdt::Doc;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "collaborative_notes");
# let default_key = user.get_default_key()?;
let database = user.create_database(settings, &default_key).await?;

// Set up global write permissions
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Global permission allows any device to read and write
let collaborative_key = AuthKey::active(None::<String>, Permission::Write(10));
settings_store.set_auth_key("*", collaborative_key).await?;

transaction.commit().await?;
# Ok(())
# }
```

**How it works**:

1. Any device can bootstrap without approval (global permission grants access)
2. Devices discover available SigKeys using `Database::find_sigkeys()`
3. Select a SigKey from the available options (will include `"*"` for global permissions)
4. Open the database with the selected SigKey
5. All transactions automatically use the configured permissions
6. No individual keys are added to the database's auth settings

**Example of opening a collaborative database**:

<!-- Code block ignored: Requires existing database with collaborative permissions setup -->

```rust,ignore
use eidetica::{Instance, Database, backend::database::Sqlite};
use eidetica::auth::crypto::{generate_keypair, format_public_key};
use eidetica::auth::types::SigKey;

let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
let (signing_key, verifying_key) = generate_keypair();
let database_root_id = /* ID from existing collaborative database */;

// Get your public key
let pubkey = format_public_key(&verifying_key);

// Discover all SigKeys this public key can use
let sigkeys = Database::find_sigkeys(&instance, &database_root_id, &pubkey).await?;

// Use the first available SigKey (will be "*" for global permissions)
if let Some((sigkey, _permission)) = sigkeys.first() {
    // Extract pubkey or name hint from the SigKey
    let sigkey_str = sigkey.hint().pubkey.clone()
        .or_else(|| sigkey.hint().name.clone())
        .expect("Expected pubkey or name hint");

    // Open the database with the discovered SigKey
    let database = Database::open(instance, &database_root_id, signing_key, sigkey_str).await?;

    // Create transactions as usual
    let txn = database.new_transaction().await?;
    // ... make changes ...
    txn.commit().await?;
}
```

This is ideal for:

- Team collaboration spaces
- Shared notes and documents
- Public wikis
- Development/testing environments

**Security note**: Use appropriate permission levels. `Write(10)` allows Write and Read operations but not Admin operations (managing keys and settings).

### Revoking Access

Remove a user's access:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
# use eidetica::crdt::Doc;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Generate a keypair for alice
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
# let alice_pubkey = format_public_key(&alice_verifying_key);
// First add alice key so we can revoke it
let transaction_setup = database.new_transaction().await?;
let settings_setup = transaction_setup.get_settings()?;
settings_setup.set_auth_key(&alice_pubkey, AuthKey::active(Some("alice"), Permission::Write(10))).await?;
transaction_setup.commit().await?;
let transaction = database.new_transaction().await?;

let settings_store = transaction.get_settings()?;

// Revoke the key by its pubkey identifier
settings_store.revoke_auth_key(&alice_pubkey).await?;

transaction.commit().await?;
# Ok(())
# }
```

Note: Historical entries created by revoked keys remain valid.

## Multi-User Setup Example

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "multi_user_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let transaction = database.new_transaction().await?;
# let settings_store = transaction.get_settings()?;
#
// Generate keypairs for different users
let (_super_admin_signing_key, super_admin_verifying_key) = generate_keypair();
let super_admin_public_key = format_public_key(&super_admin_verifying_key);

let (_dept_admin_signing_key, dept_admin_verifying_key) = generate_keypair();
let dept_admin_public_key = format_public_key(&dept_admin_verifying_key);

let (_user1_signing_key, user1_verifying_key) = generate_keypair();
let user1_public_key = format_public_key(&user1_verifying_key);

// Use update_auth_settings for complex multi-key setup
// Keys are indexed by pubkey, with optional name metadata
settings_store.update_auth_settings(|auth| {
    // Super admin (priority 0 - highest)
    auth.overwrite_key(&super_admin_public_key, AuthKey::active(
        Some("super_admin"),
        Permission::Admin(0),
    ))?;

    // Department admin (priority 10)
    auth.overwrite_key(&dept_admin_public_key, AuthKey::active(
        Some("dept_admin"),
        Permission::Admin(10),
    ))?;

    // Regular users (priority 100)
    auth.overwrite_key(&user1_public_key, AuthKey::active(
        Some("user1"),
        Permission::Write(100),
    ))?;

    Ok(())
}).await?;

transaction.commit().await?;
# Ok(())
# }
```

## Advanced: Cross-Database Authentication (Delegation)

Delegation allows databases to reference other databases as sources of authentication keys. This enables powerful patterns like:

- Users manage their own keys in personal databases
- Multiple projects share authentication across databases
- Hierarchical access control without granting admin privileges

### How Delegation Works

When you delegate to another database:

1. **The delegating database** references another database in its `_settings.auth`
2. **The delegated database** maintains its own keys in its `_settings.auth`
3. **Permission clamping** ensures delegated keys can't exceed specified bounds
4. **Delegation paths** reference databases by their **root entry ID** and resolve the final key in the delegated database

### Basic Delegation Setup

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
# use eidetica::auth::{DelegatedTreeRef, Permission, PermissionBounds, TreeReference};
# use eidetica::store::SettingsStore;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let default_key = user.get_default_key()?;
#
# // Create user's personal database
# let alice_database = user.create_database(Doc::new(), &default_key).await?;
#
# // Create main project database
# let project_database = user.create_database(Doc::new(), &default_key).await?;
// Get the user's database root and current tips
let user_root = alice_database.root_id().clone();
let user_tips = alice_database.get_tips().await?;

// Add delegation reference to project database
let transaction = project_database.new_transaction().await?;
let settings = transaction.get_settings()?;

settings.update_auth_settings(|auth| {
    // Delegation is stored by the root tree ID automatically
    auth.add_delegated_tree(DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            max: Permission::Write(15),
            min: Some(Permission::Read),
        },
        tree: TreeReference {
            root: user_root,
            tips: user_tips,
        },
    })?;
    Ok(())
}).await?;

transaction.commit().await?;
# Ok(())
# }
```

Now any key in Alice's personal database can access the project database, with permissions clamped to the specified bounds.

### Understanding Delegation Paths

**Critical concept**: A delegation path traverses through databases using **two different types of identifiers**:

1. **Root tree IDs** - Identify delegated databases (DelegatedTreeRef stored by root ID)
2. **Key hints** - Identify signing keys (by pubkey or name) in the final database

#### Delegated Tree References

Delegations are stored in the **delegating database's** auth settings by the delegated tree's **root entry ID**:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
# use eidetica::auth::{DelegatedTreeRef, Permission, PermissionBounds, TreeReference};
# use eidetica::store::SettingsStore;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let default_key = user.get_default_key()?;
# let alice_db = user.create_database(Doc::new(), &default_key).await?;
# let alice_root = alice_db.root_id().clone();
# let alice_tips = alice_db.get_tips().await?;
# let project_db = user.create_database(Doc::new(), &default_key).await?;
# let transaction = project_db.new_transaction().await?;
# let settings = transaction.get_settings()?;
// In project database: delegation stored by Alice's database root ID
settings.update_auth_settings(|auth| {
    auth.add_delegated_tree(DelegatedTreeRef {
        tree: TreeReference {
            root: alice_root,  // ← Root ID used as storage key
            tips: alice_tips,
        },
        permission_bounds: PermissionBounds {
            max: Permission::Write(15),
            min: Some(Permission::Read),
        },
    })?;
    Ok(())
}).await?;
# transaction.commit().await?;
# Ok(())
# }
```

This creates an entry in the project database's auth settings:

- **Key**: The root entry ID of Alice's database (e.g., `sha256:abc123...`)
- **Value**: DelegatedTreeRef with permission bounds and tree reference

#### Signing Key Names

These are names in the **delegated database's** auth settings that point to **public keys**:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::store::SettingsStore;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let default_key_id = user.get_default_key()?;
# let alice_db = user.create_database(Doc::new(), &default_key_id).await?;
# let alice_pubkey_str = user.get_public_key(&default_key_id)?;
# let transaction = alice_db.new_transaction().await?;
# let settings = transaction.get_settings()?;
// In Alice's database: keys are indexed by pubkey
// (The default key was added automatically during bootstrap)
// We can update it with a descriptive name
settings.update_auth_settings(|auth| {
    auth.overwrite_key(
        &alice_pubkey_str,  // ← Index by pubkey
        AuthKey::active(
            Some("alice_work"),  // Optional name metadata
            Permission::Write(10),
        )
    )?;
    Ok(())
}).await?;
# transaction.commit().await?;
# Ok(())
# }
```

This updates the entry in Alice's database auth settings:

- **Key**: Indexed by the pubkey string (e.g., `"ed25519:ABC..."`)
- **Name**: `"alice_work"` (optional human-readable metadata)
- **Permissions**: The access level for this key

### Using Delegated Keys

A delegation path is a sequence of steps that traverses from the delegating database to the signing key:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc};
# use eidetica::auth::{SigKey, DelegationStep};
# use eidetica::store::DocStore;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(Sqlite::in_memory().await?)).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let default_key = user.get_default_key()?;
# let project_db = user.create_database(Doc::new(), &default_key).await?;
# let user_db = user.create_database(Doc::new(), &default_key).await?;
# let user_tips = user_db.get_tips().await?;
// Create a delegation path:
// - path: list of delegated trees to traverse (by root ID)
// - hint: identifies the final signer in the last delegated tree
let delegation_path = SigKey::Delegation {
    path: vec![
        // Step: Traverse to Alice's database (using its root ID)
        DelegationStep {
            tree: user_db.root_id().to_string(),  // Root ID of delegated database
            tips: user_tips,  // Tips for Alice's database
        },
    ],
    // Final signer hint - resolved in Alice's database auth settings
    hint: eidetica::auth::KeyHint::from_name("alice_laptop"),
};

// Use the delegation path to create an authenticated operation
// Note: This requires the actual signing key to be available
// project_database.new_operation_with_sig_key(delegation_path)?;
# Ok(())
# }
```

**Path traversal**:

1. Start in **project database** auth settings
2. Look up root ID in path → finds DelegatedTreeRef (stored by that root ID) → jumps to **Alice's database**
3. Use `hint` to find the signer: look up `"alice_laptop"` (by name) → finds AuthKey → gets **Ed25519 public key**
4. Use that public key to verify the entry signature

### Permission Clamping

Permissions from delegated databases are automatically clamped:

```text
User DB key: Admin(5)     →  Project DB clamps to: Write(15)  (max bound)
User DB key: Write(10)    →  Project DB keeps:      Write(10) (within bounds)
User DB key: Read         →  Project DB keeps:      Read      (above min bound)
```

**Rules**:

- If delegated permission > max bound: lowered to max
- If delegated permission < min bound: raised to min (if specified)
- Permissions within bounds are preserved
- Admin permissions only apply within the delegated database

This makes it convenient to reuse the same validation rules across both databases. Only an Admin can grant permissions to a database by modifying the Auth Settings, but we can grant lower access to a User, and allow them to use any key they want, by granting access to a User controlled database and giving **that** the desired permissions. The User can then manage their own keys using their own Admin keys, under exactly the same rules.

### Multi-Level Delegation

Delegated databases can themselves delegate to other databases, creating chains:

```rust,ignore
// Entry signed through a delegation chain:
{
  "auth": {
    "sig": "...",
    "key": [
      {
        "key": "team@example.com",      // Step 1: Delegation ref in Main DB → Team DB
        "tips": ["team_db_tip"]
      },
      {
        "key": "alice@example.com",     // Step 2: Delegation ref in Team DB → Alice's DB
        "tips": ["alice_db_tip"]
      },
      {
        "key": "alice_laptop",          // Step 3: Signing key in Alice's DB → pubkey
        // No tips - this is a pubkey, not a tree
      }
    ]
  }
}
```

**Path traversal**:

1. Look up `"team@example.com"` in **Main DB** → finds DelegatedTreeRef → jump to **Team DB**
2. Look up `"alice@example.com"` in **Team DB** → finds DelegatedTreeRef → jump to **Alice's DB**
3. Look up `"alice_laptop"` in **Alice's DB** → finds AuthKey → get **Ed25519 public key**
4. Use that public key to verify the signature

Each level applies its own permission clamping, with the final effective permission being the **minimum** across all levels.

### Common Delegation Patterns

**User-Managed Access**:

```text
Project DB → delegates to → Alice's Personal DB
                              ↓
                         Alice manages her own keys
```

**Team Hierarchy**:

```text
Main DB → delegates to → Team DB → delegates to → User DB
          (max: Admin)            (max: Write)
```

**Cross-Project Authentication**:

```text
Project A ───┐
             ├→ delegates to → Shared Auth DB
Project B ───┘
```

### Key Aliasing

Auth settings can contain multiple names for the same public key with different permissions:

```json
{
  "_settings": {
    "auth": {
      "Ed25519:abc123...": {
        "pubkey": "Ed25519:abc123...",
        "permissions": "admin:0",
        "status": "active"
      },
      "alice_work": {
        "pubkey": "Ed25519:abc123...",
        "permissions": "write:10",
        "status": "active"
      },
      "alice_readonly": {
        "pubkey": "Ed25519:abc123...",
        "permissions": "read",
        "status": "active"
      }
    }
  }
}
```

This allows:

- The same key to have different permission contexts
- Readable key names for human-friendly lookups
- Fine-grained access control based on how the key is referenced

### Best Practices

1. **Use descriptive key names**: `"alice_laptop"`, `"deploy_bot"` for keys that will be looked up by name
2. **Set appropriate permission bounds**: Don't grant more access than needed
3. **Update delegation tips**: Keep tips current to ensure revocations are respected
4. **Track delegated database root IDs**: Delegation paths use root IDs, so document which IDs correspond to which databases
5. **Document delegation chains**: Complex hierarchies can be hard to debug

### See Also

- [Delegation Design](../design/authentication.md#delegation-delegated-databases) - Technical details
- [Permission System](../design/authentication.md#permission-hierarchy) - How permissions work

## Troubleshooting

**"Authentication failed"**: Check that:

- The key exists in database settings
- The key status is Active (not Revoked)
- The key has sufficient permissions for the operation

**"Key name conflict"**: When using `set_auth_key()` with different public key:

- `set_auth_key()` provides upsert behavior for same public key
- Returns KeyNameConflict error if key name exists with different public key
- Use `get_auth_key()` to check existing key before deciding action

**"Cannot modify key"**: Admin operations require:

- Admin-level permissions
- Equal or higher priority than the target key

**Multi-device conflicts**: During bootstrap sync between devices:

- If same key name with same public key: Operation succeeds (safe)
- If same key name with different public key: Operation fails (prevents conflicts)
- Consider using device-specific key names like "alice_laptop", "alice_phone"

**Network partitions**: Authentication changes merge automatically using Last-Write-Wins. The most recent change takes precedence.

## Bootstrap Security Policy

When new devices join existing databases through bootstrap synchronization, Eidetica provides two approval methods to balance security and convenience.

### Bootstrap Approval Methods

Eidetica supports two bootstrap approval approaches, checked in this order:

1. **Global Wildcard Permissions** - Databases with global '\*' permissions automatically approve bootstrap requests if the requested permission is satisfied
2. **Manual Approval** - Default secure behavior requiring admin approval for each device

### Default Security Behavior

By default, bootstrap requests are **rejected** for security:

<!-- Code block ignored: Requires network connectivity to peer server -->

```rust,ignore
// Bootstrap will fail without explicit policy configuration
user.request_database_access(
    &sync,
    "127.0.0.1:8080",
    &database_id,
    &key_id,  // User's key ID
    Permission::Write(100),
).await; // Returns PermissionDenied error
```

### Global Wildcard Permissions (Recommended for Collaboration)

The simplest approach for collaborative databases is to use global wildcard permissions:

```rust,ignore
let mut settings = Doc::new();
let mut auth_doc = Doc::new();

// Add admin key
auth_doc.set_json("admin", serde_json::json!({
    "pubkey": admin_public_key,
    "permissions": {"Admin": 1},
    "status": "Active"
}))?;

// Add global wildcard permission for automatic bootstrap
auth_doc.set_json("*", serde_json::json!({
    "pubkey": "*",
    "permissions": {"Write": 10},  // Allows Read and Write(11+) requests
    "status": "Active"
}))?;

settings.set("auth", auth_doc);
```

**Benefits**:

- No per-device key management required
- Immediate bootstrap approval
- Simple configuration - one permission setting controls all devices
- See [Bootstrap Guide](bootstrap.md#global-permission-bootstrap) for details

### Manual Approval Process

For controlled access scenarios, use manual approval to review each bootstrap request:

**Security Recommendations**:

- **Development/Testing**: Use global wildcard permissions for convenience
- **Production**: Use manual approval for controlled access
- **Team Collaboration**: Use global wildcard permissions with appropriate permission levels
- **Public Databases**: Use global wildcard permissions for open access, or manual approval for controlled access

### Bootstrap Flow

1. **Client Request**: Device requests access with public key and permission level
2. **Global Permission Check**: Server checks if global '\*' permission satisfies request
3. **Global Permission Approval**: If global permission exists and satisfies request, access is granted immediately
4. **Manual Approval Queue**: If no global permission, request is queued for admin review
5. **Admin Decision**: Admin explicitly approves or rejects the request
6. **Database Access**: Approved devices can read/write according to granted permissions

## See Also

- [Core Concepts](core_concepts.md) - Understanding Databases and Entries
- [Getting Started](getting_started.md) - Basic database setup
- [Authentication Reference](../internal/authentication.md) - Technical reference
