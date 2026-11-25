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
# let (instance, _) = Instance::connect_or_create_backend(
#     Box::new(backend),
#     eidetica::NewAccount::passwordless("admin"),
# ).await?;
#
// Create and login a passwordless user via the bootstrapped admin
// (the new user gets an Ed25519 keypair automatically)
let admin = instance.login_account("admin", "").await?;
admin.admin().await?.create_account(eidetica::NewAccount::passwordless("alice")).await?;
let mut user = instance.login_account("alice", "").await?;

// Create a database using the user's default key
let mut settings = Doc::new();
settings.set("name", "my_database");
let default_key = user.get_default_key()?;
let database = user.create_database(settings, &default_key).await?;

// All operations are now authenticated
let txn = database.new_transaction().await?;
// ... make changes ...
txn.commit().await?;  // Automatically signed
# Ok(())
# }
```

## Creating with explicit authority

`Database::create` and `Database::create_with_init` accept initial `auth` settings and an actual unlocked signer.
Supplying Auth preserves exactly those grants: there is no additional creator key or creator `Admin(0)` privilege.
Without an `auth` field, the convenience default grants the supplied key `Admin(0)`.
Explicit empty, deleted, malformed or unauthorized Auth is an error, not a request for that default.

### Direct keys: no Identity required

An app `Admin(20)` can sign genesis while preserving a stronger owner `Admin(0)`.
That does not let the app revoke, replace or demote the owner afterward.
The signer must have Admin permission for genesis, even when the initial application data needs only Write permission later.

```rust
use eidetica::{Database, Instance, backend::database::InMemory, crdt::Doc};
use eidetica::auth::{AuthSettings, AuthKey, Permission, crypto::PrivateKey};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (instance, _) = Instance::create_backend(
    Box::new(InMemory::new()), eidetica::NewAccount::passwordless("example"),
).await?;
let owner = PrivateKey::generate();
let app = PrivateKey::generate();
let mut auth = AuthSettings::new();
auth.add_key(&owner.public_key(), AuthKey::active(None, Permission::Admin(0)))?;
auth.add_key(&app.public_key(), AuthKey::active(None, Permission::Admin(20)))?;
let mut initial = Doc::new();
initial.set("name", "app data");
initial.set("auth", auth.as_doc().clone());
let db = Database::create(&instance, app, initial).await?;
assert_eq!(db.current_permission().await?, Permission::Admin(20));
let txn = db.new_transaction().await?;
txn.get_settings()?.revoke_auth_key(&owner.public_key()).await?;
assert!(txn.commit().await.is_err());
# Ok(())
# }
```

### Delegation-only and mixed Auth

A delegation source is an existing database declaring membership; it can be an Identity database.
Creating that Identity is separate, and a reference to one is not itself an Auth grant.
Both its configured pointer and the signer's claimed snapshot need complete locally Verified history.
In this self-contained example, normal creation/commits establish that history first.
On another instance, acquire and verify the proof before creating the target; creation does not fetch it for you.

```rust
use eidetica::{Database, Instance, backend::database::InMemory, crdt::Doc, database::DatabaseKey};
use eidetica::auth::{AuthSettings, AuthKey, Permission, crypto::PrivateKey};
use eidetica::auth::types::{DelegatedTreeRef, DelegationStep, KeyHint, PermissionBounds, SigKey, TreeReference};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (instance, _) = Instance::create_backend(
    Box::new(InMemory::new()), eidetica::NewAccount::passwordless("example"),
).await?;
let owner = PrivateKey::generate();
let app = PrivateKey::generate();
let identity = Database::create(&instance, owner, Doc::new()).await?;
let txn = identity.new_transaction().await?;
txn.get_settings()?.set_auth_key(&app.public_key(), AuthKey::active(None, Permission::Admin(20))).await?;
txn.commit().await?;
let tips = identity.snapshot().await?.into_tips();
let delegation = DelegatedTreeRef {
    tree: TreeReference { root: identity.root_id().clone(), tips: tips.clone() },
    permission_bounds: PermissionBounds { max: Permission::Admin(0), min: None },
};
let mut auth = AuthSettings::new();
auth.add_delegated_tree(delegation)?;
let mut initial = Doc::new();
initial.set("auth", auth.as_doc().clone());
let identity_path = SigKey::Delegation {
    path: vec![DelegationStep { tree: identity.root_id().clone(), tips }],
    hint: KeyHint::from_pubkey(&app.public_key()),
};
let delegated = Database::create(&instance,
    DatabaseKey::with_identity(app.clone(), identity_path), initial).await?;
assert!(delegated.get_settings().await?.auth_snapshot().await?.get_all_keys()?.is_empty());
let txn = delegated.new_transaction().await?;
txn.get_store::<eidetica::store::DocStore>("data").await?.set("message", "signed by app").await?;
txn.commit().await?;

// Mixed Auth: owner administration through the same Identity, app directly.
auth.add_key(&app.public_key(), AuthKey::active(None, Permission::Admin(20)))?;
let mut initial = Doc::new();
initial.set("auth", auth.as_doc().clone());
let mixed = Database::create(&instance, app, initial).await?;
assert_eq!(mixed.current_permission().await?, Permission::Admin(20));
# Ok(())
# }
```

`max: Admin(0), min: None` is a ceiling, not a promotion: the delegated app remains `Admin(20)` while the owner retains `Admin(0)`.
The app can create permitted equal/lower-priority grants afterward but cannot alter the stronger owner-bearing delegation.
`max: Write(20)` would instead clamp Admin to Write and cannot authorize genesis.
No new minimum-bound or public-wildcard semantics are introduced.

Pass a `DatabaseKey` for an exact name/global/delegation identity, or a `PrivateKey` for its direct identity.
A name-only hint is pinned to that signer's public key; an invalid path never falls back to a separate direct grant or connection login key.
The returned handle retains the signer and path for later reads and writes.
`create_with_init` uses the same Auth rules and keeps ordinary store initialization in the single genesis entry; its callback cannot open system stores.

Connected instances use the same API: the client signs and the daemon verifies before acknowledging a successful commit.
The new handle proves possession of its signer and uses its selected identity rather than the connection's login identity.
The existing delegated `current_permission` client-side introspection is not supported on connected instances; exercise actual operations instead.
A connected historical `verify_entry_signature` query also needs its acting identity to pass Read gates on each proof database, independently of successful commit validation.
See [credential and service boundaries](account_credentials.md#embedded-and-service-use).

### Failure and compatibility limits

- Unknown, revoked or wrong signing keys, insufficient permissions and invalid paths cannot create an authorized root.
- Missing or present-but-Unverified delegated history is unresolved: local creation returns an error; raw ingest retains the target as Unverified for a later retry.
  An empty snapshot, foreign-tree proof, failed proof or invalid signature is rejected.
  A connected submission that is not Verified returns an error, never a successful creation acknowledgement.
- A later entry cannot use newly staged Auth to authorize itself, even if it is the first Auth write in that history.
- This is causal authorization, not globally latest revocation or unseen-root trust enrollment.
  Existing live data and verification labels are not migrated, rewritten or deleted.
  Older validators that do not recognize delegation-only Auth must be upgraded before receiving new delegation-only histories.
- These are native database primitives, not a new Account provisioning, enrollment or recovery workflow.
  See [Account design](../design/accounts.md#explicit-genesis-authority) for the integration boundary.

The native/raw and connected regressions are in `crates/lib/tests/it/auth/genesis.rs`; subsequent priority cases remain in `auth/priority_enforcement.rs`.
The [internal flow](../internal/authentication.md#explicit-genesis-flow) and [design invariants](../design/authentication.md#explicit-genesis-authority) explain the shared enforcement.

## Key Concepts

**Mandatory Authentication**: Every entry must be signed - no exceptions.

**First Account Becomes Admin**: Whoever is named in the [`NewAccount`] passed to `Instance::connect_or_create` (URL-based) or [`Instance::create_backend`] (pre-built backend) is the instance's first user **and** is automatically granted `Admin(0)` on the system databases (`_users`, `_databases`). There is no hidden default account: production deployments run `eidetica daemon init --username <NAME> [--password PASS | --passwordless]` with explicit credentials. Subsequent users are created by an existing admin via `admin.admin().await?.create_account(NewAccount::…)` and land as non-admins until promoted.

[`Instance::create_backend`]: https://docs.rs/eidetica/latest/eidetica/struct.Instance.html#method.create_backend
[`NewAccount`]: https://docs.rs/eidetica/latest/eidetica/instance/struct.NewAccount.html

**Permission Levels**:

- **Admin**: Can modify settings and manage keys
- **Write**: Can read and write data
- **Read**: Can only read data

**Key Storage**: Public keys belong in database Auth settings; private-key custody is separate.
The login convenience path stores authenticated encrypted credential bundles in native installation-local records, not replicated Account Entries.
Passwords are required string values; every string is accepted, including empty, without length/complexity restrictions or an insecure-use acknowledgement.
`NewAccount::passwordless(name)` is sugar for `NewAccount::with_password(name, "")`; login uses `login_account(name, "")`.
Empty-string wrapping uses the same Argon2id/AES-256-GCM format, but provides no meaningful confidentiality against a bundle holder.
Unlocked signers remain usable without re-entering the password; rewrapping a password is not signing-key revocation.
There is no automatic migration or old plaintext-format import.
For explicit caller-held keys and daemon-independent signing, see [credential bundles and signers](account_credentials.md).

## Common Tasks

### Adding Users

Give other users access to your database:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::generate_keypair;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "auth_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let transaction = database.new_transaction().await?;
# // Generate a keypair for the new user
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
let settings_store = transaction.get_settings()?;

// Add a user with write access (indexed by pubkey, name is optional metadata)
let user_key = AuthKey::active(Some("alice_laptop"), Permission::Write(10));
settings_store.set_auth_key(&alice_verifying_key, user_key).await?;

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
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Global permission for public read access
// Any valid signature is accepted
let public_key = AuthKey::active(None, Permission::Read);
settings_store.set_global_auth_key(public_key).await?;

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
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "collaborative_notes");
# let default_key = user.get_default_key()?;
let database = user.create_database(settings, &default_key).await?;

// Set up global write permissions
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Global permission allows any device to read and write
let collaborative_key = AuthKey::active(None, Permission::Write(10));
settings_store.set_global_auth_key(collaborative_key).await?;

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
use eidetica::{Instance, backend::database::Sqlite};

let instance = Instance::open_backend(Box::new(Sqlite::in_memory().await?)).await?;

// Log in as an existing user
let mut user = instance.login_account("bob", "").await?;

let database_root_id = /* ID from existing collaborative database */;

// Open the database — the AccountSession resolves the signing key automatically
let database = user.open_database(&database_root_id).await?;

// Create transactions as usual — automatically signed with the user's key
let txn = database.new_transaction().await?;
// ... make changes ...
txn.commit().await?;
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
# use eidetica::auth::crypto::generate_keypair;
# use eidetica::crdt::Doc;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "test_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# // Generate a keypair for alice
# let (_alice_signing_key, alice_verifying_key) = generate_keypair();
// First add alice key so we can revoke it
let transaction_setup = database.new_transaction().await?;
let settings_setup = transaction_setup.get_settings()?;
settings_setup.set_auth_key(&alice_verifying_key, AuthKey::active(Some("alice"), Permission::Write(10))).await?;
transaction_setup.commit().await?;
let transaction = database.new_transaction().await?;

let settings_store = transaction.get_settings()?;

// Revoke the key by its pubkey identifier
settings_store.revoke_auth_key(&alice_verifying_key).await?;

transaction.commit().await?;
# Ok(())
# }
```

Historical entries created by revoked keys remain valid.

### Removing a key from a delegated identity

1. Call `Identity::revoke_key` with an authorized identity handle. Once a serving replica verifies that commit, the key no longer has **current** Read through that identity. It cannot discover the database or capture a new Snapshot on the strength of that membership alone.
2. In each database that delegates to the identity, use a signer authorized to update its Settings. In a transaction, read the existing delegation, keep its permission bounds, and use `Identity::as_delegation` with those bounds to build the updated reference. Check that the new identity Snapshot includes the revocation and advances the existing pin, then call `SettingsStore::add_delegated_tree` and commit. Syncing the identity alone does not write this commit for the delegating database.
3. To confirm the cutoff on a replica, check its Verified database Snapshot: the committed delegation must point to identity history that includes the revocation. An explicit read at that cut can no longer use the removed membership. A cut that still pins the grant remains readable, **even if it contains data written after the identity revoked the key**. Advancing the pin does not change those earlier cuts.

This cutoff is per database and causal, not a timestamp or a guarantee about offline replicas. It does not withdraw an independent grant or erase data already disclosed.

### Historical Signatures and Operating Keys

`Database::verify_entry_signature(id)` checks historical authorization and the signature, not current authority or application identity.
Service clients run the same check on the daemon; the handle's acting identity needs current Read on the target and every claimed delegated database, not Write or Admin.
Missing evidence or an unsupported query is not success.

To identify an operating key, fetch the Entry through an authorized handle, require historical verification to succeed, then call `auth::crypto::verify_entry_signature(&entry, &public_key)` with concrete authorized keys.
Read the identity database through its own authorized handle, and test each candidate if key names collide; names, hints and record labels do not identify a signer.
Check current membership and application policy separately: later revocation does not invalidate historical signatures.

## Multi-Account Setup Example

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::generate_keypair;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# // Setup database for testing
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "multi_user_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let transaction = database.new_transaction().await?;
# let settings_store = transaction.get_settings()?;
#
// Generate keypairs for different users
let (_super_admin_signing_key, super_admin_verifying_key) = generate_keypair();
let (_dept_admin_signing_key, dept_admin_verifying_key) = generate_keypair();
let (_user1_signing_key, user1_verifying_key) = generate_keypair();

// Set auth keys directly
// Super admin (priority 0 - highest)
settings_store.set_auth_key(&super_admin_verifying_key, AuthKey::active(
    Some("super_admin"),
    Permission::Admin(0),
)).await?;

// Department admin (priority 10)
settings_store.set_auth_key(&dept_admin_verifying_key, AuthKey::active(
    Some("dept_admin"),
    Permission::Admin(10),
)).await?;

// Regular users (priority 100)
settings_store.set_auth_key(&user1_verifying_key, AuthKey::active(
    Some("user1"),
    Permission::Write(100),
)).await?;

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
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let default_key = user.get_default_key()?;
#
# // Create user's personal database
# let alice_database = user.create_database(Doc::new(), &default_key).await?;
#
# // Create main project database
# let project_database = user.create_database(Doc::new(), &default_key).await?;
// Get the user's database root and current tips
let user_root = alice_database.root_id().clone();
let user_tips = alice_database.snapshot().await?.into_tips();

// Add delegation reference to project database
let transaction = project_database.new_transaction().await?;
let settings = transaction.get_settings()?;

settings.add_delegated_tree(DelegatedTreeRef {
    permission_bounds: PermissionBounds {
        max: Permission::Write(15),
        min: Some(Permission::Read),
    },
    tree: TreeReference {
        root: user_root,
        tips: user_tips,
    },
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
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let default_key = user.get_default_key()?;
# let alice_db = user.create_database(Doc::new(), &default_key).await?;
# let alice_root = alice_db.root_id().clone();
# let alice_tips = alice_db.snapshot().await?.into_tips();
# let project_db = user.create_database(Doc::new(), &default_key).await?;
# let transaction = project_db.new_transaction().await?;
# let settings = transaction.get_settings()?;
// In project database: delegation stored by Alice's database root ID
settings.add_delegated_tree(DelegatedTreeRef {
    tree: TreeReference {
        root: alice_root,  // <- Root ID used as storage key
        tips: alice_tips,
    },
    permission_bounds: PermissionBounds {
        max: Permission::Write(15),
        min: Some(Permission::Read),
    },
}).await?;
# transaction.commit().await?;
# Ok(())
# }
```

This creates an entry in the project database's auth settings:

- **Key**: The root entry ID of Alice's database (a CID string, e.g., `bafyr4i...`)
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
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let default_key_id = user.get_default_key()?;
# let alice_db = user.create_database(Doc::new(), &default_key_id).await?;
# let transaction = alice_db.new_transaction().await?;
# let settings = transaction.get_settings()?;
// In Alice's database: keys are indexed by pubkey
// (The default key was added automatically during bootstrap)
// We can update it with a descriptive name
settings.set_auth_key(
    &default_key_id,
    AuthKey::active(
        Some("alice_work"),  // Optional name metadata
        Permission::Write(10),
    )
).await?;
# transaction.commit().await?;
# Ok(())
# }
```

This updates the entry in Alice's database auth settings:

- **Key**: Indexed by the pubkey string (e.g., `"ed25519:ABC..."`)
- **Name**: `"alice_work"` (optional human-readable metadata)
- **Permissions**: The access level for this key

### Using Delegated Keys

A delegation path names each delegated root and the snapshot the signer observed.
Claims must cover the configured pointer and observations inherited from all parents;
complete locally `Verified` dependency history is required.
Admins may rewind a configured pointer, but that alone does not erase inherited
floors. Effective removal resets only the direct component, not nested observations.
See the canonical [causal rules and examples](../design/authentication.md#causal-snapshot-validation).
If proof is incomplete, [verify dependencies first and retry](cli.md#re-verification-after-reset-or-incomplete-proof);
automatic dependency acquisition/retry is deferred to [PR #126](https://github.com/arcuru/eidetica/pull/126).

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
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let default_key = user.get_default_key()?;
# let project_db = user.create_database(Doc::new(), &default_key).await?;
# let user_db = user.create_database(Doc::new(), &default_key).await?;
# let user_tips = user_db.snapshot().await?.into_tips();
// Create a delegation path:
// - path: list of delegated trees to traverse (by root ID)
// - hint: identifies the final signer in the last delegated tree
let delegation_path = SigKey::Delegation {
    path: vec![
        // Step: Traverse to Alice's database (using its root ID)
        DelegationStep {
            tree: user_db.root_id().clone(),  // Root ID of delegated database
            tips: user_tips,  // Tips for Alice's database
        },
    ],
    // Final signer hint - resolved in Alice's database auth settings
    hint: eidetica::auth::KeyHint::from_name("alice_laptop"),
};

// Use the delegation path to create an authenticated transaction
// Note: This requires the actual signing key to be available
// project_database.new_transaction_with_sig_key(delegation_path)?;
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
Account DB key: Admin(5)  →  Project DB clamps to: Write(15)  (max bound)
Account DB key: Write(10) →  Project DB keeps:      Write(10) (within bounds)
Account DB key: Read      →  Project DB keeps:      Read      (above min bound)
```

**Rules**:

- If delegated permission > max bound: lowered to max
- If delegated permission < min bound: raised to min (if specified)
- Permissions within bounds are preserved
- Admin permissions only apply within the delegated database

This makes it convenient to reuse the same validation rules across both databases. Only an Admin can grant permissions to a database by modifying the Auth Settings, but we can grant lower access to an Account, and allow it to use any key they want, by granting access to an Account-controlled database and giving **that** the desired permissions. The Account can then manage its own keys using its own Admin keys, under exactly the same rules.

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

**Account-Managed Access**:

```text
Project DB → delegates to → Alice's Personal DB
                              ↓
                         Alice manages her own keys
```

**Team Hierarchy**:

```text
Main DB → delegates to → Team DB → delegates to → Account DB
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

### Causal settings pins and ordinary Store branches

Every Entry's signed `settings_tips` identifies its complete causal pre-write
`_settings` frontier. Validation derives that frontier independently; a signed
stale, omitted or forged pin is not authority. Settings writes must also consume
that exact frontier, with a genesis-only bootstrap exception. This is stricter
than ordinary Stores, whose omitted observed tips remain readable as independent
branches. Both still require named parents in the same Store/database and main
ancestry, and strictly increasing effective Store heights. The status is for
the whole Entry, not separate contributions or sparse verification. See the
[verification contract](../design/verification.md#store-parent-frontiers).

### Upgrade prerequisite

Before trusting an existing database under these rules, follow the
[offline trust-reset and re-verification procedure](cli.md#db-reset-local-verification-offline-trust-reset).
There is no automatic migration: skipping it may trust legacy `Verified` labels.
Clearing only the derived cache is not a substitute.

### Best Practices

1. **Use descriptive key names**: `"alice_laptop"`, `"deploy_bot"` for keys that will be looked up by name
2. **Set appropriate permission bounds**: Don't grant more access than needed
3. **Manage delegation tips deliberately**: A pointer rewind alone does not reset inherited floors. See the [rewind, effective-removal and nested examples](../design/authentication.md#causal-snapshot-validation) before using removal as a recovery tool. To move a pin forward, use `SettingsStore::advance_delegation`; see [refreshing delegating databases](identity_guide.md#refresh-delegating-databases-after-removing-a-key)
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

**Settings commit rejected with `EntryValidationFailed`**: Changing a key, delegation or global permission requires:

- Admin-level permissions
- Equal or higher priority than the grant being changed, both before and after the change (you cannot grant more than you hold)

These checks run on signed entries at commit and remote verification, including raw settings writes.
They use the entry's causal pre-write settings, not unrelated newer grants or revocations on another branch.
Delegated signers use their clamped permission; changing a delegation is checked at its old and new bounds.
Raw fragments that leave malformed grants or auth containers are rejected, even for `Admin(0)`.
Partial updates remain allowed when the resulting grant is complete and valid.
See the [priority rules and regression coverage](../design/authentication.md#priority-system).

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
use eidetica::{sync::DatabaseTicket, account::types::SyncSettings};

// Bootstrap will fail without explicit policy configuration
user.request_database_access(
    &sync,
    &ticket,
    &key_id,  // Account's key ID
    Permission::Write(100),
    SyncSettings::disabled(),
    None,
).await; // Returns PermissionDenied error
```

### Global Wildcard Permissions (Recommended for Collaboration)

The simplest approach for collaborative databases is to use global wildcard permissions:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::SettingsStore};
# use eidetica::auth::{AuthKey, Permission};
# use eidetica::auth::crypto::generate_keypair;
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, mut user) = eidetica::Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     eidetica::NewAccount::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "wildcard_example");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# let (_admin_signing_key, admin_verifying_key) = generate_keypair();
let transaction = database.new_transaction().await?;
let settings_store = transaction.get_settings()?;

// Add admin key
settings_store.set_auth_key(&admin_verifying_key, AuthKey::active(
    Some("admin"),
    Permission::Admin(1),
)).await?;

// Add global wildcard permission for automatic bootstrap
settings_store.set_global_auth_key(AuthKey::active(
    None,
    Permission::Write(10),  // Allows Read and Write(11+) requests
)).await?;

transaction.commit().await?;
# Ok(())
# }
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
