# Accounts and Account Sessions

An **Account** is a private root database for a person, group, or application.
Its stable identity is its database root ID, not its display name or an instance's login record UUID.
`AccountSession::account()` returns the typed `Account`, and `Account::id()` returns that root ID.
The backing database is encapsulated: `Account` does not re-export the `Database` API, and applications do not read or write its stores directly.
Routine discovery goes through the session's reference operations below.

An **AccountSession** is an authenticated, unlocked handle on one Instance, with decrypted keys and the existing database access, key management, sharing, and sync-preference operations.
`Instance::login_account` returns a session; `NewAccount` supplies initial account creation settings, and `InstanceAdmin::create_account` creates subsequent accounts.
`AccountSession::login_id()` identifies the instance-local login record, while `username()` is its local login name.
`AccountInfo` is that registration record, not the portable Account itself.
A person, group, or app can have an Account; it is not necessarily one account per process, device, or conversation.

**Identity** retains its separate meaning: a database declaring keys and delegation identity, potentially readable by other parties under its Auth policy.
An Account and an Identity need not have a one-to-one relationship.
Neither an organizational link nor Account membership adds a new Role or grants access to another database.

## Current custody and security boundary

This API slice does **not** relocate private keys or establish complete portable-account/scoped-key isolation.
The existing internal layout remains: `_users` contains login records and root credentials; the Account root contains `keys`, `databases`, and `settings` bookkeeping.
The historical serialized field names and trusted-login message encoding are retained; the Rust API has no deprecated User aliases.
No data rewrite, credential deletion, or live rollout is performed by this rename.

Password-protected credentials use the existing Argon2id/AES-256-GCM path; passwordless credentials retain the existing unencrypted storage behavior.
Client-side unlock, proof of possession, per-tree service authorization, signer selection, and embedded/service boundaries are unchanged.
The trusted Unix socket login still assumes filesystem-trusted callers; it is not a network password protocol.
See [Service Architecture](../internal/service.md) and [Key Management](key_management.md).
Moving secrets to instance-local custody, joining an Account on another instance with distinct keys, and linked-account management are separate work.
Do not infer those guarantees from the Account name.

## Application-owned root references

Applications store direct database IDs in their Account under a namespace and name they choose, through the session:

- `AccountSession::set_reference(namespace, name, id)` writes a reference.
- `AccountSession::reference(namespace, name)` reads it as `Option<ID>`.
- `AccountSession::open_reference(namespace, name)` reads it and opens the target with `open_database`.

The namespace separates applications that share one Account; the name is an application alias.
Reference names must be nonempty and contain no dots; invalid names are rejected before reading or writing.
Each namespace is kept in its own store inside the Account, apart from the Account's key and tracking stores, so a namespace cannot overwrite them.
There is no mandatory account-wide catalog, alias index, tags schema, or recursive activation.
Reading an ID does not download, activate, or authorize the referenced database.
Opening goes through the existing key and Auth path, exactly as `AccountSession::open_database(id)` does.
The retained `databases` tracking store still supports key/sync bookkeeping; it is not the required discovery interface.
`find_database` remains an optional linear display-name search, not the root-reference path.

```rust,no_run
use eidetica::{AccountSession, Database, ID, Result};

async fn remember_group(session: &AccountSession, group: &ID) -> Result<()> {
    session.set_reference("my_app", "group", group).await
}

async fn open_group(session: &AccountSession) -> Result<Option<Database>> {
    session.open_reference("my_app", "group").await
}
```

The application chooses `my_app` and `group`; the library reserves neither.
A display-name change on the target does not change its ID or break this reference.
Lookup reads only the selected reference and opens the selected target, without opening unrelated tracked databases.
Existing key selection may still search the session's keys; this is not a claim that every operation is constant time.

### Absence and failures

`reference` and `open_reference` return `Ok(None)` only when the Account has no reference under that namespace and name in the current snapshot.
Unavailable Account data, malformed reference data, transport failure, and denied access are errors, distinct from absence.
A present reference whose target cannot be opened is still a present reference.
An unheld key, pending bootstrap, missing target data, or failed authorization must not trigger creation of a replacement.
`open_reference` reports a present reference whose target cannot be opened as an error, never as `None`.
Do not use a broad `is_not_found()` guard around the result to decide whether to create a target.

Known missing root metadata is also **not** permission to create: initialization is an explicit application operation.
No getter performs backfill, scans names, creates databases, or rewrites references.
A partial initialization that created a target but failed to publish its reference requires explicit repair with that ID; it must not silently create another target on retry.

### Initialization and concurrency

Ordinary CRDT stores do not provide distributed compare-and-set or create-if-absent uniqueness.
Two initializers can both observe no row and create competing targets; writing one row afterward does not undo the other creation.
Applications must serialize initialization through one designated writer or an existing exclusive setup mechanism.
If they cannot establish that precondition, they must refuse automatic initialization and request explicit provisioning/reconciliation rather than claim uniqueness.
This slice adds no lock service or second job engine.

The todo example initializes its reference only when `connect_or_create` creates a new embedded instance, under that backend's exclusive storage ownership.
Returning login opens the `todo`/`list` reference and fails on missing or broken metadata rather than creating another database.
This is not a distributed initialization recipe or a migration for older example files.
Concurrent local login creation retains the existing behavior: duplicate usernames are detected by login and reported as `AccountError::DuplicateAccountsDetected`, not silently selected.

### Portable group versus device-local peer selection

An app's shared group/root database ID belongs in its Account's application metadata.
A peer database selected for one installation is an operational choice, not a universally selected peer for every installation of that Account.
Consumers should keep that selection in their own instance-local configuration, or explicitly key it by a stable installation identity; they must not interpret a single portable `peer` field as the local choice everywhere.
A missing local peer selection requires explicit initialization under the same serialization rule.
This library slice neither defines a generic peer catalog nor changes the existing key/sync storage layout.

## Sharing and bootstrap remain existing mechanisms

AccountSession sharing and database-access requests continue using ordinary Auth and the existing bootstrap flow.
`AccountSession::join()` joins a database from a ticket and returns it opened, using embedded sync or daemon networking while the signing key stays client-owned.
Pending approval requires an explicit retry and writes no new Account mappings or preferences; rejection is terminal for that request.
This is basic database join, not joining an existing Account on a second instance or enrolling a daemon device.
Held/unreadable recovery, automatic delegated-history acquisition, and restart-safe completion remain outside this API; see [current recovery limits](../user_guide/bootstrap.md#current-recovery-limits).
Successful network transfer is distinct from local key-mapping/tracking persistence.
If local bookkeeping fails after a grant, the error remains visible and retries use the existing flow; changing names does not implement a new join engine.
Explicit signer selection and per-database mappings remain necessary.
See [Bootstrapping](bootstrap.md) for request, transfer, approval, and retry behavior.

### Mapping persistence and retry safety

Key mappings and tracked-database preferences are written together in the Account
database. Staging a mapping does not update the session's key-manager cache: cache
publication follows commit acknowledgement or a read-back of Verified persisted
metadata. A failed or lost acknowledgement is not rollback; read-back can recover
an already committed mapping while the operation still reports its error.

Tracking rediscovers the valid SigKey even for an unchanged key, so a legacy or
provisional identity is not retained merely because a row exists. Equivalent
mapping and preference rows do not append identical configuration history.
A Pending, Rejected, or failed network result does not start Account bookkeeping or
delete existing legacy rows; successful access must precede bookkeeping.
An error during the later bookkeeping step can still follow persistence.
See the [bootstrap lifecycle](../internal/bootstrap.md#request-status) for the
separate sync-side approval state.

`request_database_access()` and `join()` observe the tracked row before their
network wait and preserve a preference change observed when bookkeeping resumes. Mapping repair
can still proceed without overwriting that changed row. Explicit `track_database()`
and the split `record_database_access()` form instead treat supplied preferences
as current intent. Split callers can use `record_database_access_if_unchanged()`
with the row observed before their network wait to preserve an intervening change.
This is an observed-state check, not a guarantee against every concurrent or
distributed preference update.
