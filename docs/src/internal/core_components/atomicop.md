# AtomicOp

Atomic transaction mechanism for tree modifications.

## Lifecycle

1. **Creation**: Initialize with current tree tips as parents
2. **Subtree Access**: Get typed handles for data manipulation
3. **Staging**: Accumulate changes in internal entry
4. **Commit**: Sign, validate, and store finalized entry

## Features

- Multiple subtree changes in single commit
- Automatic authentication using tree's default key
- Type-safe subtree access
- Cryptographic signing and validation

## Integration

**Entry Management**: Creates and manages entries via EntryBuilder

**Authentication**: Signs operations and validates permissions

**CRDT Support**: Enables subtree conflict resolution

**Backend Storage**: Stores entries with verification status
