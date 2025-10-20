# Transaction

Atomic transaction mechanism for database modifications.

## Lifecycle

1. **Creation**: Initialize with current database tips as parents
2. **Store Access**: Get typed handles for data manipulation
3. **Staging**: Accumulate changes in internal entry
4. **Commit**: Sign, validate, and store finalized entry

## Features

- Multiple store changes in single commit
- Automatic authentication using database's default key
- Type-safe store access
- Cryptographic signing and validation

## Integration

**Entry Management**: Creates and manages entries via EntryBuilder

**Authentication**: Signs operations and validates permissions

**CRDT Support**: Enables store conflict resolution

**Backend Storage**: Stores entries with verification status

## Authentication Validation

Transaction commit includes comprehensive authentication validation that distinguishes between valid auth states and corrupted configurations.

### Validation Process

During `commit()` (transaction/mod.rs ~line 938-960), the system validates authentication configuration:

1. **Extract effective settings**: Get `_settings` state at commit time
2. **Check for tombstone**: Use `is_tombstone("auth")` to detect deleted auth
3. **Retrieve auth value**: Use `get("auth")` to get configuration
4. **Validate type**: Ensure auth is Doc type (if present)
5. **Parse auth settings**: Convert Doc to AuthSettings
6. **Validate operation**: Check signature and permissions

### Error Types

Defined in `transaction/errors.rs`:

- **`AuthenticationRequired`**: Unsigned op attempted in signed mode
- **`NoAuthConfiguration`**: Auth lookup failed in signed mode
- **`CorruptedAuthConfiguration`**: Auth has wrong type or is deleted
- **`SigningKeyNotFound`**: Requested signing key doesn't exist
- **`InsufficientPermissions`**: Key lacks required permissions

All are classified as authentication errors via `is_authentication_error()`.
