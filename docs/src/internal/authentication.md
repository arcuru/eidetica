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

Delegated snapshot floors are causal and keyed by delegated database root. A delegated signature must ancestry-cover the snapshots inherited through all parent paths for that root, even if direct-key entries, signer changes, or another delegated identity intervene. Siblings can name different snapshots; a merge descendant must cover every inherited sibling snapshot.

The `tips` on a `DelegatedTreeRef` are a separately committed floor. A claimed snapshot must cover it, and `_settings` writes can only move that pointer forward. At a merge, the new pointer must cover the committed pointers inherited from all parents.

Delegated authentication depends on having the history needed to reconstruct each claimed snapshot and inherited floor. If that proof is incomplete, the signed entry remains `Unverified` and outside the verified frontier rather than failing permanently. Validation reports the delegated database root and first known missing entries so sync can satisfy that dependency and retry verification. Proven invalid signatures, wrong-tree claims, and regressions still fail.

These checks pin historical snapshots; they do not claim a snapshot is a live head or make later authority reduction retroactive. Automatic dependency tracking and recursive fetching are not implemented yet. A future implementation can replicate delegated databases as ordinary databases that remain available to peers and may also be tracked directly for local edits.

## Conflict Resolution

Auth changes use **Last-Write-Wins** via DAG structure:

- Priority determines who CAN make changes
- LWW determines WHICH change wins
- Historical entries remain valid after permission changes
