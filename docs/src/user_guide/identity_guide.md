# Identity membership

An Identity is an ordinary database whose Auth records explicit member keys.
`AccountSession::create_identity` gives its initial key Admin and sets a global Read floor.
Anyone who can obtain the public Identity history can read it; reading it does not make their key a member.
Don't put secrets in an Identity.

An Account can refer to zero or more Identities by name. `identity_id(name)` reads that portable reference without opening the Identity, importing keys or granting access to it.
`tracked_identity(name)` and `identities()` read this installation's choices; `get_identity(name)` opens an already active selection, never activates or creates one.
Use an Identity only when you want its explicit signing context. `acting_as(&key_id, None)` is still the direct-key path.

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

To change sync preferences for an already Active Identity, call `register_identity` with the original enrollment key and permission and the new `SyncSettings`. Keep using the original enrollment key's public ID even if that key has been revoked; do not substitute the replacement key in the request. The retry checks that the currently selected key is owned and still an active direct member, and returns a handle bound to that key. It does not send a membership request or use the ticket's peer addresses. A ticket containing only the same root (`DatabaseTicket::new(root)`) is sufficient for this Active retry, not for initial or Pending registration.

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
