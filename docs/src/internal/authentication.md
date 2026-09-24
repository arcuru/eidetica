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

Delegated authorization state is a disposable projection keyed by main-tree Entry ID. Validation joins only the states of locally `Verified` immediate parents and tracks a frontier per delegated root at every signature step (including nested steps). A validated claim replaces the inherited frontier for that root; siblings join at a merge. A cache miss for a `Verified` parent reconstructs its state from its `Verified` ancestry, not an empty floor.

Only a first-hop delegation in the primary tree incorporates its configured `_settings.auth.delegations` pointer. A direct-key Admin can write any valid pointer, including a rewind, but a delegated signature on that same entry must cover its new pointer. The entry's **resulting merged** `_settings` state decides removal: absence clears the first-hop root's frontier; a losing removal does not. Nested roots survive removal/re-addition of the first hop. An Admin choosing this recovery path must account for those nested floors.

Claimed and configured tips require complete locally `Verified` ancestry of the delegated database. Missing and present-but-`Unverified` dependencies are retryable and do not mark the signed entry `Failed`; wrong-tree and proven invalid tips fail. Validation does not recursively verify another tree under the verification lock. Dependency fetching and dependency-first retry are separate sync work; explicit later verification can settle dependencies.

**Upgrade prerequisite:** Before using these authorization rules on a database verified under older code, an operator must explicitly run the separate local verification-reset utility to clear **all** stored verification labels (including `Failed`) and derived caches, then reverify from immutable Entries. There is no automatic migration or version marker here. Skipping the reset may trust legacy `Verified` labels; clearing only the cache is not a safe substitute. The signed Entry/AuthInfo wire format is unchanged.

Historical signature checks require the signed pre-write settings pin to equal the complete canonical `_settings` frontier of the entry's main parents, rather than the current database head. A missing main or pinned ancestor defers verification; a forged or stale complete pin fails. Only the genesis entry can bootstrap against its own settings. The resulting settings projection is reconstructed from complete main-tree ancestry at each entry, not from the signature's pre-write pin or unrelated live tips. Snapshot pinning does not promise live-head freshness or retroactive revocation.

## Conflict Resolution

Auth changes use **Last-Write-Wins** via DAG structure:

- Priority determines who CAN make changes
- LWW determines WHICH change wins
- Historical entries remain valid after permission changes
