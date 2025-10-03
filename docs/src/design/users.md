**Implementation Status**: 🔵 Proposed

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

The system uses separate system databases for different concerns:

```text
Instance
├── Backend Storage (local only, not in databases)
│   └── _device_key (SigningKey for Instance identity)
│
├── System Databases (separate databases, authenticated with _device_key)
│   ├── _instance
│   │   └── Instance configuration and metadata
│   ├── _users (Table with UUID primary keys)
│   │   └── User directory: Maps UUID → UserInfo (username stored in UserInfo)
│   ├── _databases
│   │   └── Database tracking: Maps database_id → DatabaseTracking
│   └── _sync
│       └── Sync configuration and bootstrap requests
│
└── User Databases (one per user)
    ├── Password-protected
    ├── Encrypted private keys
    ├── Database → SigKey mappings
    └── User preferences
```

**Key Architectural Principle**: The library always uses the multi-user architecture underneath, with ergonomic wrappers providing single-user simplicity when needed.

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
    pub password_hash: String,

    /// Salt for password hashing (base64 encoded string)
    pub password_salt: String,

    /// User account creation timestamp
    pub created_at: u64,

    /// Last login timestamp
    pub last_login: Option<u64>,

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
    /// Key identifier (typically the base64-encoded public key string)
    pub key_id: String,

    /// Encrypted private key (encrypted with user's password-derived key)
    pub encrypted_private_key: Vec<u8>,

    /// Encryption nonce/IV
    pub nonce: Vec<u8>,

    /// Display name for this key
    pub display_name: Option<String>,

    /// When this key was created
    pub created_at: u64,

    /// Last time this key was used
    pub last_used: Option<u64>,

    /// Database-specific SigKey mappings
    /// Maps: Database ID → SigKey used in that database's auth settings
    pub database_sigkeys: HashMap<ID, String>,
}
```

#### 4. UserDatabasePreferences (stored in user's private database `databases` subtree)

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserDatabasePreferences {
    /// Database ID
    pub database_id: ID,

    /// Whether user wants to sync this database
    pub sync_enabled: bool,

    /// Sync settings specific to this database
    pub sync_settings: SyncSettings,

    /// User's preferred SigKey for this database
    pub preferred_sigkey: Option<String>,

    /// Custom labels or notes
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncSettings {
    /// Sync interval (if periodic)
    pub interval_seconds: Option<u64>,

    /// Sync on commit
    pub sync_on_commit: bool,

    /// Additional sync configuration
    pub properties: HashMap<String, String>,
}
```

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

    /// Database creation time
    pub created_at: u64,

    /// Last modification time
    pub last_modified: u64,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}
```

### System Databases

The Instance manages four separate system databases, all authenticated with `_device_key`:

#### `_instance` System Database

- **Type**: Separate database
- **Purpose**: Instance configuration and management
- **Structure**: Configuration settings, metadata, system policies
- **Authentication**: `_device_key` as Admin; admin users can be granted access
- **Access**: Admin users have Admin permission, regular users have Read permission
- **Created**: On Instance initialization

#### `_users` System Database

- **Type**: Separate database
- **Purpose**: User directory and authentication
- **Structure**: Table with UUID primary keys, stores UserInfo (username field for login lookups)
- **Authentication**: `_device_key` as Admin
- **Access**: Admin users can manage users
- **Created**: On Instance initialization
- **Note**: Username uniqueness enforced at application layer via search; see Race Conditions section

#### `_databases` System Database

- **Type**: Separate database
- **Purpose**: Instance-wide database registry and optimization
- **Structure**: Table mapping database_id → DatabaseTracking
- **Authentication**: `_device_key` as Admin
- **Maintenance**: Updated when users add/remove databases from preferences
- **Benefits**: Fast discovery of databases, see which users care about each DB
- **Created**: On Instance initialization

#### `_sync` System Database

- **Type**: Separate database (existing)
- **Purpose**: Synchronization configuration and bootstrap request management
- **Structure**: Various subtrees for sync settings, peer info, bootstrap requests
- **Authentication**: `_device_key` as Admin
- **Access**: Managed by Instance and Sync module
- **Created**: When sync is enabled via `Instance::with_sync()`

### Instance Identity vs User Management

The Instance identity is separate from user management:

#### Instance Identity

The Instance uses `_device_key` for its identity:

- **Storage**: Stored in backend (local storage, not in any database)
- **Purpose**: Instance sync identity and system database authentication
- **Access**: Available to Instance on startup (no password required)
- **Usage**: Used to authenticate to all system databases as Admin

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

The library provides a layered architecture with a single implementation underneath and ergonomic wrappers on top:

#### Core Layer: Always Multi-User

All Instance initialization creates the multi-user foundation:

**Initialization Steps:**

1. Load or generate `_device_key` from backend
2. Create system databases (`_instance`, `_users`, `_databases`) authenticated with `_device_key`
3. Initialize InstanceCore with backend and system databases

**Key Points:**

- System databases (`_instance`, `_users`, `_databases`) always exist
- `_device_key` stored in backend provides Instance identity
- All database operations go through user context internally
- This is the foundation for both single-user and multi-user modes

#### Single-User Ergonomics Layer

The simple API for embedded applications provides seamless single-user experience:

**Initialization:**

- `Instance::new()` creates default user automatically with auto-generated password
- Default user auto-logged in and stored as implicit user
- Returns Instance in SingleUser mode

**Operation Pattern:**

- All database and key operations delegate to implicit user
- No explicit user login or session management required
- Errors if no implicit user available

**Characteristics:**

- Perfect for embedded apps, CLI tools, single-user deployments
- No authentication overhead for simple use cases
- Transparent user context in all operations

#### Explicit Multi-User Layer

The full API for multi-user servers and applications:

**Initialization:**

- `Instance::new_multiuser()` has no implicit user
- Returns Instance in MultiUser mode

**User Management:**

- Explicit `create_user()` for user account creation
- Explicit `login_user()` returns User session object
- All operations performed through User object

**Characteristics:**

- Full control over user sessions and lifecycle
- Password-based authentication required
- Perfect for servers, multi-tenant applications
- Clear separation between users

#### Binary Usage

The binary (multi-user sync server) builds on the explicit multi-user API:

<!-- Code block ignored: Missing Instance type and backend definition -->

```rust,ignore
// bin/main.rs
let instance = Instance::new_multiuser(backend)?;
let sync = instance.with_sync()?;

// Expose HTTP/gRPC endpoints for:
// - User creation/authentication
// - Database operations per-user
// - Sync protocol handling
```

The binary is simply the library in explicit multi-user mode with network transport layers.

### Instance API

The Instance API has two modes: single-user (ergonomic) and multi-user (explicit).

#### Initialization

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Instance {
    // === Single-User Mode ===

    /// Create instance with implicit default user (simple API)
    /// - Loads/generates _device_key from backend
    /// - Creates system databases (_instance, _users, _databases)
    /// - Creates and auto-logs in default user
    /// - All operations use implicit user context
    pub fn new(backend: Box<dyn BackendDB>) -> Result<Self>;

    // === Multi-User Mode ===

    /// Create instance without implicit user (explicit API)
    /// - Loads/generates _device_key from backend
    /// - Creates system databases (_instance, _users, _databases)
    /// - Requires explicit user login for operations
    pub fn new_multiuser(backend: Box<dyn BackendDB>) -> Result<Self>;
}
```

#### Single-User Convenience Methods

These methods work with the implicit default user (only available in single-user mode):

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Instance {
    // === Database Operations (single-user convenience) ===

    /// Create database using implicit user
    pub fn new_database(&self, settings: Doc) -> Result<Database>;

    /// Load database (available in both modes)
    pub fn load_database(&self, root_id: &ID) -> Result<Database>;

    // === Key Management (single-user convenience) ===

    /// Add private key to implicit user's keyring
    pub fn add_private_key(&self, display_name: Option<&str>) -> Result<String>;

    /// Get implicit user's keys for a database
    pub fn get_keys_for_database(
        &self,
        database_id: &ID,
    ) -> Result<Vec<(String, Permission)>>;

    // === Database Preferences (single-user convenience) ===

    /// Add database to implicit user's preferences
    pub fn add_database_preference(
        &self,
        database_id: &ID,
        preferences: UserDatabasePreferences,
    ) -> Result<()>;

    /// List implicit user's database preferences
    pub fn list_database_preferences(&self) -> Result<Vec<UserDatabasePreferences>>;
}
```

#### Explicit User Management

These methods are available in both modes:

<!-- Code block ignored: API interface showing function signatures without bodies -->

```rust,ignore
impl Instance {
    // === User Management ===

    /// Create a new user account
    /// Returns (user_uuid, user_info) where user_uuid is the generated primary key
    pub fn create_user(
        &self,
        username: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<(String, UserInfo)>;

    /// Login a user with password (returns User session object)
    /// Searches by username; errors if duplicate usernames detected
    pub fn login_user(
        &self,
        username: &str,
        password: &str,
    ) -> Result<User>;

    /// List all users (returns usernames)
    pub fn list_users(&self) -> Result<Vec<String>>;

    /// Disable a user account
    pub fn disable_user(&self, username: &str) -> Result<()>;

    // === Database Tracking ===

    /// Register a database in the tracking table
    pub fn register_database(
        &self,
        database_id: &ID,
        name: Option<&str>,
    ) -> Result<()>;

    /// Get tracking info for a database
    pub fn get_database_tracking(&self, database_id: &ID) -> Result<Option<DatabaseTracking>>;

    /// List all tracked databases
    pub fn list_tracked_databases(&self) -> Result<Vec<DatabaseTracking>>;

    /// Update database tracking (add user, update metadata)
    pub fn update_database_tracking(
        &self,
        database_id: &ID,
        update: DatabaseTrackingUpdate,
    ) -> Result<()>;
}

pub enum DatabaseTrackingUpdate {
    AddUser(String),
    RemoveUser(String),
    UpdateMetadata(HashMap<String, String>),
}
```

### User API

<!-- Code block ignored: API interface showing struct and impl with function signatures without bodies -->

```rust,ignore
/// User session object, returned after successful login
pub struct User {
    user_uuid: String,   // Stable internal UUID (Table primary key)
    username: String,    // Username (login identifier)
    user_database: Database,
    backend: Arc<dyn BackendDB>,
    /// Decrypted user keys (in memory only during session)
    key_manager: UserKeyManager,
}

impl User {
    /// Get the internal user UUID (stable identifier)
    pub fn user_uuid(&self) -> &str;

    /// Get the username (login identifier)
    pub fn username(&self) -> &str;

    // === Key Management ===

    /// Generate a new private key for this user
    pub fn add_private_key(
        &self,
        display_name: Option<&str>,
    ) -> Result<String>;

    /// Import an existing private key
    pub fn import_private_key(
        &self,
        private_key: SigningKey,
        display_name: Option<&str>,
    ) -> Result<String>;

    /// Get public key for a stored key
    pub fn get_public_key(&self, key_id: &str) -> Result<Option<VerifyingKey>>;

    /// List all key IDs owned by this user
    pub fn list_keys(&self) -> Result<Vec<String>>;

    /// Set the SigKey that a key uses in a specific database
    pub fn set_database_sigkey(
        &self,
        key_id: &str,
        database_id: &ID,
        sigkey: &str,
    ) -> Result<()>;

    /// Get the SigKey that a key uses in a specific database
    pub fn get_database_sigkey(
        &self,
        key_id: &str,
        database_id: &ID,
    ) -> Result<Option<String>>;

    /// Remove a key from user's keyring
    pub fn remove_key(&self, key_id: &str) -> Result<()>;

    // === Database Preferences ===

    /// Add a database to user's preferences
    pub fn add_database_preference(
        &self,
        database_id: &ID,
        preferences: UserDatabasePreferences,
    ) -> Result<()>;

    /// Update database preferences
    pub fn update_database_preference(
        &self,
        database_id: &ID,
        update: DatabasePreferenceUpdate,
    ) -> Result<()>;

    /// Get preferences for a database
    pub fn get_database_preference(
        &self,
        database_id: &ID,
    ) -> Result<Option<UserDatabasePreferences>>;

    /// List all databases this user cares about
    pub fn list_database_preferences(&self) -> Result<Vec<UserDatabasePreferences>>;

    /// Remove database from preferences
    pub fn remove_database_preference(&self, database_id: &ID) -> Result<()>;

    // === Key Discovery ===

    /// Find keys this user has that can access a database
    /// Optionally filtered by minimum permission
    pub fn get_keys_for_database(
        &self,
        database_id: &ID,
        min_permission: Option<Permission>,
    ) -> Result<Vec<(String, Permission)>>;

    /// Find the best key for accessing a database
    pub fn find_key_for_database(
        &self,
        database_id: &ID,
    ) -> Result<Option<String>>;

    // === Bootstrap Management ===

    /// Approve a bootstrap request using this user's admin key
    ///
    /// This method:
    /// 1. Finds one of this user's keys with Admin permission for the target database
    /// 2. Creates a transaction using that key's SigKey
    /// 3. Adds the requesting key to the database's auth settings
    /// 4. Updates the bootstrap request status to Approved
    ///
    /// Requires: User must have Admin permission for the target database
    pub fn approve_bootstrap_request(
        &self,
        request_id: &str,
        tree_id: &ID,
    ) -> Result<()>;

    /// Reject a bootstrap request
    ///
    /// Updates the bootstrap request status to Rejected.
    /// No keys are added to the database.
    pub fn reject_bootstrap_request(
        &self,
        request_id: &str,
    ) -> Result<()>;

    // === Session Management ===

    /// Get user ID
    pub fn user_id(&self) -> &str;

    /// Logout (clears decrypted keys from memory)
    pub fn logout(self) -> Result<()>;

    /// Change user password (re-encrypts all keys)
    pub fn change_password(
        &mut self,
        old_password: &str,
        new_password: &str,
    ) -> Result<()>;
}

pub enum DatabasePreferenceUpdate {
    EnableSync(bool),
    SetSyncSettings(SyncSettings),
    SetPreferredSigKey(String),
    UpdateNotes(String),
}
```

### UserKeyManager (Internal)

<!-- Code block ignored: Missing HashMap and SigningKey imports -->

```rust,ignore
/// Internal key manager that holds decrypted keys during user session
struct UserKeyManager {
    /// Decrypted keys (key_id → SigningKey)
    decrypted_keys: HashMap<String, SigningKey>,

    /// Key metadata (loaded from user database)
    key_metadata: HashMap<String, UserKey>,

    /// User's password-derived encryption key (for saving new keys)
    encryption_key: Vec<u8>,
}
```

See [key_management.md](./key_management.md) for detailed implementation.

## User Flows

### User Creation Flow

1. Admin calls `instance.create_user()` with username and password
2. System searches `_users` Table for existing username (race condition possible)
3. System hashes password with Argon2id and random salt
4. Generates default Ed25519 keypair for the user (kept in memory only)
5. Retrieves instance `_device_key` public key from backend
6. Creates user database with authentication for both `_device_key` (Admin) and user's key (Admin)
7. Encrypts user's private key with password-derived key
8. Stores encrypted key in user database `keys` Table (using public key as identifier, signed with `_device_key`)
9. Creates UserInfo and inserts into `_users` Table (auto-generates UUID primary key)
10. Returns (user_uuid, UserInfo) tuple

**Note**: The user's keypair is never stored unencrypted in the backend. It exists only as encrypted data in the user's private database. The user database is authenticated with both the instance `_device_key` (for admin operations) and the user's default key (for user ownership). Initial entries are signed with `_device_key`.

### Login Flow

1. User calls `instance.login_user()` with username and credentials
2. System searches `_users` Table by username
3. If multiple users with same username found, returns `DuplicateUsersDetected` error
4. Verifies password against stored hash
5. Loads user's private database
6. Loads encrypted keys from user database
7. Derives encryption key from password
8. Decrypts all private keys
9. Creates UserKeyManager with decrypted keys
10. Updates last_login timestamp in `_users` Table (using UUID)
11. Returns User session object (contains both user_uuid and username)

### Database Access Flow

1. User identifies target database
2. Calls `user.find_key_for_database()` to get appropriate key
3. Retrieves SigKey mapping for that key in target database
4. Loads database and creates transaction with SigKey
5. Performs operations
6. Commits transaction

### Key Addition Flow

1. User calls `user.add_private_key()` with optional display name
2. System generates new Ed25519 keypair
3. Encrypts private key with user's password-derived key
4. Creates UserKey metadata
5. Stores encrypted key in user database
6. Adds to in-memory UserKeyManager
7. User maps key to database with `user.set_database_sigkey()`
8. Adds key to database auth settings via transaction

### Database Preference Management

1. User creates UserDatabasePreferences with sync settings and preferences
2. Calls `user.add_database_preference()` to store preferences
3. Instance updates database tracking to add user to database's user list
4. User can query preferences with `user.list_database_preferences()`

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

1. **Backend Security**: `_device_key` stored in backend with appropriate file permissions
2. **Limited Exposure**: `_device_key` only used for system database authentication
3. **Audit Logging**: Log Instance-level operations on system databases
4. **Key Rotation**: Support rotating `_device_key` (requires updating all system databases)

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

The Users system provides a unified architecture that supports both embedded applications and multi-user servers:

**Core Architecture:**

- Separate system databases (`_instance`, `_users`, `_databases`, `_sync`)
- Instance identity (`_device_key`) stored in backend, not password-protected
- Strong isolation between users
- Password-based authentication with encrypted key storage
- Per-user key management and database preferences

**Layered API:**

- **Single-User Mode**: Simple `Instance::new()` with implicit default user, perfect for embedded apps
- **Multi-User Mode**: Explicit `Instance::new_multiuser()` requiring user login, perfect for servers
- **Binary**: Builds on multi-user mode with network transport layers

**Key Benefits:**

- One implementation underneath (always multi-user)
- Ergonomic wrappers for different use cases
- No migration burden (new architecture only)
- Clean separation between library (core + APIs) and binary (network layer)
- Instance restart just loads `_device_key` from backend
