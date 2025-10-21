# User System

## Purpose and Architecture

The User system provides multi-user account management with per-user key management, database tracking, and sync preferences. Each user maintains their own encrypted private database for storing keys, database preferences, and personal settings.

## Key Responsibilities

**Account Management**: User creation, login/logout with optional password protection, and session management.

**Key Management**: Per-user encryption keys with secure storage, key-to-SigKey mapping for database access, and automatic SigKey discovery.

**Database Tracking**: Per-user list of tracked databases with individual sync preferences, automatic permission discovery, and preference management.

**Secure Storage**: User data stored in a private database, with password-based encryption for the private keys of password-protected users.

## Design Principles

- **Session-Based**: All user operations happen through User session objects obtained via login
- **Secure by Default**: User keys never stored in plaintext, passwords hashed with Argon2id
- **Separation of Concerns**: User manages preferences, other modules read preferences and adjust Instance behavior
- **Auto-Discovery**: Automatic SigKey discovery using database permissions
- **Multi-User Support**: Different users can have different preferences for the same database

## Data Model

### UserKey

Each user has one or more cryptographic keys for database authentication:

```rust,ignore
pub struct UserKey {
    /// Unique identifier for this key
    pub key_id: String,

    /// Encrypted private key bytes
    pub encrypted_key: Vec<u8>,

    /// Encryption nonce
    pub nonce: Vec<u8>,

    /// Per-database SigKey mappings
    pub database_sigkeys: HashMap<ID, String>,

    /// When this key was created
    pub created_at: u64,

    /// Optional display name
    pub display_name: Option<String>,
}
```

The `database_sigkeys` HashMap maps database IDs to SigKey identifiers, allowing each user key to authenticate with multiple databases using different SigKeys.

### UserDatabasePreferences

Tracks which databases a user wants to sync and their sync configuration:

```rust,ignore
pub struct UserDatabasePreferences {
    /// Database ID being tracked
    pub database_id: ID,

    /// Which user key to use for this database
    pub key_id: String,

    /// User's sync preferences for this database
    pub sync_settings: SyncSettings,

    /// When user added this database
    pub added_at: u64,
}
```

### SyncSettings

Per-database sync configuration:

```rust,ignore
pub struct SyncSettings {
    /// Whether user wants to sync this database
    pub sync_enabled: bool,

    /// Sync on commit
    pub sync_on_commit: bool,

    /// Sync interval (if periodic)
    pub interval_seconds: Option<u64>,

    /// Additional sync configuration
    pub properties: HashMap<String, String>,
}
```

## Storage Architecture

Each user has a private database: `user:<username>`

- **keys Table**: Stores `UserKey` entries with encrypted private keys
- **databases Table**: Stores `UserDatabasePreferences` for tracked databases
- **settings DocStore**: User preferences and configuration

## Database Tracking Flow

When a user adds a database to track:

1. **Validate Input**: Check database isn't already tracked, verify key_id exists
2. **Derive Public Key**: Get public key from the user's private key
3. **Auto-Discovery**: Call `Database::find_sigkeys()` with user's public key
4. **Permission Sorting**: Results sorted by permission level (Admin > Write > Read)
5. **Select Best**: Choose highest-permission SigKey from results
6. **Store Mapping**: Save SigKey mapping in `UserKey.database_sigkeys`
7. **Save Preferences**: Store `UserDatabasePreferences` in databases Table
8. **Commit**: Changes persisted to backend

This automatic discovery eliminates the need for users to manually specify which SigKey to use - the system finds the best available access level.

## Key Management

### Adding Keys

```rust,ignore
user.add_private_key(Some("backup_key"))?;
```

Keys are:

1. Generated as Ed25519 keypairs
2. Encrypted using user's encryption key (derived from password or master key)
3. Stored in the user's private database
4. Never persisted in plaintext

### Key-to-SigKey Mapping

```rust,ignore
user.map_key("my_key", &db_id, "sigkey_id")?;
```

Manual mapping is supported for advanced use cases, but most applications use auto-discovery via `add_database()`.

### Default Keys

Each user has a default key (usually created during account creation) accessible via:

```rust,ignore
let default_key_id = user.get_default_key()?;
```

## API Surface

### User Creation and Login

```rust,ignore
// Create user (on Instance)
instance.create_user("alice", Some("password"))?;

// Login to get User session
let user = instance.login_user("alice", Some("password"))?;
```

### Database Tracking

```rust,ignore
// Add database to tracking
let prefs = DatabasePreferences {
    database_id: db_id.clone(),
    key_id: user.get_default_key()?,
    sync_settings: SyncSettings {
        sync_enabled: true,
        sync_on_commit: false,
        interval_seconds: Some(60),
        properties: Default::default(),
    },
};
user.add_database(prefs)?;

// List tracked databases
let databases = user.list_database_prefs()?;

// Get specific preferences
let prefs = user.database_prefs(&db_id)?;

// Update preferences (upsert behavior)
user.set_database(new_prefs)?;

// Remove from tracking
user.remove_database(&db_id)?;

// Load a tracked database
let database = user.open_database(&db_id)?;
```

### Key Management

```rust,ignore
// Add a key
user.add_private_key(Some("device_key"))?;

// List all keys
let keys = user.list_keys()?;

// Get default key
let default = user.get_default_key()?;

// Set database-specific SigKey mapping
user.map_key("my_key", &db_id, "sigkey_id")?;
```

## Security Considerations

### Password Protection

Password-protected users use Argon2id for key derivation:

```rust,ignore
let config = argon2::Config {
    variant: argon2::Variant::Argon2id,
    // ... secure parameters
};
```

This provides resistance against:

- Brute force attacks
- Rainbow table attacks
- Side-channel timing attacks

### Key Storage

- Private keys encrypted at rest
- Decrypted keys only held in memory during User session
- Keys cleared from memory on logout
- No plaintext key material ever persisted

### Permission System Integration

The User system integrates with the permission system via SigKey discovery:

1. User's public key derived from private key
2. Database queried for SigKeys matching that public key
3. Results include permission level (Direct or DelegationPath)
4. Highest permission selected automatically
5. Currently only Direct SigKeys supported (DelegationPath planned)

## Multi-User Support

Different users can track the same database with different preferences:

- Each user has independent tracking lists
- Each user can use different keys for the same database
- Each user can configure different sync settings
- No coordination needed between users
