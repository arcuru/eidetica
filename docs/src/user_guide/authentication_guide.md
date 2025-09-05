# Authentication Guide

How to use Eidetica's authentication system for securing your data.

## Quick Start

Every Eidetica database requires authentication. Here's the minimal setup:

```rust,ignore
use eidetica::{Instance, backend::database::InMemory};
use eidetica::crdt::Doc;

// Create database
let database = InMemory::new();
let db = Instance::new(Box::new(database));

// Add an authentication key (generates Ed25519 keypair)
db.add_private_key("my_key")?;

// Create a database using that key
let mut settings = Doc::new();
settings.set("name", "my_database");
let database = db.new_database(settings, "my_key")?;

// All operations are now authenticated
let op = database.new_operation()?;
// ... make changes ...
op.commit()?;  // Automatically signed
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

```rust,ignore
use eidetica::auth::{AuthKey, Permission, KeyStatus};

let op = database.new_operation()?;
let auth = op.auth_settings()?;

// Add a user with write access
let user_key = AuthKey {
    key: "ed25519:USER_PUBLIC_KEY_HERE".to_string(),
    permissions: Permission::Write(10),
    status: KeyStatus::Active,
};
auth.add_key("alice", user_key)?;

op.commit()?;
```

### Making Data Public

Allow anyone to read your database:

```rust,ignore
let op = database.new_operation()?;
let auth = op.auth_settings()?;

// Wildcard key for public read access
let public_key = AuthKey {
    key: "*".to_string(),
    permissions: Permission::Read,
    status: KeyStatus::Active,
};
auth.add_key("*", public_key)?;

op.commit()?;
```

### Revoking Access

Remove a user's access:

```rust,ignore
let op = database.new_operation()?;
let auth = op.auth_settings()?;

// Revoke the key
if let Some(mut key) = auth.get_key("alice")? {
    key.status = KeyStatus::Revoked;
    auth.update_key("alice", key)?;
}

op.commit()?;
```

Note: Historical entries created by revoked keys remain valid.

## Multi-User Setup Example

```rust,ignore
// Initial setup with admin hierarchy
let op = database.new_operation()?;
let auth = op.auth_settings()?;

// Super admin (priority 0 - highest)
auth.add_key("super_admin", AuthKey {
    key: "ed25519:SUPER_ADMIN_KEY".to_string(),
    permissions: Permission::Admin(0),
    status: KeyStatus::Active,
})?;

// Department admin (priority 10)
auth.add_key("dept_admin", AuthKey {
    key: "ed25519:DEPT_ADMIN_KEY".to_string(),
    permissions: Permission::Admin(10),
    status: KeyStatus::Active,
})?;

// Regular users (priority 100)
auth.add_key("user1", AuthKey {
    key: "ed25519:USER1_KEY".to_string(),
    permissions: Permission::Write(100),
    status: KeyStatus::Active,
})?;

op.commit()?;
```

## Key Management Tips

1. **Use descriptive key names**: "alice_laptop", "build_server", etc.
2. **Set up admin hierarchy**: Lower priority numbers = higher authority
3. **Regular key rotation**: Periodically update keys for security
4. **Backup admin keys**: Keep secure copies of critical admin keys

## Advanced: Cross-Database Authentication

Databases can delegate authentication to other databases:

```rust,ignore
// In main database, delegate to a user's personal database
let op = main_tree.new_operation()?;
let auth = op.auth_settings()?;

// Reference another database for authentication
auth.add_delegated_tree("user@example.com", DelegatedTreeRef {
    tree_root: "USER_TREE_ROOT_ID".to_string(),
    max_permission: Permission::Write(15),
    min_permission: Some(Permission::Read),
})?;

op.commit()?;
```

This allows users to manage their own keys in their personal databases while accessing your database with appropriate permissions.

## Troubleshooting

**"Authentication failed"**: Check that:

- The key exists in database settings
- The key status is Active (not Revoked)
- The key has sufficient permissions for the operation

**"Cannot modify key"**: Admin operations require:

- Admin-level permissions
- Equal or higher priority than the target key

**Network partitions**: Authentication changes merge automatically using Last-Write-Wins. The most recent change takes precedence.

## See Also

- [Core Concepts](core_concepts.md) - Understanding Databases and Entries
- [Getting Started](getting_started.md) - Basic database setup
- [Authentication Details](../internal/core_components/authentication.md) - Technical implementation
