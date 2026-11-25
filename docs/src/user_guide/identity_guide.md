# Identity membership

An Identity is an ordinary database whose Auth records explicit member keys.
`AccountSession::create_identity` gives its initial key Admin and sets a global Read floor.
Anyone who can obtain the public Identity history can read it; reading it does not make their key a member.
Don't put secrets in an Identity.

An Account can refer to zero or more Identities by name. `identity_id(name)` reads that portable reference without opening the Identity, importing keys or granting access to it.
`tracked_identity(name)` and `identities()` read this installation's choices; `get_identity(name)` opens an already active selection, never activates or creates one.
Use an Identity only when you want its explicit signing context. `acting_as(&key_id, None)` is still the direct-key path.
An existing Identity can be reused as the owner of an [app or group Account](linked_accounts.md); an app's own login key need not be an Identity member.

## Create and select an Identity

Use one initializer for a name. Separate installations can otherwise create distinct roots before either sees the other's Account reference.
An error after creation can carry the committed root via `Error::created_database_id`; reuse that root instead of retrying creation blindly.

```rust
# use eidetica::{Instance, NewAccount, auth::{AuthKey, Permission}, backend::database::InMemory};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (_instance, mut account) = Instance::create_backend(
    Box::new(InMemory::new()), NewAccount::with_password("owner", "local password"),
).await?;
let first = account.get_default_key()?;
let identity = account.create_identity("team", &first).await?;
assert_eq!(account.identity_id("team").await?.as_ref(), Some(identity.root_id()));

let second = account.add_private_key(Some("other local key")).await?;
identity.add_key(&second, AuthKey::active(None, Permission::Write(5))).await?;
let selected = account.select_identity_key("team", &second).await?;
assert_eq!(selected.key_id(), &second);
assert_eq!(identity.key_id(), &first); // The earlier handle is still bound to first.
# Ok(())
# }
```

Selection requires a private key the caller already holds and explicit active membership for that key. Importing a key, when needed, is a separate operation.
`Identity::database()` gives the underlying Database. To sign for a different database that delegates to the Identity, use `Identity::open_database(target_id)`; it uses this Identity's root and does not fall back to another Identity or a direct grant.
The target's [delegation bounds](authentication_guide.md) still apply. A reference in an Account is not a delegation.

`forget_identity(name)` removes only local tracking and selection. It neither revokes a key nor deletes the Identity, drops its Account reference, disables database sharing, or rebinds existing handles.

## Join an existing Identity from another installation

1. On the sharing installation, enable sync and a listening transport. Call `identity.database().share().await?`, then `identity.ticket().await?` on its session-owned handle. Send the locator through your normal onboarding channel; the ticket contains no private key or authority.
2. The requester generates or selects a key held on **its own** installation. Call `register_identity` with the ticket, that key, the requested permission and explicit local `SyncSettings`. It returns `IdentityRegistration::Pending` if peer approval is needed, even if the public root is already replicated. A valid existing explicit grant can satisfy the request without a new approval.
3. An approver on the sharing peer uses a proven current Admin key for that Identity root. Inspect `pending_access_requests(root, admin_key)`, verify who is requesting membership out of band, and decide whether to grant the exact requested permission. Key proof is not proof of a person's identity. If approving, first sign the ordinary Auth grant using the Identity's admin handle, then call `complete_access_request(root, request_id, admin_key)`. To deny, call `reject_access_request` on the same root and request instead, without adding a grant.
4. The requester explicitly repeats `register_identity` with the same name, root, enrollment key and requested permission. `Active(identity)` means membership was verified and local tracking completed. A rejection is an error and remains terminal for that exact request. Changing root, enrollment key or requested permission for the same tracked name is a conflict, not a replacement.

While enrollment is pending, `tracked_identity(name)` exposes the requested preferences through `pending_sync_settings()`. Once active, read the current preferences from `identity.database().sync_settings().await?` or from `account.tracked_database(identity.root_id()).await?`. Use `identity.database().share().await?` or `stop_sharing().await?` to change sharing, including after key rotation. Neither requires a ticket or the enrollment key. Retrying registration can repair local tracking, but it does not overwrite current database preferences.

```rust,no_run
use eidetica::{AccountSession, Result, account::{Identity, IdentityRegistration, SyncSettings},
    auth::{AuthKey, Permission, crypto::PublicKey}, sync::DatabaseTicket};

async fn request_membership(
    session: &mut AccountSession, ticket: &DatabaseTicket, key: &PublicKey,
) -> Result<IdentityRegistration> {
    session.register_identity("team", ticket, key, Permission::Write(5), SyncSettings::on_commit()).await
}

async fn approve(
    session: &AccountSession, identity: &Identity, id: &str, key: &PublicKey, permission: Permission,
) -> Result<()> {
    // Call only after inspecting this request and deciding to grant its authority.
    identity.add_key(key, AuthKey::active(None, permission)).await?;
    session.complete_access_request(identity.root_id(), id, identity.key_id()).await
}
```

These calls work with embedded instances or connected clients. Start the network listener on the peer that shares the Identity; a connected client does not start its own sync engine. The client signs proofs and grants, and the daemon runs the network exchange and verifies submitted Entries.

The signed grant and recorded decision are separate commits. If the second fails, inspect the existing grant and retry finalization; do not blindly resubmit a settings update. If verified membership has arrived separately, `activate_identity(name)` explicitly checks the local grant and activates the local selection. It does not contact the peer, change its request status, or rewrite database sync preferences; retry `register_identity` to finish that bookkeeping. Local activation is not proof of a peer decision or of global catch-up.

Register separately for each required Identity. There is no automatic enrollment of all Account references, background completion or stronger global revocation guarantee. See [the bootstrap lifecycle](../internal/bootstrap.md#identity-membership) and [Accounts design](../design/accounts.md#identity-references-and-activation).

## Refresh delegating databases after removing a key

`Identity::revoke_key` changes the Identity only. Each database that delegates to it keeps the pin in its own Settings until someone with authority there moves it. Until then, a removed key that still claims an older Identity snapshot can sign entries for that database.

Live access checks on a replica that has accepted the removal already use the new state. Moving the pin makes the removal part of the delegating database's signed history: entries descending from that Settings write can no longer claim an Identity snapshot below the pin.

Eidetica has no global list of databases that delegate to an Identity. Work from the list you keep, and treat any other database as not refreshed.

1. Remove the key, then take the Identity's Verified snapshot as the requested cut.
2. Sync the Identity to the replica where each delegating database is administered. A [watch](transactions.md) on that replica's default Identity handle (`Database::on_write`) reports the Verified snapshot it has reached. Verifying the Identity does not recheck other databases; if one was waiting for it, run `verify()` on the engine that holds it.
3. At that replica, stage `SettingsStore::advance_delegation` and commit it with a key allowed to change that delegation.
4. In a later transaction, call `advance_delegation` again. `false` means the pin this replica accepts covers the cut. Drop that transaction; it wrote nothing.
5. Read the removed key's state at the accepted pin. A pin can also name a concurrent Identity branch, and settings from that branch can win, so covering the removal is not the same as the key being revoked there. If it is still active, remove it again on the joined Identity history and refresh to that cut.

```rust
# use eidetica::{Instance, NewAccount, auth::{AuthKey, Permission, PermissionBounds,
#     crypto::generate_keypair}, backend::database::InMemory};
use eidetica::{Database, Result, Snapshot, auth::{KeyStatus, crypto::PublicKey}};

async fn covered(target: &Database, identity: &Database, cut: &Snapshot) -> Result<bool> {
    let tx = target.new_transaction().await?;
    Ok(!tx.get_settings()?.advance_delegation(identity, cut).await?)
}

async fn refresh(target: &Database, identity: &Database, cut: &Snapshot) -> Result<&'static str> {
    let tx = target.new_transaction().await?;
    if !tx.get_settings()?.advance_delegation(identity, cut).await? {
        return Ok("already covered");
    }
    tx.commit().await?;
    if covered(target, identity, cut).await? {
        Ok("verified here")
    } else {
        Ok("not advanced: another settings write won")
    }
}

async fn revoked_at_pin(target: &Database, identity: &Database, key: &PublicKey) -> Result<bool> {
    let auth = target.get_settings().await?.auth_snapshot().await?;
    let pin = Snapshot::new(auth.get_delegated_tree(identity.root_id())?.tree.tips);
    let tx = identity.new_transaction_at(&pin).await?;
    Ok(tx.get_settings()?.get_auth_key(key).await?.status() == &KeyStatus::Revoked)
}

# #[tokio::main]
# async fn main() -> Result<()> {
# let (_instance, mut account) = Instance::create_backend(
#     Box::new(InMemory::new()), NewAccount::passwordless("owner")).await?;
# let key = account.get_default_key()?;
# let identity = account.create_identity("personal", &key).await?;
# let (_, phone) = generate_keypair();
# identity.add_key(&phone, AuthKey::active(None, Permission::Write(10))).await?;
# let mut known = Vec::new();
# for _ in 0..2 {
#     let (database, _) = account.new_database().build().await?;
#     let tx = database.new_transaction().await?;
#     tx.get_settings()?.add_delegated_tree(identity.as_delegation(PermissionBounds {
#         max: Permission::Write(10), min: None }).await?).await?;
#     tx.commit().await?;
#     known.push(database);
# }
identity.revoke_key(&phone).await?;
let cut = identity.database().snapshot().await?;

// Keep one result per database. An error is not an advance.
for database in &known {
    let outcome = refresh(database, identity.database(), &cut).await;
    let revoked = revoked_at_pin(database, identity.database(), &phone).await;
    println!("{}: {outcome:?}, key revoked at pin: {revoked:?}", database.root_id());
}
# for database in &known {
#     assert!(covered(database, identity.database(), &cut).await?);
#     assert!(revoked_at_pin(database, identity.database(), &phone).await?);
# }
# Ok(())
# }
```

Report each database on its own:

| Outcome               | Evidence                                                                                                                                                                                                                                                                                                                                                                |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Requested             | You asked an operator, or committed at another replica. A successful sync does not prove acceptance.                                                                                                                                                                                                                                                                    |
| Verified at recipient | After a commit, `advance_delegation` on a default handle for the recipient's replica returns `false`. A connected client reads its daemon's Verified state; don't use an `allow_unverified` handle for this check.                                                                                                                                                      |
| Already covered       | The first call returned `false`, so nothing was written.                                                                                                                                                                                                                                                                                                                |
| Not advanced          | The pin still does not cover the cut: an unreachable replica, no such delegation, a signer without authority over its bounds, Identity history that is missing or not yet Verified, or another settings write that won. An error alone can hide a commit that landed, so read back before reporting. Fix the cause and retry; the cut and the other results still hold. |

`advance_delegation` is not specific to key removal: any Verified snapshot of a delegated database can be the cut. It keeps the bounds it reads and never drops history the pin already covers; if the two frontiers are incomparable, the new pin names both. A pin holds at most 64 tips; if commit refuses a wider one, merge the Identity's branches and refresh to that snapshot. Re-adding `Identity::as_delegation` instead would need the bounds restated, and could move the pin backward from a replica that is behind.

Know the limits before relying on a cutoff:

- **Per replica and per branch.** A replica that has not accepted the refresh, and any branch that does not descend from it, can still accept entries signed under the old pin. Those entries stay valid when the branches merge. There is no wall-clock cutoff.
- **Concurrent Settings writes.** A delegation merges as a whole value. A concurrent bounds change, removal or other pin write can win over your refresh, or lose to it. A refresh that wins can undo a concurrent narrowing or removal. After branches merge, check both the pin and the bounds, then repeat the change that lost.
- **Nested delegation.** A pin covers one step. For `project -> team -> identity`, advance the team's pin to the Identity cut first, then the project's pin to a team snapshot where the new pin is the effective one, not merely in its history. Each step needs its own database's authority.
- **Existing handles.** An `Identity::open_database` handle keeps the snapshot it was opened at. If that is below the new pin, its commits fail; reopen it.
- **Other grants.** A direct grant, a global grant or another delegation that admits the same key is unaffected. History is not rewritten, so an earlier snapshot still records the pin and grant it had.

The [authentication design](../design/authentication.md#causal-snapshot-validation) defines how pins become floors.
