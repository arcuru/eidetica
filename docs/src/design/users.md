> ✅ **Status: Implemented**
>
> This design is fully implemented and functional.

# Users System

This design document outlines a comprehensive multi-user system for Eidetica that provides user isolation, password-based authentication, and per-user key management.

## Problem Statement

The current implementation has no concept of users:

1. **No User Isolation**: All keys and settings are stored at the Instance level, shared across all operations.

2. **No Authentication**: There's no way to protect access to private keys or restrict database operations to specific users.

3. **No Multi-User Support**: Only one implicit "user" can work with an Instance at a time.

4. **Key Management Challenges**: All private keys are accessible to anyone with Instance access, with no encryption or access control.

5. **No User Preferences**: Users cannot have personalized settings for which databases they care about, sync preferences, etc.

## Goals

1. **Unified Architecture**: Single implementation that supports both embedded (single-user ergonomics) and server (multi-user) use cases.

2. **Multi-User Support**: Multiple users can have accounts on a single Instance, each with isolated keys and preferences.

3. **Password-Based Authentication**: Users authenticate with passwords to access their keys and perform operations.

4. **User Isolation**: Each user's private keys and preferences are encrypted and isolated from other users.

5. **Root User**: A special system user that the Instance uses for infrastructure operations.

6. **User Preferences**: Users can configure which databases they care about and how they want to sync them.

7. **Database Tracking**: Instance-wide visibility into which databases exist and which users access them.

8. **Ergonomic APIs**: Simple single-user API for embedded apps, explicit multi-user API for servers (both build on same foundation).

## Non-Goals

1. **Multi-Factor Authentication**: Advanced auth methods deferred to future work.
2. **Role-Based Access Control**: Complex permission systems beyond user isolation are out of scope.
3. **User Groups**: Team/organization features are not included.
4. **Federated Identity**: External identity providers are not addressed.

## Proposed Solution

### Architecture Overview

The system separates infrastructure management (Instance) from contextual operations (User):

```text
Instance (Infrastructure Layer)
├── InstanceMetadata (persisted in backend)
│   ├── device_key (SigningKey for Instance identity)
│   ├── users_db (ID of _users system database)
│   ├── databases_db (ID of _databases system database)
│   └── sync_db (ID of _sync database, if enabled)
│
├── System Databases (authenticated with device_key)
│   ├── _instance
│   │   └── Instance configuration and metadata
│   ├── _users (Table with UUID primary keys)
│   │   └── User directory: Maps UUID → UserInfo (username stored in UserInfo)
│   ├── _databases
│   │   └── Database tracking: Maps database_id → DatabaseTracking
│   └── _sync
│       └── Sync configuration and bootstrap requests
│
└── User Management
    ├── User creation (with or without password)
    └── User login (returns User session)

User (Operations Layer - returned from login)
├── User session with decrypted keys
├── Database operations (new, load, find)
├── Key management (add, list, get)
└── User preferences
```

**Key Architectural Principle**: Instance handles infrastructure (user accounts, backend, system databases). User handles all contextual operations (database creation, key management). All operations run in a User context after login.

### Core Data Structures

#### 1. UserInfo (stored in `_users` database)

**Storage**: Users are stored in a Table with auto-generated UUID primary keys. The username field is used for login lookups via search operations.

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserInfo {
    /// Unique username (login identifier)
    /// Note: Stored with UUID primary key in Table, username used for search
    pub username: String,

    /// ID of the user's private database
    pub user_database_id: ID,

    /// Password hash (using Argon2id)
    /// None for passwordless users (single-user embedded mode)
    pub password_hash: Option<String>,

    /// Salt for password hashing (base64 encoded string)
    /// None for passwordless users (single-user embedded mode)
    pub password_salt: Option<String>,

    /// User account creation timestamp (Unix timestamp)
    pub created_at: i64,

    /// Account status
    pub status: UserStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum UserStatus {
    Active,
    Disabled,
    Locked,
}
```

#### 2. UserProfile (stored in user's private database `_settings` subtree)

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserProfile {
    /// Username
    pub username: String,

    /// Display name
    pub display_name: Option<String>,

    /// Email or other contact info
    pub contact_info: Option<String>,

    /// User preferences
    pub preferences: UserPreferences,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserPreferences {
    /// Default sync behavior
    pub default_sync_enabled: bool,

    /// Other user-specific settings
    pub properties: HashMap<String, String>,
}
```

#### 3. UserKey (stored in user's private database `keys` subtree)

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserKey {
    /// Public key identifier
    pub key_id: PublicKey,

    /// Key storage (encrypted ciphertext or plaintext PrivateKey)
    pub storage: KeyStorage,

    /// Display name for this key
    pub display_name: Option<String>,

    /// When this key was created (Unix timestamp)
    pub created_at: i64,

    /// Last time this key was used (Unix timestamp)
    pub last_used: Option<i64>,

    /// Whether this is the user's default key, which has admin access on the user's DB
    /// Only one key should be marked as default at a time
    pub is_default: bool,

    /// Database-specific SigKey mappings
    /// None = default pubkey identity, Some(sigkey) = non-default identity
    pub database_sigkeys: HashMap<ID, Option<SigKey>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum KeyStorage {
    /// Key is encrypted with a password-derived key (AES-256-GCM)
    Encrypted {
        /// Encryption algorithm identifier
        algorithm: String,
        /// Encrypted prefixed-string-encoded PrivateKey
        ciphertext: Vec<u8>,
        /// Encryption nonce/IV (12 bytes for AES-GCM)
        nonce: Vec<u8>,
    },
    /// Key is stored unencrypted (passwordless users only)
    Unencrypted {
        /// PrivateKey stored directly — serde carries the signing algorithm tag
        key: PrivateKey,
    },
}
```

#### 4. TrackedDatabase (stored in user's private database `databases` Table)

**Purpose**: Tracks which databases a user has added to their list, along with sync preferences. The User tracks what they want (sync_enabled, sync_on_commit), while the Sync module tracks actual status (last_synced, connection state). This separation allows multiple users with different sync preferences to sync the same database in a single Instance.

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackedDatabase {
    /// Database ID being tracked
    pub database_id: ID,

    /// Which user key to use for this database
    pub key_id: PublicKey,

    /// Sync preferences for this database
    pub sync_settings: SyncSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

**Design Notes**:

- **SigKey Discovery**: When tracking a database via `track_database()`, the system automatically discovers which SigKey the user can use via `Database::find_sigkeys()`, selecting the highest-permission SigKey available. The discovered SigKey is stored in `UserKey.database_sigkeys` HashMap, where `None` represents the default pubkey identity and `Some(sigkey)` represents non-default identities (global, name-based, delegation).

- **Separation of Concerns**: The `key_id` in TrackedDatabase references the user's key, while the actual SigKey mapping is stored in `UserKey.database_sigkeys`. This allows the same key to use different SigKeys in different databases.

- **Sync Settings vs Sync Status**: User settings indicate what the user wants (sync_enabled, sync_on_commit), while the Sync module tracks actual sync status (last_synced, connection state). Multiple users can have different settings for the same database.

#### 5. DatabaseTracking (stored in `_databases` table)

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatabaseTracking {
    /// Database ID (this is the key in the table)
    pub database_id: ID,

    /// Cached database name (for quick lookup)
    pub name: Option<String>,

    /// Users who have this database in their preferences
    pub users: Vec<String>,

    /// Database creation time (Unix timestamp)
    pub created_at: i64,

    /// Last modification time (Unix timestamp)
    pub last_modified: i64,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}
```

### System Databases

The Instance manages four separate system databases, all authenticated with `device_key`:

#### `_instance` System Database

- **Type**: Separate database
- **Purpose**: Instance configuration and management
- **Structure**: Configuration settings, metadata, system policies
- **Authentication**: `device_key` as Admin; admin users can be granted access
- **Access**: Admin users have Admin permission, regular users have Read permission
- **Created**: On Instance initialization

#### `_users` System Database

- **Type**: Separate database
- **Purpose**: User directory and authentication
- **Structure**: Table with UUID primary keys, stores UserInfo (username field for login lookups)
- **Authentication**: `device_key` as Admin
- **Access**: Admin users can manage users
- **Created**: On Instance initialization
- **Note**: Username uniqueness enforced at application layer via search; see Race Conditions section

#### `_databases` System Database

- **Type**: Separate database
- **Purpose**: Instance-wide database registry and optimization
- **Structure**: Table mapping database_id → DatabaseTracking
- **Authentication**: `device_key` as Admin
- **Maintenance**: Updated when users add/remove databases from preferences
- **Benefits**: Fast discovery of databases, see which users care about each DB
- **Created**: On Instance initialization

#### `_sync` System Database

- **Type**: Separate database (existing)
- **Purpose**: Synchronization configuration and bootstrap request management
- **Structure**: Various subtrees for sync settings, peer info, bootstrap requests
- **Authentication**: `device_key` as Admin
- **Access**: Managed by Instance and Sync module
- **Created**: When sync is enabled via `Instance::enable_sync()`

### Instance Identity vs User Management

The Instance identity is separate from user management:

#### Instance Identity

The Instance uses `device_key` for its identity:

- **Storage**: Stored in `InstanceMetadata` (persisted in backend alongside system database IDs)
- **Purpose**: Instance sync identity and system database authentication
- **Access**: Available to Instance on startup (no password required)
- **Usage**: Used to authenticate to all system databases as Admin
- **Persistence**: The `InstanceMetadata` also stores system database IDs for O(1) lookup on startup

#### User Management

Users are created by administrators or self-registration:

```rust
/// Users authenticate with passwords
/// Each has isolated key storage and preferences
/// Must login to perform operations
```

**User Lifecycle:**

1. Created via `Instance::create_user()` by an admin
2. User logs in via `Instance::login_user()`
3. User session provides access to keys and preferences
4. User logs out via `User::logout()`

### Library Architecture Layers

The library separates infrastructure (Instance) from contextual operations (User):

#### Instance Layer: Infrastructure Management

Instance manages the multi-user infrastructure and system resources:

**Initialization:**

1. Check for existing `InstanceMetadata` in backend
2. If new: generate `device_key`, create system databases, save `InstanceMetadata`
3. If existing: load `InstanceMetadata` with device key and system database IDs

**Responsibilities:**

- User account management (create, login)
- System database maintenance
- Backend coordination
- Database tracking

**Key Points:**

- Instance is always multi-user underneath
- No direct database or key operations
- All operations require a User session

#### User Layer: Contextual Operations

User represents an authenticated session with decrypted keys:

**Creation:**

- Returned from `Instance::login_user(username, Option<password>)`
- Contains decrypted private keys in memory
- Has access to user's preferences and database mappings

**Responsibilities:**

- Database operations (create_database, open_database, find_database)
- Key management (add_private_key, list_keys, get_signing_key)
- Database preferences
- Bootstrap approval

**Key Points:**

- All database creation and key management happens through User
- Keys are zeroized on logout or drop
- Clean separation between users

#### Passwordless Users

For embedded/single-user scenarios, users can be created without passwords:

**Creation:**

```rust,ignore
// Create passwordless user
instance.create_user("alice", None)?;

// Login without password
let user = instance.login_user("alice", None)?;

// Use User API normally
let db = user.new_database(settings)?;
```

**Characteristics:**

- No authentication overhead
- Keys stored unencrypted in user database
- Perfect for embedded apps, CLI tools, single-user deployments
- Still uses full User API for operations

#### Password-Protected Users

For multi-user scenarios, users have password-based authentication:

**Creation:**

```rust,ignore
// Create password-protected user
instance.create_user("bob", Some("password123"))?;

// Login with password verification
let user = instance.login_user("bob", Some("password123"))?;

// Use User API normally
let db = user.new_database(settings)?;
```

**Characteristics:**

- Argon2id password hashing
- AES-256-GCM key encryption
- Perfect for servers, multi-tenant applications
- Clear separation between users

### Instance API

Instance manages infrastructure and user accounts:

#### Initialization

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Instance {
    /// Create or open an instance
    /// - Loads InstanceMetadata if exists, otherwise initializes new instance
    /// - InstanceMetadata contains device_key and system database IDs
    pub fn open(backend: Box<dyn BackendImpl>) -> Result<Self>;
}
```

#### User Management

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Instance {
    /// Create a new user account
    /// Returns user_uuid (the generated primary key)
    pub fn create_user(
        &self,
        username: &str,
        password: Option<&str>,
    ) -> Result<String>;

    /// Login a user (returns User session object)
    /// Searches by username; errors if duplicate usernames detected
    pub fn login_user(
        &self,
        username: &str,
        password: Option<&str>,
    ) -> Result<User>;

    /// List all users (returns usernames)
    pub fn list_users(&self) -> Result<Vec<String>>;
}
```

### User API

<!-- Code block ignored: API interface showing struct and impl with function signatures without bodies -->

```rust,ignore
/// User session object, returned after successful login
///
/// Represents an authenticated user with decrypted private keys loaded in memory.
/// All contextual operations (database creation, key management) happen through User.
pub struct User {
    user_uuid: String,   // Stable internal UUID (Table primary key)
    username: String,    // Username (login identifier)
    user_database: Database,
    instance: WeakInstance,  // Weak reference to Instance for storage access
    /// Decrypted user keys (in memory only during session)
    key_manager: UserKeyManager,
}

impl User {
    /// Get the internal user UUID (stable identifier)
    pub fn user_uuid(&self) -> &str;

    /// Get the username (login identifier)
    pub fn username(&self) -> &str;

    // === Database Operations ===

    /// Create a new database in this user's context
    pub fn create_database(&self, settings: Doc, key_id: &PublicKey) -> Result<Database>;

    /// Load a database using this user's keys
    pub fn open_database(&self, database_id: &ID) -> Result<Database>;

    /// Find databases by name
    pub fn find_database(&self, name: impl AsRef<str>) -> Result<Vec<Database>>;

    /// Find the best key for accessing a database

    /// Get the resolved SigKey mapping for a key in a specific database
    pub fn key_mapping(
        &self,
        key_id: &PublicKey,
        database_id: &ID,
    ) -> Result<Option<SigKey>>;

    /// Add a SigKey mapping for a key in a specific database
    pub fn map_key(
        &mut self,
        key_id: &PublicKey,
        database_id: &ID,
        sigkey: SigKey,
    ) -> Result<()>;

    // === Tracked Databases ===

    /// List all tracked databases.
    pub fn databases(&self) -> Result<Vec<TrackedDatabase>>;

    /// Get a specific tracked database by ID.
    pub fn database(&self, database_id: &ID) -> Result<TrackedDatabase>;

    /// Track a database with auto-discovery of SigKeys (upsert behavior).
    pub fn track_database(&mut self, database_id: impl Into<ID>, key_id: &PublicKey, sync_settings: SyncSettings) -> Result<()>;

    /// Stop tracking a database.
    pub fn untrack_database(&mut self, database_id: &ID) -> Result<()>;

    // === Key Management ===

    /// Generate a new private key for this user
    pub fn add_private_key(
        &mut self,
        display_name: Option<&str>,
    ) -> Result<PublicKey>;

    /// List all key IDs owned by this user
    pub fn list_keys(&self) -> Result<Vec<PublicKey>>;

    /// Get a signing key by its ID
    pub fn get_signing_key(&self, key_id: &PublicKey) -> Result<SigningKey>;

    // === Session Management ===

    /// Logout (clears decrypted keys from memory)
    pub fn logout(self) -> Result<()>;
}
```

### UserKeyManager (Internal)

<!-- Code block ignored: Missing HashMap and SigningKey imports -->

```rust,ignore
/// Internal key manager that holds decrypted keys during user session
struct UserKeyManager {
    /// Decrypted keys (key_id → PrivateKey)
    decrypted_keys: HashMap<String, PrivateKey>,

    /// Key metadata (loaded from user database)
    key_metadata: HashMap<String, UserKey>,

    /// User's password-derived encryption key (for saving new keys)
    encryption_key: Vec<u8>,
}
```

See [key_management.md](./key_management.md) for detailed implementation.

## User Flows

### User Creation Flow

**Password-Protected User:**

1. Admin calls `instance.create_user(username, Some(password))`
2. System searches `_users` Table for existing username (race condition possible)
3. System hashes password with Argon2id and random salt
4. Generates default Ed25519 keypair for the user (kept in memory only)
5. Uses instance `device_key` from InstanceMetadata
6. Creates user database with authentication for both `device_key` (Admin) and user's key (Admin)
7. Encrypts user's private key with password-derived key (AES-256-GCM)
8. Stores encrypted key in user database `keys` Table (using public key as identifier, signed with `device_key`)
9. Creates UserInfo and inserts into `_users` Table (auto-generates UUID primary key)
10. Returns user_uuid

**Passwordless User:**

1. Admin calls `instance.create_user(username, None)`
2. System searches `_users` Table for existing username (race condition possible)
3. Generates default Ed25519 keypair for the user (kept in memory only)
4. Uses instance `device_key` from InstanceMetadata
5. Creates user database with authentication for both `device_key` (Admin) and user's key (Admin)
6. Stores unencrypted private key in user database `keys` Table (marked as Unencrypted)
7. Creates UserInfo with None for password fields and inserts into `_users` Table
8. Returns user_uuid

**Note**: For password-protected users, the keypair is never stored unencrypted in the backend. For passwordless users, keys are stored unencrypted for instant access. The user database is authenticated with both the instance `device_key` (for admin operations) and the user's default key (for user ownership). Initial entries are signed with `device_key`.

### Login Flow

**Password-Protected User:**

1. User calls `instance.login_user(username, Some(password))`
2. System searches `_users` Table by username
3. If multiple users with same username found, returns `DuplicateUsersDetected` error
4. Verifies password against stored hash
5. Loads user's private database
6. Loads encrypted keys from user database
7. Derives encryption key from password
8. Decrypts all private keys
9. Creates UserKeyManager with decrypted keys
10. Returns User session object (contains both user_uuid and username)

**Passwordless User:**

1. User calls `instance.login_user(username, None)`
2. System searches `_users` Table by username
3. If multiple users with same username found, returns `DuplicateUsersDetected` error
4. Verifies UserInfo has no password (password_hash and password_salt are None)
5. Loads user's private database
6. Loads unencrypted keys from user database
7. Creates UserKeyManager with keys (no decryption needed)
8. Returns User session object (contains both user_uuid and username)

### Database Creation Flow

1. User obtains User session via login
2. User creates database settings (Doc with name, etc.)
3. Calls `user.new_database(settings)`
4. System selects first available signing key from user's keyring
5. Creates database using `Database::new()` for root entry creation
6. Stores database_sigkeys mapping in UserKey for future loads
7. Returns Database object
8. User can now create transactions and perform operations on the database

### Database Access Flow

The user accesses databases through the `User.open_database()` method, which handles all key management automatically:

1. User calls `user.open_database(&database_id)`
2. System finds appropriate key via `find_key()`
   - Checks user's key metadata for SigKey mappings to this database
   - Verifies keys are authorized in database's auth settings
   - Selects key with highest permission level
3. System retrieves decrypted SigningKey from UserKeyManager
4. System gets SigKey mapping via `key_mapping()`
5. System loads Database with `Database::open()`
   - Database stores `DatabaseKey` with signing key and auth identity
6. User creates transactions normally: `database.new_transaction()`
   - Transaction automatically receives provided key from Database
   - No backend key lookup required
7. User performs operations and commits
   - Transaction uses provided SigningKey directly during commit()

**Key Insight**: Once a Database is loaded via `User.open_database()`, all subsequent operations transparently use the user's keys. The user doesn't need to think about key management - it's handled at database load time.

### Key Addition Flow

**Password-Protected User:**

1. User calls `user.add_private_key(display_name)`
2. System generates new Ed25519 keypair
3. Encrypts private key with user's password-derived key (AES-256-GCM)
4. Creates UserKey metadata with Encrypted variant
5. Stores encrypted key in user database
6. Adds to in-memory UserKeyManager
7. Returns key_id

**Passwordless User:**

1. User calls `user.add_private_key(display_name)`
2. System generates new Ed25519 keypair
3. Creates UserKey metadata with Unencrypted variant
4. Stores unencrypted key in user database
5. Adds to in-memory UserKeyManager
6. Returns key_id

## Bootstrap Integration

The Users system integrates with the bootstrap protocol for access control:

- **User Authentication**: Bootstrap requests approved by logged-in users
- **Permission Checking**: Only users with a key that has Admin permission for the database can approve bootstrap requests
- **Key Discovery**: User's key manager finds appropriate Admin key for database
- **Transaction Creation**: Uses user's Admin key SigKey to add requesting key to database auth

See [bootstrap.md](./bootstrap.md) for detailed bootstrap protocol and wildcard permissions.

## Integration with Key Management

The key management design (see [key_management.md](./key_management.md)) provides the technical implementation details for:

1. **Password-Derived Encryption**: How user passwords are used to derive encryption keys for private key storage
2. **Key Encryption Format**: Specific encryption algorithms and formats used
3. **Database ID → SigKey Mapping**: Technical structure and storage
4. **Key Discovery Algorithms**: How keys are matched to databases and permissions

The Users system provides the architectural context:

- Who owns keys (users)
- How keys are isolated (user databases)
- When keys are decrypted (during user session)
- How keys are managed (User API)

## Security Considerations

### Password Security

1. **Password Hashing**: Use Argon2id for password hashing with appropriate parameters
2. **Random Salts**: Each user has a unique random salt
3. **No Password Storage**: Only hashes stored, never plaintext
4. **Rate Limiting**: Login attempts should be rate-limited

### Key Encryption

1. **Password-Derived Keys**: Use PBKDF2 or Argon2 to derive encryption keys from passwords
2. **Authenticated Encryption**: Use AES-GCM or ChaCha20-Poly1305
3. **Unique Nonces**: Each encrypted key has a unique nonce/IV
4. **Memory Security**: Clear decrypted keys from memory on logout

### User Isolation

1. **Database-Level Isolation**: Each user's private database is separate
2. **Access Control**: Users cannot access other users' databases or keys
3. **Authentication Required**: All user operations require valid session
4. **Session Timeouts**: Consider implementing session expiration

### Instance Identity Protection

1. **Backend Security**: `device_key` stored in `InstanceMetadata` with appropriate file permissions
2. **Limited Exposure**: `device_key` only used for system database authentication
3. **Audit Logging**: Log Instance-level operations on system databases
4. **Key Rotation**: Support rotating `device_key` (requires updating all system databases)

## Known Limitations

### Username Uniqueness Race Condition

**Issue**: Username uniqueness is enforced at the application layer using search-then-insert operations, which creates a race condition in distributed/concurrent scenarios.

**Current Behavior**:

- `create_user()` searches for existing username, then inserts if not found
- Two concurrent creates with same username can both succeed
- Results in multiple UserInfo records with same username but different UUIDs

**Detection**:

- `login_user()` searches by username
- If multiple matches found, returns `UserError::DuplicateUsersDetected`
- Prevents login until conflict is resolved manually

## Performance Implications

1. **Login Cost**: Password hashing and key decryption add latency to login (acceptable)
2. **Memory Usage**: Decrypted keys held in memory during session
3. **Database Tracking**: O(1) lookup for database metadata and user lists (via UUID primary key)
4. **Username Lookup**: O(n) search for username validation/login (where n = total users)
5. **Key Discovery**: O(n) where n = number of user's keys (typically small)

## Implementation Strategy

### Phase 1: Core User Infrastructure

1. Define data structures (UserInfo, UserProfile, UserKey, etc.)
2. Implement password hashing and verification
3. Implement key encryption/decryption
4. Create `_instance` system database
5. Create `_users` system database
6. Create `_databases` tracking table
7. Unit tests for crypto and data structures

### Phase 2: User Management API

1. Implement `Instance::create_user()`
2. Implement `Instance::login_user()`
3. Implement User struct and basic methods
4. Implement UserKeyManager
5. Integration tests for user creation and login

### Phase 3: Key Management Integration

1. Implement `User::add_private_key()`
2. Implement `User::set_database_sigkey()`
3. Implement key discovery methods
4. Update Transaction to work with User sessions
5. Tests for key operations

### Phase 4: Database Preferences

1. Implement database preference storage
2. Implement database tracking updates
3. Implement preference query APIs
4. Tests for preference management

### Phase 5: Migration and Integration

1. Update existing code to work with Users
2. Provide migration utilities for existing instances
3. Update documentation and examples
4. End-to-end integration tests

## Future Work

1. **Multi-Factor Authentication**: Add support for TOTP, hardware keys
2. **User Groups/Roles**: Team collaboration features
3. **Permission Delegation**: Allow users to delegate access to specific databases
4. **Key Recovery**: Secure key recovery mechanisms
5. **Session Management**: Advanced session features (multiple devices, revocation)
6. **Audit Logs**: Comprehensive logging of user operations
7. **User Quotas**: Storage and database limits per user

## Conclusion

The Users system provides a clean separation between infrastructure (Instance) and contextual operations (User):

**Core Architecture:**

- Instance manages infrastructure: user accounts, backend, system databases
- User handles all contextual operations: database creation, key management
- Separate system databases (`_instance`, `_users`, `_databases`, `_sync`)
- Instance identity (`device_key`) stored in `InstanceMetadata` for system database authentication
- Strong isolation between users

**User Types:**

- **Passwordless Users**: Optional password support enables instant login without authentication overhead, perfect for embedded apps
- **Password-Protected Users**: Argon2id password hashing and AES-256-GCM key encryption for multi-user scenarios

**Key Benefits:**

- Clean separation: Instance = infrastructure, User = operations
- All operations run in User context after login
- Flexible authentication: users can have passwords or not
- Instance restart loads `InstanceMetadata` (device key + system database IDs) for O(1) initialization
