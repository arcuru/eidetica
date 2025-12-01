# Authentication

Ed25519-based cryptographic authentication ensuring data integrity and access control.

## Authentication States

| State        | `_settings.auth` | Unsigned Ops | Authenticated Ops |
| ------------ | ---------------- | ------------ | ----------------- |
| **Unsigned** | Missing or `{}`  | ✓ Allowed    | ✓ Bootstrap       |
| **Signed**   | Has keys         | ✗ Rejected   | ✓ Validated       |

### Invalid States (Prevented)

| State         | `_settings.auth` | All Ops    |
| ------------- | ---------------- | ---------- |
| **Corrupted** | Wrong type       | ✗ Rejected |
| **Deleted**   | Tombstone        | ✗ Rejected |

**Corruption Prevention:**

- **Layer 1 (Proactive)**: Transactions that would corrupt or delete auth fail during `commit()`
- **Layer 2 (Reactive)**: If already corrupted, all operations fail with `CorruptedAuthConfiguration`

## Permission Hierarchy

| Permission | Settings | Keys | Write | Read | Priority |
| ---------- | -------- | ---- | ----- | ---- | -------- |
| **Admin**  | ✓        | ✓    | ✓     | ✓    | 0-2^32   |
| **Write**  | ✗        | ✗    | ✓     | ✓    | 0-2^32   |
| **Read**   | ✗        | ✗    | ✗     | ✓    | None     |

Lower priority number = higher privilege. Keys can only modify keys with equal or lower priority.
Only Admin keys can modify the Settings, including the stored Keys.

## Key Types

**Direct Keys**: Ed25519 public keys in `_settings.auth`:

```json
{
  "KEY_LAPTOP": {
    "pubkey": "ed25519:BASE64_PUBLIC_KEY",
    "permissions": "write:10",
    "status": "active"
  }
}
```

**Wildcard Key** (`*`): Details a default Permission for any key. Used for public databases or to avoid authentication.

**Delegated Keys**: Reference another database for authentication:

```json
{
  "user@example.com": {
    "permission-bounds": { "max": "write:15" },
    "database": { "root": "TREE_ID", "tips": ["TIP_ID"] }
  }
}
```

## Delegation

Databases can delegate auth to other databases with permission clamping:

- `max`: Maximum permission (required)
- `min`: Minimum permission (optional)
- Effective = clamp(delegated, min, max)

It is recursively applied, so the remote database can also delegate to other remote databases.

This can be used for building groups containing multiple keys/identities, or managing an individual's device-level keys.

Instead of a separate custom way of users managing and authenticating multiple keys, an individual can use the same authentication scheme as any other database.
Then whenever they need access to a database, the db will authenticate them by granting access to their 'identity' database. This allows granting people/entities access to a database while letting them manage their own keys using all the same facilities as a typical database, including key rotation and revocation.

**Tip tracking** ensures revocations are respected, entries must use equal or newer tips than previously seen.

To keep remote delegated databases up to date, writes update the known tips of the delegated database. This is necessary to ensure that the primary tree sees the latest tips of the delegated tree and knows which keys to allow/block.

## Conflict Resolution

Auth changes use **Last-Write-Wins** via DAG structure:

- Priority determines who CAN make changes
- LWW determines WHICH change wins
- Historical entries remain valid after permission changes
