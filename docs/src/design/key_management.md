> ✅ **Status: Implemented**
>
> This design is fully implemented and functional.

# Key Management Technical Details

This design document describes the technical implementation of key storage, encryption, and discovery within the Eidetica Users system. For the overall architecture and user-centric key management, see [users.md](./users.md).

## Overview

Keys in Eidetica are managed at the user level. Each user owns a set of private keys that are:

- Encrypted with the user's password
- Stored in the user's private database
- Mapped to specific SigKeys in different databases
- Decrypted only during active user sessions

## Problem Statement

Key management requires solving several technical challenges:

1. **Secure Storage**: Private keys must be encrypted at rest
2. **Password-Derived Encryption**: Encryption keys derived from user passwords
3. **SigKey Mapping**: Same key can be known by different SigKeys in different databases
4. **Key Discovery**: Finding which key to use for a given database operation
5. **Memory Security**: Clearing sensitive data after use

## Technical Components

### Password-Derived Key Encryption

**Algorithm**: Argon2id for key derivation, AES-256-GCM for encryption

**Argon2id Parameters:**

- Memory cost: 64 MiB minimum
- Time cost: 3 iterations minimum
- Parallelism: 4 threads
- Output: 32 bytes for AES-256

**Encryption Process:**

1. Derive 256-bit encryption key from password using Argon2id
2. Generate random 12-byte nonce for AES-GCM
3. Serialize private key to bytes
4. Encrypt with AES-256-GCM
5. Store ciphertext and nonce

**Decryption Process:**

1. Derive encryption key from password (same parameters)
2. Decrypt ciphertext using nonce and encryption key
3. Deserialize bytes back to SigningKey

### Key Storage Format

Keys are stored in the user's private database in the `keys` subtree as a Table:

<!-- Code block ignored: Missing Serialize/Deserialize imports from serde -->

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserKey {
    /// Local key identifier (public key string or hardcoded name)
    /// Examples: "ed25519:ABC123..." or "_device_key"
    pub key_id: String,

    /// Encrypted private key bytes (encrypted with user password-derived key)
    pub encrypted_private_key: Vec<u8>,

    /// Nonce/IV used for encryption (12 bytes for AES-GCM)
    pub nonce: Vec<u8>,

    /// Display name for UI/logging
    pub display_name: Option<String>,

    /// Unix timestamp when key was created
    pub created_at: u64,

    /// Unix timestamp when key was last used for signing
    pub last_used: Option<u64>,

    /// Database-specific SigKey mappings
    /// Maps: Database ID → SigKey string
    pub database_sigkeys: HashMap<ID, String>,
}
```

**Storage Location**: User database → `keys` subtree → Table<UserKey>

**Table Key**: The `key_id` field (not stored in struct, used as table key)

### SigKey Mapping

A key can be known by different SigKeys in different databases:

```text
Local Key: "ed25519:ABC123..."
├── Database A: SigKey "alice"
├── Database B: SigKey "admin"
└── Database C: SigKey "alice_laptop"
```

**Mapping Storage**: The `database_sigkeys` HashMap in `UserKey` stores these mappings as `database_id → sigkey_string`.

**Lookup**: When creating a transaction, retrieve the appropriate SigKey from the mapping using the database ID.

### Database Access Index

To efficiently find which keys can access a database, we build a reverse index from database auth settings:

<!-- Code block ignored: Missing HashMap and type imports -->

```rust,ignore
/// Built by reading _settings.auth from database tips
pub struct DatabaseAccessIndex {
    /// Maps: Database ID → Vec<(local_key_id, permission)>
    access_map: HashMap<ID, Vec<(String, Permission)>>,
}
```

**Index Building**: For each database, read its `_settings.auth`, match SigKeys to user keys via the `database_sigkeys` mapping, and store the resulting `(key_id, permission)` pairs.

**Key Lookup**: Query the index by database ID to get all user keys with access, optionally filtered by minimum permission level.

### Key Discovery

Finding the right key for a database operation involves:

1. **Get Available Keys**: Query the DatabaseAccessIndex for keys with access to the database, filtered by minimum permission if needed
2. **Filter to Decrypted Keys**: Ensure we have the private key decrypted in memory
3. **Select Best Key**: Choose the key with highest permission level for the database
4. **Retrieve SigKey**: Get the mapped SigKey from the `database_sigkeys` field for transaction creation

### Memory Security

Decrypted keys are held in memory only during active user sessions:

- **Session-Based**: Keys decrypted on login, held in memory during session
- **Explicit Clearing**: On logout, overwrite key bytes with zeros using the `zeroize` crate
- **Drop Safety**: Implement `Drop` to automatically clear keys when manager is destroyed
- **Encryption Key**: Also clear the password-derived encryption key from memory

## Implementation Details

### UserKeyManager Structure

<!-- Code block ignored: Missing HashMap and SigningKey imports -->

```rust,ignore
pub struct UserKeyManager {
    /// Decrypted private keys (only in memory during session)
    /// Map: key_id → SigningKey
    decrypted_keys: HashMap<String, SigningKey>,

    /// Key metadata (including SigKey mappings)
    /// Map: key_id → UserKey
    key_metadata: HashMap<String, UserKey>,

    /// User's password-derived encryption key
    /// Used for encrypting new keys during session
    encryption_key: Vec<u8>,

    /// Database access index (for key discovery)
    access_index: DatabaseAccessIndex,
}
```

**Creation**: On user login, derive encryption key from password, decrypt all user's private keys, and build the database access index.

**Key Operations**:

- **Add Key**: Encrypt private key with session encryption key, create metadata, store in both maps
- **Get Key**: Retrieve decrypted key by ID, update last_used timestamp
- **Serialize**: Export all key metadata (with encrypted keys) for storage

### Password Change

When a user changes their password, all keys must be re-encrypted:

1. **Verify Old Password**: Authenticate user with current password
2. **Derive New Encryption Key**: Generate new salt, derive key from new password
3. **Re-encrypt All Keys**: Iterate through decrypted keys, encrypt each with new key
4. **Update Password Hash**: Hash new password with new salt
5. **Store Updates**: Write all updated UserKey records and password hash in transaction
6. **Update In-Memory State**: Replace session encryption key with new one

## Security Properties

### Encryption Strength

- **Key Derivation**: Argon2id with 64 MiB memory, 3 iterations
- **Encryption**: AES-256-GCM (authenticated encryption)
- **Key Size**: 256-bit encryption keys
- **Nonce**: Unique 96-bit nonces for each encryption

### Attack Resistance

- **Brute Force**: Argon2id parameters make password cracking expensive
- **Replay Attacks**: Nonces prevent reuse of ciphertexts
- **Tampering**: GCM authentication tag detects modifications
- **Memory Dumps**: Keys cleared from memory on logout

### Limitations

- **Password Strength**: Security depends on user password strength
- **No HSM Support**: Keys stored in software (future enhancement)
- **No Key Recovery**: Lost password means lost keys (by design)

## Performance Considerations

### Login Performance

Password derivation is intentionally slow:

- Argon2id: ~100-200ms per derivation
- Key decryption: ~1ms per key
- Total login time: ~200ms + (num_keys × 1ms)

This is acceptable for login operations.

### Runtime Performance

During active session:

- Key lookups: O(1) from HashMap
- SigKey lookups: O(1) from HashMap
- Database key discovery: O(n) where n = number of keys
- No decryption overhead (keys already decrypted)

## Testing Strategy

1. **Unit Tests**:
   - Password derivation consistency
   - Encryption/decryption round-trips
   - Key serialization/deserialization
   - SigKey mapping operations

2. **Security Tests**:
   - Verify different passwords produce different encrypted keys
   - Verify wrong password fails decryption
   - Verify nonce uniqueness
   - Verify memory clearing

3. **Integration Tests**:
   - Full user session lifecycle
   - Key addition and usage
   - Password change flow
   - Multiple keys with different SigKey mappings

## Future Enhancements

1. **Hardware Security Module Support**: Store keys in HSMs
2. **Key Derivation Tuning**: Adjust Argon2 parameters based on hardware
3. **Key Backup/Recovery**: Secure key recovery mechanisms
4. **Multi-Device Sync**: Sync encrypted keys across devices
5. **Biometric Authentication**: Use biometrics instead of passwords where available

## Conclusion

This key management implementation provides:

- Strong encryption of private keys at rest
- User-controlled key ownership through passwords
- Flexible SigKey mapping for multi-database use
- Efficient key discovery for database operations
- Memory security through session-based decryption

For the overall architecture and user management, see the [Users design](./users.md).
