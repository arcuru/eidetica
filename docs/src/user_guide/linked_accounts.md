# Linked Accounts and app login

An Account is an ordinary database for a person, group, or app. Use its root ID to identify it; its display name and the daemon's local login name can differ.
Organize related Accounts with `set_reference("accounts", alias, root_id)` and find them with `reference("accounts", alias)`. The convention works at any depth, with no automatic traversal. The `identities` namespace names Identity roots; applications can use other namespaces for their data roots. An alias is nonempty and has no dots.

Anyone who can read an Account can see its references. A reference does not grant access to its target, copy keys, or start synchronization. The target's Auth is what matters.

## Provision an owned app and log in

Run setup once per app Account, as an Instance administrator with write access to the organizing Account. Pick an active administrator of the owner Identity with stronger priority than `Admin(20)` (a smaller number). Use `create_identity` if you need a new Identity, or pass an existing [`Identity`](identity_guide.md) handle. Choosing an existing Identity does not create another one or change its membership.

Here the person bootstraps an embedded Instance as its administrator. The app gets a separate protected login on that same Instance:

```rust
# use eidetica::{Instance, NewAccount, backend::database::InMemory};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (instance, mut person) = Instance::create_backend(
    Box::new(InMemory::new()), NewAccount::with_password("alex", "personal example password"),
).await?;
let owner_key = person.add_private_key(Some("owner Identity key")).await?;
let owner = person.create_identity("work", &owner_key).await?;
let app_root = person.admin().await?.provision_owned_app(
    "notes", &owner, NewAccount::with_password("notes-local", "app example password"),
).await?;
assert_eq!(person.reference("accounts", "notes").await?, Some(app_root.clone()));

let app = instance.login_account("notes-local", "app example password").await?;
assert_eq!(app.account().id(), &app_root);
assert_eq!(app.account().get_name().await?, "notes");
let key = app.get_default_key()?;
assert_eq!(app.list_keys()?, vec![key.clone()]);
let acting = app.acting_as(&key, None)?; // Direct Auth; no default Identity.
let app_db = acting.open_database(&instance, &app_root).await?;
assert_eq!(app_db.current_permission().await?, eidetica::auth::Permission::Admin(20));
assert!(acting.open_database(&instance, person.account().id()).await.is_err());
# Ok(())
# }
```

For a daemon, connect with `Instance::connect("unix:///path/to/eidetica.sock")` and use these same provisioning and login APIs. Give the app a **separate connection** and only its local login name and password. Do not log the app in over the person's connection: one connection has one current login. The app uses ordinary `login_account`; it does not need to save a bundle itself or hold a personal key. The daemon stores the encrypted login bundle, while unlock and signing happen in the client. Use distinct nonempty passwords; the local socket and backend files still require [trusted access](../internal/service.md#security-model).

The app can administer its own Account subject to the existing priority rules. The stronger owner delegation protects owner administration, and the app key is not a member of the owner's Identity. The app Account does not point back to the personal Account. Ordinary login retains the existing best-effort daemon-device Read grant on the Account when permitted; it does not authorize background access to other databases.

## Create a group Account or use just a key

A group can delegate its Auth to an Identity already used elsewhere. This uses the chosen member as the actual signer, leaves the group key policy in the group Auth, and places a discovery link in the person's Account:

```rust,no_run
use eidetica::{Account, AccountSession, Instance, Result, ID, account::Identity,
    auth::{AuthSettings, Permission, types::PermissionBounds}, database::DatabaseKey};

async fn organize_group(instance: &Instance, person: &AccountSession, team: &Identity) -> Result<ID> {
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(team.as_delegation(PermissionBounds {
        max: Permission::Admin(0), min: None,
    }).await?)?;
    let signer = DatabaseKey::with_identity(
        person.get_signing_key(team.key_id())?, team.delegation_key().await?,
    );
    let group = Account::create(instance, signer, "Working Group", auth).await?;
    person.set_reference("accounts", "group", group.id()).await?;
    Ok(group.id().clone())
}
```

If no Identity is needed, add an explicit direct `AuthKey` to `AuthSettings` for the key that signs `Account::create`. Do not insert any Identity reference unless you want it visible to readers. `AccountSigner::new(root_id, key)` and `AccountSession::from_signer` open without a registered login; `InstanceAdmin::register_account_login` instead sets up an ordinary login for a key with an existing active direct grant. Creating an Account with explicit Auth does not register a login or give the creator an extra grant.

## Data roots need their own Auth

If the app stores a data root under `notes`/`list`, `reference("notes", "list")` returns its ID. Use a login's configured target mapping with `open_reference`, or pass the ID to an explicitly selected `AccountSigner`. Neither lookup nor open installs a key or adds a grant.

- **Direct grant:** put the app installation's public key in the data database's Auth. Other installations of the app cannot use this grant. Add separate target grants when needed.
- **Delegation:** explicitly put the app Account and a verified snapshot in the data database's Auth with suitable `PermissionBounds`. Build an acting handle that names this exact path and actual key. The target must be able to verify the path, including any newer snapshot used to prove later membership. The following example opens a target already configured this way:

```rust,no_run
use eidetica::{AccountSession, Database, ID, Instance, Result,
    auth::{crypto::PublicKey, types::{DelegationStep, KeyHint, SigKey}}};

async fn open_delegated_list(instance: &Instance, app: &AccountSession, key: &PublicKey,
    target: &ID) -> Result<Database> {
    let direct = app.acting_as(key, None)?;
    let root = app.account().id();
    let account_db = direct.open_database(instance, root).await?;
    let path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: root.clone(), tips: account_db.snapshot().await?.into_tips(),
        }],
        hint: KeyHint::from_pubkey(key),
    };
    app.acting_as(key, Some(path))?.open_database(instance, target).await
}
```

Choosing a new acting handle does not change older handles. If the selected path fails, an explicit handle does not try the same key's direct grant or another Identity. When creating data databases, provide their full initial Auth to [`Database::create`](authentication_guide.md#creating-with-explicit-authority). A lower-priority app administrator can sign genesis while preserving a stronger owner grant; it does not get a second Admin grant from being the creator.

## Reuse the root on another installation

Reopen the **same Account root** on the new installation. Do not run `provision_owned_app` again or copy a private key or login bundle. The following is a manual Rust API flow; the CLI does not combine these steps into a second-device login command.

### 1. Share the existing root

Enable sync on both installations and accept connections on the sharing peer, using a reachable [transport](synchronization_guide.md). With a session and a key already authorized on the Account, call `track_database(root, &key, SyncSettings::enabled())`, `open_database(root)`, and then `ticket()` on the opened database. Send the ticket and confirm the expected root over your onboarding channel. A ticket locates a database and peer; it is not an invitation that grants Auth.

If the Account history uses delegation (including genesis of an owned app Account), the joining installation needs the verified proof history, too. Provide those histories separately before retrying the Account transfer. In the example tests, the sharing installation shares its public owner Identity with `identity.database().share()` and `identity.ticket()`. The receiving installation's sync host fetches it with `Sync::sync_with_ticket`. This obtains public proof data without installing a member key. A connected client does not have its own `Sync`; for this public prefetch, use the owning sync host. Do not import a private organizer's chain to stand in for an independent persona's proof.

### 2. Request and approve Account access

On the new installation, log in with its existing local administrator account. Generate a fresh key there with `add_private_key(Some("notes on this installation"))`. This keeps the key available locally for retries without depending on the Account being joined. Use the same key and an explicit permission in `join`; `SyncSettings::disabled()` still allows this foreground transfer.

```rust,no_run
use eidetica::{AccountSession, Database, Result, account::SyncSettings,
    auth::{Permission, crypto::PublicKey}, sync::DatabaseTicket};

async fn join_notes(
    local: &mut AccountSession, ticket: &DatabaseTicket, key: &PublicKey,
) -> Result<Database> {
    local.join(ticket, key, Permission::Write(30), SyncSettings::disabled(), None).await
}
```

`BootstrapPending` means wait for a decision, not that the new key has access. Retrying the same root, key and permission while pending identifies the same request. On the sharing installation, a **direct Admin of the target Account** uses `pending_access_requests(root, &admin_key)`. Check the root, public key and requested permission, and verify the requester's identity independently. The key proof establishes possession, not who is holding it.

To approve, use a database handle bound to that Admin. In a normal settings transaction, call `set_auth_key(&request.requesting_pubkey, AuthKey::active(None, approved_permission))` and commit. Then call `complete_access_request(root, &request_id, &admin_key)`. Finalization checks existing Auth; it does not add a key. It refuses approval if the actual grant does not satisfy the request. To offer weaker access, decide on that scope explicitly and have the requester use that permission instead. To deny the request, call `reject_access_request` **without** adding the grant. `BootstrapRejected` is terminal for that request; don't loop or create a replacement Account. A key that can only read or write the Account cannot list or decide its requests. A delegated-only Admin may be able to sign a valid Auth update, but these session request-listing and decision methods take a direct Admin key.

Repeat `join_notes` after approval. Check that the returned root matches the ticket and that `current_permission()` meets the request. Read permits reading, not writes. Write permits data operations, not Auth changes. An Admin's grant is still subject to the existing priority rules. If the grant committed but recording the decision failed, inspect both before retrying; don't blindly reapply the grant. Missing proof history, a held replica the key cannot read, or a lost connection is not a successful join. See the [current recovery limits](bootstrap.md#current-recovery-limits).

### 3. Register and reopen the local login

`join` tracks access under the session that requested it. It does not turn that session into the joined Account's login. With the new installation's administrator, wrap the same locally held key for the now-accessible root and open the new login separately:

```rust,no_run
use eidetica::{AccountSession, Instance, NewAccount, Result, ID, auth::crypto::PublicKey};

async fn register_notes(
    local_admin: &AccountSession, separate_client: &Instance, root: &ID,
    key: &PublicKey, password: &str,
) -> Result<AccountSession> {
    local_admin.admin().await?.register_account_login(
        root, local_admin.get_signing_key(key)?,
        NewAccount::with_password("notes-here", password),
    ).await?;
    let notes = separate_client.login_account("notes-here", password).await?;
    assert_eq!(notes.account().id(), root);
    Ok(notes)
}
```

For embedded use, `separate_client` can be the same Instance. For connected use, open a separate connection to the **same local daemon** with `Instance::connect`. The administrator registers an encrypted, installation-local bundle; unlock and signing happen on the client. Later, use `login_account` again with the local name and password. `register_account_login` requires an existing active direct key grant, as well as Instance administration. It will not fetch the Account, approve the key, or overwrite an existing login. After an uncertain registration result, check whether the intended login already unlocks the expected root with the expected key before repeating it. If the key already has sufficient delegated authority and you don't need a registered login, use [a caller-held signer](account_credentials.md) with its actual delegation instead of adding a redundant direct grant.

### 4. Select only the Identities and data you need

Use `identity_id(name)` to read an Account reference and compare it with the Identity ticket. Even if the public history is present, `get_identity(name)` does not make the key a member. Follow [Identity membership](identity_guide.md#join-an-existing-identity-from-another-installation) for each needed Identity: call `register_identity` on the **new Account login**, have an Admin of that Identity approve the separate request, and retry registration until it returns `Active(identity)`. Use `identity.database()` to sign an ordinary write, or `identity.open_database(target)` where the target delegates to it. An unrelated or independent persona is not auto-enrolled. Choose sync preferences separately; activation does not enroll the daemon's device key for ongoing private sync.

Fetch each application database explicitly. A target with only a direct grant for the first installation's key requires its own grant for the new key. A target that already delegates suitable permissions to this Account can accept the new member with verified Account proof, without editing the target's Auth. The tests fetch a target with public Read, then open it with the [explicit delegation](#data-roots-need-their-own-auth) to sign a write. Public Read is only the acquisition route; it does not confer Write. For private targets, arrange authorized acquisition and proof separately. This flow does not discover delegated access automatically on connected clients.

The embedded and Unix-connected versions of this flow both run in [`linked_accounts_tests.rs`](https://github.com/arcuru/eidetica/blob/main/crates/lib/tests/it/account/linked_accounts_tests.rs). To run them against two temporary installations from the repository:

```bash
nix develop -c nix run .#test-inmemory -- -E 'test(test_owned_app_embedded) | test(test_owned_app_service)'
```

## Handle partial setup without creating duplicates

Run first-time setup through one designated writer. An existing alias or login name is an error, not permission to replace it. After genesis, provisioning registers the local login and then writes the Account reference. These are separate writes, not one transaction. `Error::created_database_id()` reports a known committed root on later failure. When linking fails after registration, keep that root and login; after checking the alias, repair it with `set_reference`. For other errors or a lost connection, read back the login and link before retrying. If the generated app key is unavailable and no login was registered, an authorized owner can grant a fresh key on the **existing root**, then register it. Do not create a replacement simply because a lookup failed.

Unlink with `remove_reference` to remove a name from the current Account view. Local forget (`untrack_database` or `forget_identity`) changes installation bookkeeping; the exact behavior depends on the operation. Neither removes the target or revokes keys. Revoke in the relevant database's Auth with a sufficiently strong key. This changes what later causally dependent writes can do; it does not erase old entries, override a public access floor, or instantly change every peer's view.

For an independent public persona, use a separate Account, keys and proof chain rather than an owner delegation from your private Account. A private Account may reference that persona without changing its proofs or acquiring authority to sign for it. Public-only readers need not learn the private link, but readers of the organizing Account and an operator who sees both trees can correlate them. See the [privacy limits](../design/accounts.md#independent-personas) and [storage layout](../internal/account_credentials.md#linked-account-provisioning).
