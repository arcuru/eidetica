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
