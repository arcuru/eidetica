# Accounts and Account Sessions

An **Account** is a private root database for a person, group, or application.
Its stable identity is its database root ID, not its display name or an instance's login record UUID.
`AccountSession::account()` returns the typed `Account`, and `Account::id()` returns that root ID.
The backing database is encapsulated: `Account` does not re-export the `Database` API, and applications do not read or write its stores directly.
Routine discovery goes through the session's reference operations below.

An **AccountSession** is an authenticated, unlocked handle on one Instance, with decrypted keys and the existing database access, key management, sharing, and sync-preference operations.
`Instance::login_account` returns a session; `NewAccount` supplies initial account creation settings, and `InstanceAdmin::create_account` creates subsequent accounts.
`AccountSession::login_id()` identifies the instance-local login record, while `username()` is its local login name; both are empty for caller-held sessions, whose `account_info()` is `None`.
`AccountInfo` is that registration record, not the portable Account itself.
A person, group, or app can have an Account; it is not necessarily one account per process, device, or conversation.

**Identity** retains its separate meaning: a database declaring keys and delegation identity, potentially readable by other parties under its Auth policy.
An Account and an Identity need not have a one-to-one relationship.
Neither an organizational link nor Account membership adds a new Role or grants access to another database.

The [explicit credential and signer API](../user_guide/account_credentials.md) supports caller-held keys and proof-only service connections without daemon login storage.
Its [native local-record substrate](../internal/account_credentials.md) is separate from the Entry DAG.
The ordinary login/key/preference writers also use native local records; AccountDB does not carry those records.

## Explicit genesis authority

The native foundation is `Database::create` / `create_with_init` with explicit `_settings.auth` and a `PrivateKey` or `DatabaseKey` signer.
It supports direct-only, delegation-only and mixed Auth without inserting an independent creator grant.
An app with effective `Admin(20)` can initialize a root that preserves stronger owner authority, including a pre-existing Identity delegation bounded by `Admin(0)`.
Genesis validates its actual signer against the initial Auth and complete locally Verified proof; subsequent entries use causal pre-write Auth and enforce priority protection of the owner.
There is no automatic personal parent, new app principal or distinct ownership ACL.
Identity creation remains separate and optional, and an organizational reference grants no authority.

`Account::create` uses this primitive with an explicit `AuthSettings` and descriptive name. It creates no login and does not require an Identity.
The separate `NewAccount` convenience flow still creates its ordinary default root-key grant.
Applications using explicit authority must supply every target's intended Auth rather than assume a reference conveys the owner's grants.
Credential bundles and immutable acting handles remain separate from replicated authority.
No live data, existing Auth, old history or local verification labels are migrated or deleted.
See the [tested direct/delegation/mixed examples](../user_guide/authentication_guide.md#creating-with-explicit-authority) and [genesis invariants](authentication.md#explicit-genesis-authority).

## Owned apps and other linked Accounts

`accounts` is a reference namespace for a person, group or app to organize other Accounts by name. An Account can contain zero, one or many `identities` references, including references to Identities used by other Accounts. No reference implies ownership; a group may delegate to an existing Identity without making its members administrators of the personal Account. There is no recursive activation, inherited permission, or mandatory Identity per Account.

`InstanceAdmin::provision_owned_app` creates one app Account for all installations. The selected Identity's active administrator signs genesis through a verified snapshot of that Identity. Genesis delegates to the Identity, bounded by `Admin(0)` with no minimum, and gives a fresh installation key direct `Admin(20)`. The selected member must be stronger than the app grant. The app key is not added to the Identity and the app does not receive the selected member's private key. Local login registration precedes the forward reference write in the organizing Account. There is no required reverse link.

Use one setup writer per alias. Genesis, login registration and reference publication do not share a commit; the root and phase of a known partial result let a caller repair the original setup. `remove_reference` changes only the current view of one alias, not the history or authority. Forgetting local tracking and authoring a revocation on a target are separate operations.

Each downstream database has its own Auth. Direct grants target a key; another installation key on the same app Account does not inherit them. A database can instead delegate to the app Account, with explicit bounds that apply to the entire path through that Account. A later member needs a current enough verified snapshot to prove its grant; a reference cannot substitute for acquisition and verification of that proof. Use a separate stronger owner path on a downstream database if the app delegation would clamp the owner's authority. Existing causal revocation and public-floor semantics still apply.

Another installation uses its own key, an approved grant on the **same** app Account and local registration of that existing root. The registration API consumes an already authorized direct key; it does not create a new root, obtain approval, request Identity membership or configure background authority. The [second-installation walkthrough](../user_guide/linked_accounts.md#reuse-the-root-on-another-installation) composes ordinary ticket bootstrap with that registration and separately enrolls only the Identity it needs.

### Independent personas

An ordinary owned app deliberately exposes the owner Identity path to anyone with its Auth history. For a public persona that should not reveal the private organizer, create its own key and authority chain instead. Do not reuse organizer keys, delegate from a private parent, or put private root IDs in outward metadata. The organizing Account can link to the persona in one direction. Its controller can discover or unlink that reference, but cannot use it to sign as the persona or recover its keys.

This is separation of histories and proof dependencies, not anonymity. Readers of the organizer see the links. A cohost, peer with access to both histories, or observer of common endpoints and profile details may still correlate them. The public-proof test in `linked_accounts_tests` verifies the public history on an instance given only its public trees; it does not model network anonymity or erase prior disclosures.

## Current custody and security boundary

Fresh Account histories contain public Auth and portable application references, not private-key/unlock records or installation activation preferences.
Native `login:<name>` and `account-state:<UUID>` records hold convenience credentials and local bookkeeping; `_users` retains instance-admin Auth only.
All convenience logins use the authenticated `CredentialBundle` format: the Account locator and explicit keys are one payload under the established Account Argon2id/AES-GCM primitives.
Passwords are required string values; every string, including empty, uses the same encrypted format.
Empty-string wrapping provides no meaningful confidentiality against a bundle holder.
`AccountSession::from_credentials` consumes caller-unlocked credentials and an explicit Account key; `from_signer` accepts a signer plus Account locator and optional explicit Identity context.
Both check Account Auth without a daemon login lookup and keep their own tracking/key hints in session memory.
They do not enable daemon background synchronization or install a credential-store backend.

No historical key deletion, compatibility import, live migration or deployment occurs here.
Old histories can still contain credentials and must not be advertised as scrubbed.
Trusted Unix-socket clients still unlock and sign in-process; the optional dashboard is the documented daemon-process exception.
See [local record formats and lifetimes](../internal/account_credentials.md).
The linked-account tests join one Account by ticket from two isolated instances with distinct local keys, then reopen the joined root through ordinary login. They explicitly fetch public Identity history and test that it grants neither membership nor automatic activation.

## Application-owned root references

Applications store direct database IDs in their Account under a namespace and name through the session:

- `AccountSession::set_reference(namespace, name, id)` writes a reference.
- `AccountSession::reference(namespace, name)` reads it as `Option<ID>`.
- `AccountSession::open_reference(namespace, name)` reads it and opens the target with `open_database`.
- `AccountSession::remove_reference(namespace, name)` unlinks it without changing target Auth or local tracking.

The namespace separates applications that share one Account; the name is an application alias.
Reference names must be nonempty and contain no dots; invalid names are rejected before reading or writing.
Each namespace is kept in its own Account store; local key/tracking records are outside the DAG altogether.
There is no mandatory account-wide catalog, alias index, tags schema, or recursive activation.
Reading an ID does not download, activate, or authorize the referenced database.
Opening goes through the existing key and Auth path, exactly as `AccountSession::open_database(id)` does.
The native `databases` map supports local activation bookkeeping; it is not a portable discovery interface.
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
Native local login insertion is create-if-absent; a concurrent duplicate name is refused.

### Portable group versus device-local peer selection

An app's shared group/root database ID belongs in its Account's application metadata.
A peer database selected for one installation is an operational choice, not a universally selected peer for every installation of that Account.
Consumers should keep that selection in their own instance-local configuration, or explicitly key it by a stable installation identity; they must not interpret a single portable `peer` field as the local choice everywhere.
A missing local peer selection requires explicit initialization under the same serialization rule.
No generic peer catalog is defined; key mappings and activation preferences are now installation-local.

## Sharing and bootstrap remain existing mechanisms

AccountSession sharing and database-access requests continue using ordinary Auth and the existing bootstrap flow.
`AccountSession::join()` joins a database from a ticket and returns it opened, using embedded sync or daemon networking while the signing key stays client-owned.
Pending approval requires an explicit retry and writes no new Account mappings or preferences; rejection is terminal for that request.
The same basic operation can acquire the existing Account's history and verify the new key's authority. It does not switch logins: the installation then separately registers its key for the existing root and opens that login. Request decision bookkeeping is independent of the signed Auth grant and currently requires a direct target Admin. No daemon device is enrolled.
Held/unreadable recovery, automatic delegated-history acquisition, and restart-safe completion remain outside this API; see [current recovery limits](../user_guide/bootstrap.md#current-recovery-limits).
Successful network transfer is distinct from local key-mapping/tracking persistence.
If local bookkeeping fails after a grant, the error remains visible and retries use the existing flow; changing names does not implement a new join engine.
Explicit signer selection and per-database mappings remain necessary.
See [Bootstrapping](bootstrap.md) for request, transfer, approval, and retry behavior.

### Identity references and activation

The `identities` Account reference namespace names Identity roots, not grants. Account Auth and the Auth on each referenced Identity are independent. A local `TrackedIdentity` record binds the name, root, selected key, original request key and permission, enrollment preferences, and Pending/Active state. Pending enrollment exposes the requested preferences; the database tracking record is authoritative for current preferences. The original request tuple remains stable when the selected signer changes. Keys and local tracking are not part of the reference; caller-held sessions retain tracking in memory.

Each Identity is an ordinary globally readable database at creation. An explicit active member is needed for local activation; public Read is not enough. Activation of a pending selection checks the exact requested permission against verified local Auth. Key selection produces a new handle, and neither selection nor forgetting revokes or rebinds older handles. An Identity delegation supplies the chosen root and verified snapshot, not a shortcut around the target's `PermissionBounds` or causal Auth rules.

`register_identity` uses the [shared bootstrap request lifecycle](../internal/bootstrap.md#identity-membership). The peer decision, verified transfer, local metadata, and portable reference are not one transaction. A reference can outlive a failed registration; it is useful for locating the original root and confers no access. A failed initial exchange removes only its provisional local row; later failures preserve existing tracking, and approval or replicated history can survive an error. Serialize first-time initialization and recover using the original root and request rather than allocating another Identity. See the [membership walkthrough](../user_guide/identity_guide.md) for the caller and approver steps.

### Mapping persistence and retry safety

Key mappings and tracked-database preferences are written together in one native
installation-local record. Staging a mapping does not update the session's key-manager cache: cache
publication follows compare/exchange acknowledgement or a read-back of persisted
local metadata. A failed or lost acknowledgement is not rollback; read-back can recover
an already committed mapping while the operation still reports its error.

Tracking rediscovers the valid SigKey even for an unchanged key, so a legacy or
provisional identity is not retained merely because a row exists. Mapping and preference edits append no Account configuration history.
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
The observed preference check is inside the local compare/exchange retry; it does not coordinate choices across installations.
