# Authentication Guide

This guide explains how to work with Eidetica's mandatory authentication system. All entries in Eidetica must be cryptographically signed with Ed25519 keys.

## Overview

Eidetica implements mandatory authentication for all database operations:

- Every entry must be signed with a valid Ed25519 private key
- Keys are managed in the tree configuration
- Permission levels control what operations each key can perform
- All authentication data is tracked in the immutable history

## Key Concepts

### Authentication Keys

Each key in Eidetica consists of:

- **Key ID**: A unique identifier for the key (e.g., "LAPTOP_KEY", "SERVER_KEY")
- **Public Key**: Ed25519 public key in format `ed25519:<base64>`
- **Permissions**: Access level (Admin, Write, or Read)
- **Status**: Active or Revoked

### Permission Levels

```rust
// Permission hierarchy with integrated priority
enum Permission {
    Admin(u32),  // Full access, can manage other keys
    Write(u32),  // Read/write data, cannot modify settings
    Read,        // Read-only access
}
```

Lower priority numbers indicate higher administrative authority. Priority 0 is typically the root admin.

## Working with Authentication

### Initial Setup

When creating a new database, you must add at least one authentication key:

```rust
use eidetica::{BaseDB, backend::database::InMemory};

// Create database
let database = InMemory::new();
let db = BaseDB::new(Box::new(database));

// Add your first authentication key
// This generates a new Ed25519 keypair and stores it
db.add_private_key("ADMIN_KEY")?;

// List available keys
let keys = db.list_private_keys();
println!("Available keys: {:?}", keys);
```

### Creating an Authenticated Tree

All trees require authentication from creation:

```rust
use eidetica::crdt::Nested;

// Create tree settings
let mut settings = Nested::new();
settings.set_string("name", "my_data");

// Create tree with authentication
// The key will be used to sign the root entry
let tree = db.new_tree(settings, "ADMIN_KEY")?;
```

### Performing Authenticated Operations

All operations are automatically authenticated using the tree's default key:

```rust
use eidetica::subtree::KVStore;

// Start an operation (uses tree's default authentication key)
let op = tree.new_operation()?;

// Make changes
let store = op.get_subtree::<KVStore>("config")?;
store.set("api_url", "https://api.example.com")?;

// Commit (automatically signs with the tree's key)
let entry_id = op.commit()?;
```

### Managing Authentication Keys

To manage keys within a tree, you need Admin permissions:

```rust
use eidetica::auth::{AuthKey, Permission, KeyStatus};

// Start an admin operation
let op = tree.new_operation()?;

// Get the auth settings interface
let auth_settings = op.auth_settings()?;

// Add a new write-access key
let new_key = AuthKey {
    key: "ed25519:QJ7bKAM9mK_mH3L5EDwszC437uRzTqAbxpk".to_string(),
    permissions: Permission::Write(10),
    status: KeyStatus::Active,
};
auth_settings.add_key("WRITER_KEY", new_key)?;

// Update an existing key's status
if let Some(mut key) = auth_settings.get_key("OLD_KEY")? {
    key.status = KeyStatus::Revoked;
    auth_settings.update_key("OLD_KEY", key)?;
}

op.commit()?;
```

### Public Read Access

To make a tree publicly readable, add a wildcard key:

```rust
let op = tree.new_operation()?;
let auth_settings = op.auth_settings()?;

// Add wildcard key for public read access
let public_key = AuthKey {
    key: "*".to_string(),
    permissions: Permission::Read,
    status: KeyStatus::Active,
};
auth_settings.add_key("*", public_key)?;

op.commit()?;
```

## Key Management Best Practices

### Priority System

When setting up administrative keys, use priority levels to create a recovery hierarchy:

```rust
// Root admin - highest priority
let root_admin = AuthKey {
    key: "ed25519:ROOT_KEY_PUBLIC_KEY".to_string(),
    permissions: Permission::Admin(0),  // Priority 0
    status: KeyStatus::Active,
};

// Regular admin - lower priority
let regular_admin = AuthKey {
    key: "ed25519:ADMIN_KEY_PUBLIC_KEY".to_string(),
    permissions: Permission::Admin(10),  // Priority 10
    status: KeyStatus::Active,
};

// The root admin can modify the regular admin's key,
// but not vice versa
```

### Key Revocation

To revoke a compromised key:

```rust
let op = tree.new_operation()?;
let auth_settings = op.auth_settings()?;

// Revoke the compromised key
if let Some(mut key) = auth_settings.get_key("COMPROMISED_KEY")? {
    key.status = KeyStatus::Revoked;
    auth_settings.update_key("COMPROMISED_KEY", key)?;
}

op.commit()?;
```

Revoked keys:

- Cannot create new entries
- Historical entries remain valid
- Content is preserved during merges

### Storing Private Keys

Eidetica stores private keys in the BaseDB instance. For production use:

1. Use separate key management for different environments
2. Regularly rotate keys for enhanced security
3. Keep backups of critical admin keys
4. Consider using hardware security modules for key storage

## Authentication in Distributed Systems

### Conflict Resolution

When authentication settings conflict during merges, Eidetica uses Last Write Wins (LWW) based on the DAG structure:

- Priority does NOT affect merge conflict resolution
- The most recent change (by DAG ordering) takes precedence
- All changes are preserved in history

### Network Partitions

During network partitions:

- Each partition can continue operations with valid keys
- Authentication changes merge deterministically on reconnection
- Revoked keys are eventually consistent across the network

## Example: Multi-User Application

Here's a complete example showing authentication in a multi-user context:

```rust
use eidetica::{BaseDB, Tree, backend::database::InMemory};
use eidetica::auth::{AuthKey, Permission, KeyStatus};
use eidetica::crdt::Nested;

fn setup_multi_user_app() -> Result<Tree> {
    // Create database with admin key
    let database = InMemory::new();
    let db = BaseDB::new(Box::new(database));
    db.add_private_key("SUPER_ADMIN")?;

    // Create application tree
    let mut settings = Nested::new();
    settings.set_string("name", "multi_user_app");
    let tree = db.new_tree(settings, "SUPER_ADMIN")?;

    // Set up authentication hierarchy
    let op = tree.new_operation()?;
    let auth_settings = op.auth_settings()?;

    // Add department admin
    let dept_admin = AuthKey {
        key: "ed25519:DEPT_ADMIN_PUBLIC_KEY".to_string(),
        permissions: Permission::Admin(10),
        status: KeyStatus::Active,
    };
    auth_settings.add_key("DEPT_ADMIN", dept_admin)?;

    // Add regular users
    let user1 = AuthKey {
        key: "ed25519:USER1_PUBLIC_KEY".to_string(),
        permissions: Permission::Write(100),
        status: KeyStatus::Active,
    };
    auth_settings.add_key("USER1", user1)?;

    // Add read-only auditor
    let auditor = AuthKey {
        key: "ed25519:AUDITOR_PUBLIC_KEY".to_string(),
        permissions: Permission::Read,
        status: KeyStatus::Active,
    };
    auth_settings.add_key("AUDITOR", auditor)?;

    op.commit()?;

    Ok(tree)
}
```

## Security Considerations

1. **Key Compromise**: If an admin key is compromised, use a higher-priority key to revoke it
2. **Audit Trail**: All authentication changes are recorded in the immutable history
3. **Network Security**: Use secure channels for key distribution
4. **Key Rotation**: Implement regular key rotation policies

## Advanced Features

The following advanced features are fully implemented:

- **Delegated Trees**: ✅ Reference other trees for authentication
- **Permission Bounds**: ✅ Constrain delegated permissions with clamping
- **Cross-Tree Authentication**: ✅ Share authentication across projects
- **Delegation Depth Limits**: ✅ Prevent circular delegation (MAX_DELEGATION_DEPTH=10)

### Future Enhancements

- **Advanced Key Status**: Ignore and Banned statuses for more granular control
