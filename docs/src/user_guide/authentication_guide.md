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
# let db = Instance::new(Box::new(database));
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
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set_string("name", "auth_example");
# let database = db.new_database(settings, "admin")?;
# let transaction = database.new_transaction()?;
# // Generate a keypair for the new user
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
# let alice_public_key = format_public_key(&alice_verifying_key);
let settings_store = SettingsStore::new(&transaction)?;

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

### Making Data Public

Allow anyone to read your database:

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory};
# use eidetica::store::SettingsStore;
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::crdt::Doc;
#
# fn main() -> eidetica::Result<()> {
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "admin")?;
# let transaction = database.new_transaction()?;
let settings_store = SettingsStore::new(&transaction)?;

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
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let database = db.new_database(settings, "admin")?;
// First add alice key so we can revoke it
let transaction_setup = database.new_transaction()?;
let settings_setup = SettingsStore::new(&transaction_setup)?;
settings_setup.set_auth_key("alice", AuthKey::active("*", Permission::Write(10))?)?;
transaction_setup.commit()?;
let transaction = database.new_transaction()?;

let settings_store = SettingsStore::new(&transaction)?;

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
# let db = Instance::new(Box::new(InMemory::new()));
# db.add_private_key("admin")?;
# let mut settings = Doc::new();
# settings.set_string("name", "multi_user_example");
# let database = db.new_database(settings, "admin")?;
# let transaction = database.new_transaction()?;
# let settings_store = SettingsStore::new(&transaction)?;
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

## Advanced: Cross-Database Authentication

Databases can delegate authentication to other databases:

<!-- Code block ignored: Complex authentication flow requiring policy setup -->

```rust,ignore
let transaction = main_database.new_transaction()?;
let settings_store = SettingsStore::new(&transaction)?;

// Use update_auth_settings for delegation setup
settings_store.update_auth_settings(|auth| {
    // Reference another database for authentication
    auth.add_delegated_tree("user@example.com", DelegatedTreeRef {
        tree_root: "USER_TREE_ROOT_ID".to_string(),
        max_permission: Permission::Write(15),
        min_permission: Some(Permission::Read(1)),
    })?;
    Ok(())
})?;

transaction.commit()?;
```

This allows users to manage their own keys in their personal databases while accessing your database with appropriate permissions.

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

When new devices join existing databases through bootstrap synchronization, Eidetica uses configurable security policies to control automatic key approval.

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

### Enabling Bootstrap Auto-Approval

To allow automatic key approval, configure the bootstrap policy:

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

### Bootstrap Flow with Policy

1. **Client Request**: Device requests access with public key and permission level
2. **Policy Check**: Server evaluates `bootstrap_auto_approve` setting
3. **Auto-Approval**: If enabled, key is automatically added to database auth settings
4. **Rejection**: If disabled, request fails with `PermissionDenied` error
5. **Database Access**: Approved devices can read/write according to granted permissions

## See Also

- [Core Concepts](core_concepts.md) - Understanding Databases and Entries
- [Getting Started](getting_started.md) - Basic database setup
- [Authentication Details](../internal/core_components/authentication.md) - Technical implementation
