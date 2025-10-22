# Authentication Guide

How to use Eidetica's authentication system for securing your data.

## Quick Start

Every Eidetica database requires authentication. Here's the minimal setup:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
# use eidetica::crdt::Doc;
#
# fn main() -> eidetica::Result<()> {
# let database = InMemory::new();
# let db = Instance::open(Box::new(database))?;
#
// Add an authentication key (generates Ed25519 keypair)
db.add_private_key("my_key")?;

// Create a database using that key
let mut settings = Doc::new();
settings.set("name", "my_database");
let database = db.new_database(settings, "my_key")?;

// All operations are now authenticated
let op = database.new_transaction()?;
// ... make changes ...
op.commit()?;  // Automatically signed
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
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let db = Instance::open(Box::new(InMemory::new()))?;
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set_string("name", "auth_example");
# let database = db.new_database(settings, "admin")?;
# let transaction = database.new_transaction()?;
# // Generate a keypair for the new user
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
# let alice_public_key = format_public_key(&alice_verifying_key);
let settings_store = transaction.get_settings()?;

// Add a user with write access
let user_key = AuthKey::active(
    &alice_public_key,
    Permission::Write(10),
)?;
settings_store.set_auth_key("alice", user_key)?;

transaction.commit()?;
# Ok(())
# }
```

### Making Data Public (Read-Only)

Allow anyone to read your database:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::crdt::Doc;
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::open(Box::new(InMemory::new()))?;
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "admin")?;
# let transaction = database.new_transaction()?;
let settings_store = transaction.get_settings()?;

// Wildcard key for public read access
let public_key = AuthKey::active(
    "*",
    Permission::Read,
)?;
settings_store.set_auth_key("*", public_key)?;

transaction.commit()?;
# Ok(())
# }
```

### Collaborative Databases (Read-Write)

Create a collaborative database where anyone can read and write without individual key management:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::crdt::Doc;
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::open(Box::new(InMemory::new()))?;
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set("name", "collaborative_notes");
let database = db.new_database(settings, "admin")?;

// Set up global write permissions
let transaction = database.new_transaction()?;
let settings_store = transaction.get_settings()?;

// Global permission allows any device to read and write
let collaborative_key = AuthKey::active(
    "*",
    Permission::Write(10),
)?;
settings_store.set_auth_key("*", collaborative_key)?;

transaction.commit()?;
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

```rust
# extern crate eidetica;
# use eidetica::{Instance, Database, backend::database::InMemory};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
# use eidetica::auth::types::SigKey;
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# let (signing_key, verifying_key) = generate_keypair();
# let database_root_id = "collaborative_db_root".into();
// Get your public key
let pubkey = format_public_key(&verifying_key);

// Discover all SigKeys this public key can use
let sigkeys = Database::find_sigkeys(&instance, &database_root_id, &pubkey)?;

// Use the first available SigKey (will be "*" for global permissions)
if let Some((sigkey, _permission)) = sigkeys.first() {
    let sigkey_str = match sigkey {
        SigKey::Direct(name) => name.clone(),
        _ => panic!("Delegation paths not yet supported"),
    };

    // Open the database with the discovered SigKey
    let database = Database::open(instance, &database_root_id, signing_key, sigkey_str)?;

    // Create transactions as usual
    let txn = database.new_transaction()?;
    // ... make changes ...
    txn.commit()?;
}
# Ok(())
# }
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
# use eidetica::{Instance, backend::database::InMemory};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::crdt::Doc;
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::open(Box::new(InMemory::new()))?;
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "admin")?;
// First add alice key so we can revoke it
let transaction_setup = database.new_transaction()?;
let settings_setup = transaction_setup.get_settings()?;
settings_setup.set_auth_key("alice", AuthKey::active("*", Permission::Write(10))?)?;
transaction_setup.commit()?;
let transaction = database.new_transaction()?;

let settings_store = transaction.get_settings()?;

// Revoke the key
settings_store.revoke_auth_key("alice")?;

transaction.commit()?;
# Ok(())
# }
```

Note: Historical entries created by revoked keys remain valid.

## Multi-User Setup Example

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::{generate_keypair, format_public_key};
#
# fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let db = Instance::open(Box::new(InMemory::new()))?;
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set_string("name", "multi_user_example");
# let database = db.new_database(settings, "admin")?;
# let transaction = database.new_transaction()?;
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
settings_store.update_auth_settings(|auth| {
    // Super admin (priority 0 - highest)
    auth.overwrite_key("super_admin", AuthKey::active(
        &super_admin_public_key,
        Permission::Admin(0),
    )?)?;

    // Department admin (priority 10)
    auth.overwrite_key("dept_admin", AuthKey::active(
        &dept_admin_public_key,
        Permission::Admin(10),
    )?)?;

    // Regular users (priority 100)
    auth.overwrite_key("user1", AuthKey::active(
        &user1_public_key,
        Permission::Write(100),
    )?)?;

    Ok(())
})?;

transaction.commit()?;
# Ok(())
# }
```

## Key Management Tips

1. **Use descriptive key names**: "alice_laptop", "build_server", etc.
2. **Set up admin hierarchy**: Lower priority numbers = higher authority
3. **Use SettingsStore methods**:
   - `set_auth_key()` for setting keys (upsert behavior)
   - `revoke_auth_key()` for removing access
   - `update_auth_settings()` for complex multi-step operations
4. **Regular key rotation**: Periodically update keys for security
5. **Backup admin keys**: Keep secure copies of critical admin keys

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
4. **Delegation paths** reference keys by their **name** in the delegated database's auth settings

### Basic Delegation Setup

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use eidetica::auth::{DelegatedTreeRef, Permission, PermissionBounds, TreeReference};
# use eidetica::store::SettingsStore;
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.add_private_key("admin")?;
#
# // Create user's personal database
# let alice_database = instance.new_database(Doc::new(), "admin")?;
#
# // Create main project database
# let project_database = instance.new_database(Doc::new(), "admin")?;
// Get the user's database root and current tips
let user_root = alice_database.root_id().clone();
let user_tips = alice_database.get_tips()?;

// Add delegation reference to project database
let transaction = project_database.new_transaction()?;
let settings = transaction.get_settings()?;

settings.update_auth_settings(|auth| {
    auth.add_delegated_tree("alice@example.com", DelegatedTreeRef {
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
})?;

transaction.commit()?;
# Ok(())
# }
```

Now any key in Alice's personal database can access the project database, with permissions clamped to the specified bounds.

### Understanding Delegation Paths

**Critical concept**: A delegation path traverses through databases using **two different types of key names**:

1. **Delegation reference names** - Point to other databases (DelegatedTreeRef)
2. **Signing key names** - Point to public keys (AuthKey) for signature verification

#### Delegation Reference Names

These are names in the **delegating database's** auth settings that point to **other databases**:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use eidetica::auth::{DelegatedTreeRef, Permission, PermissionBounds, TreeReference};
# use eidetica::store::SettingsStore;
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.add_private_key("admin")?;
# let alice_db = instance.new_database(Doc::new(), "admin")?;
# let alice_root = alice_db.root_id().clone();
# let alice_tips = alice_db.get_tips()?;
# let project_db = instance.new_database(Doc::new(), "admin")?;
# let transaction = project_db.new_transaction()?;
# let settings = transaction.get_settings()?;
// In project database: "alice@example.com" points to Alice's database
settings.update_auth_settings(|auth| {
    auth.add_delegated_tree(
        "alice@example.com",  // ← Delegation reference name
        DelegatedTreeRef {
            tree: TreeReference {
                root: alice_root,
                tips: alice_tips,
            },
            permission_bounds: PermissionBounds {
                max: Permission::Write(15),
                min: Some(Permission::Read),
            },
        }
    )?;
    Ok(())
})?;
# transaction.commit()?;
# Ok(())
# }
```

This creates an entry in the project database's auth settings:

- **Name**: `"alice@example.com"`
- **Points to**: Alice's database (via TreeReference)

#### Signing Key Names

These are names in the **delegated database's** auth settings that point to **public keys**:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::store::SettingsStore;
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# let pubkey = instance.add_private_key("alice_laptop")?;
# let alice_db = instance.new_database(Doc::new(), "alice_laptop")?;
# let alice_pubkey_str = eidetica::auth::crypto::format_public_key(&pubkey);
# let transaction = alice_db.new_transaction()?;
# let settings = transaction.get_settings()?;
// In Alice's database: "alice_laptop" points to a public key
// (This was added automatically during bootstrap, but we can add aliases)
settings.update_auth_settings(|auth| {
    auth.add_key(
        "alice_work",  // ← Signing key name (alias)
        AuthKey::active(
            &alice_pubkey_str,  // The actual Ed25519 public key
            Permission::Write(10),
        )?
    )?;
    Ok(())
})?;
# transaction.commit()?;
# Ok(())
# }
```

This creates an entry in Alice's database auth settings:

- **Name**: `"alice_work"` (an alias for the same key as `"alice_laptop"`)
- **Points to**: An Ed25519 public key

### Using Delegated Keys

A delegation path is a sequence of steps that traverses from the delegating database to the signing key:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc};
# use eidetica::auth::{SigKey, DelegationStep};
# use eidetica::store::DocStore;
#
# fn main() -> eidetica::Result<()> {
# let instance = Instance::open(Box::new(InMemory::new()))?;
# instance.add_private_key("admin")?;
# let project_db = instance.new_database(Doc::new(), "admin")?;
# let user_db = instance.new_database(Doc::new(), "admin")?;
# let user_tips = user_db.get_tips()?;
// Create a delegation path with TWO steps:
let delegation_path = SigKey::DelegationPath(vec![
    // Step 1: Look up "alice@example.com" in PROJECT database's auth settings
    //         This is a delegation reference name pointing to Alice's database
    DelegationStep {
        key: "alice@example.com".to_string(),
        tips: Some(user_tips),  // Tips for Alice's database
    },
    // Step 2: Look up "alice_laptop" in ALICE'S database's auth settings
    //         This is a signing key name pointing to an Ed25519 public key
    DelegationStep {
        key: "alice_laptop".to_string(),
        tips: None,  // Final step has no tips (it's a pubkey, not a tree)
    },
]);

// Use the delegation path to create an authenticated operation
// Note: This requires the actual signing key to be available
// project_database.new_operation_with_sig_key(delegation_path)?;
# Ok(())
# }
```

**Path traversal**:

1. Start in **project database** auth settings
2. Look up `"alice@example.com"` → finds DelegatedTreeRef → jumps to **Alice's database**
3. Look up `"alice_laptop"` in Alice's database → finds AuthKey → gets **Ed25519 public key**
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
- Readable delegation path names instead of public key strings
- Fine-grained access control based on how the key is referenced

### Best Practices

1. **Use descriptive delegation names**: `"alice@example.com"`, `"team-engineering"`
2. **Set appropriate permission bounds**: Don't grant more access than needed
3. **Update delegation tips**: Keep tips current to ensure revocations are respected
4. **Use friendly key names**: Add aliases for keys that will be used in delegation paths
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

When new devices join existing databases through bootstrap synchronization, Eidetica provides multiple approval methods to balance security and convenience.

### Bootstrap Approval Methods

Eidetica supports three bootstrap approval approaches, checked in this order:

1. **Global Permissions** - Databases with global '\*' permissions automatically approve bootstrap requests if the requested permission is satisfied
2. **Auto-Approval Policy** - When `bootstrap_auto_approve: true`, devices are automatically approved and keys added
3. **Manual Approval** - Default secure behavior requiring admin approval for each device

### Default Security Behavior

By default, bootstrap requests are **rejected** for security:

<!-- Code block ignored: Requires network connectivity to peer server -->

```rust,ignore
// Bootstrap will fail without explicit policy configuration
client_sync.sync_with_peer_for_bootstrap(
    "127.0.0.1:8080",
    &database_tree_id,
    Some("device_key_name"),
    Some(Permission::Write(100)),
).await; // Returns PermissionDenied error
```

### Option 1: Global Permissions (Recommended for Collaboration)

The simplest approach for collaborative databases is to use global permissions:

```rust,ignore
let mut settings = Doc::new();
let mut auth_doc = Doc::new();

// Add admin key
auth_doc.set_json("admin", serde_json::json!({
    "pubkey": admin_public_key,
    "permissions": {"Admin": 1},
    "status": "Active"
}))?;

// Add global permission for automatic bootstrap
auth_doc.set_json("*", serde_json::json!({
    "pubkey": "*",
    "permissions": {"Write": 10},  // Allows Read and Write(11+) requests
    "status": "Active"
}))?;

settings.set_doc("auth", auth_doc);
```

**Benefits**:

- No per-device key management required
- Immediate bootstrap approval
- Works even if `bootstrap_auto_approve: false`
- See [Bootstrap Guide](bootstrap.md#global-permission-bootstrap) for details

### Option 2: Auto-Approval Policy

To allow automatic key approval with per-device keys, configure the bootstrap policy:

<!-- Code block ignored: Complex authentication flow requiring policy setup -->

```rust,ignore
let mut settings = Doc::new();
settings.set_string("name", "Team Chat Room");

// Set up authentication with policy
let mut auth_doc = Doc::new();
let mut policy_doc = Doc::new();
policy_doc.set_json("bootstrap_auto_approve", true)?;
auth_doc.set_doc("policy", policy_doc);

// Include initial admin key
auth_doc.set_json("admin_device", serde_json::json!({
    "pubkey": admin_public_key,
    "permissions": {"Admin": 10},
    "status": "Active"
}))?;

settings.set_doc("auth", auth_doc);

let database = instance.new_database(settings, "admin_device")?;
```

### Policy Configuration

The bootstrap policy is stored in database settings at:

```text
_settings.auth.policy.bootstrap_auto_approve: bool (default: false)
```

**Security Recommendations**:

- **Development/Testing**: Enable auto-approval for convenience
- **Production**: Keep disabled, use manual key management
- **Team Collaboration**: Enable with proper access controls
- **Public Databases**: Always disabled for security

### Bootstrap Flow

1. **Client Request**: Device requests access with public key and permission level
2. **Global Permission Check**: Server checks if global '\*' permission satisfies request
3. **Policy Check**: If no global permission, server evaluates `bootstrap_auto_approve` setting
4. **Auto-Approval**: If policy enabled, key is automatically added to database auth settings
5. **Rejection**: If disabled, request requires manual admin approval
6. **Database Access**: Approved devices can read/write according to granted permissions

## See Also

- [Core Concepts](core_concepts.md) - Understanding Databases and Entries
- [Getting Started](getting_started.md) - Basic database setup
- [Authentication Details](../internal/core_components/authentication.md) - Technical implementation
