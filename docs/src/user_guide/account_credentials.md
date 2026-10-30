# Explicit credential bundles and signers

An Account locator, private credentials and database authorization are different things.
A locator identifies the Account; it does not grant access or select an Identity.
An installation should hold its own independently authorized keys, not another installation's personal bundle.
An app receives a separate scoped bundle with its own Account locator and key(s).
Its scope comes from database Auth, not a bundle label.

## Available interfaces

`CredentialBundle` encrypts exactly the supplied locator and keys with one password.
`AccountSigner` binds a locator, an actual signing key and an explicit Auth identity immutably.
These interfaces work without registering a daemon login; they do not discover, enroll or import keys from Account references.

The ordinary `Instance::login_account` convenience path stores login credentials, additional keys, signer hints and sync preferences in native installation-local records.
These records are not Entries and do not replicate with AccountDB.
Adding an Account reference neither imports credentials nor activates its target.
Existing Account histories are not made secret-free by removing current fields; this change does not delete or migrate live data.

Protected convenience logins use the same authenticated `CredentialBundle` format as explicit caller-held credentials.
`AccountSession::from_credentials` consumes an unlocked bundle and an explicit Account key selection; `from_signer` accepts one `AccountSigner`, including an explicit delegated identity.
Neither constructor needs a registered daemon login.
There is no pluggable client-owned credential-store backend, device-enrollment workflow or recovery/export feature in this API.

## Convenience login, logout and returning login

One local password unlocks the explicitly stored keys for that login.
Do not register an owner's personal credentials as an app login: provision an independently authorized app key instead.
Passwordless creation retains plaintext local credentials and is only for trusted environments.

```rust
# use eidetica::{Instance, NewAccount, backend::database::InMemory};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
let (instance, mut session) = Instance::create_backend(
    Box::new(InMemory::new()), NewAccount::with_password("local", "local password"),
).await?;
let key_id = session.add_private_key(Some("this installation")).await?;
let retained = session.acting_as(&key_id, None)?;
session.logout()?;
assert!(instance.login_account("local", Some("wrong password")).await.is_err());
let returning = instance.login_account("local", Some("local password")).await?;
assert_eq!(returning.get_signing_key(&key_id)?.public_key(), retained.key_id());
# Ok(())
# }
```

Connected logins use the same `login_account` call on an `Instance::connect("unix:///path/to/eidetica.sock")` result.
Password derivation, key decryption and signing run on the client; additional local key records are returned only within that authenticated login's scope.
The socket is still filesystem-trusted, including the pre-auth encrypted-login lookup.
Logout drops that session's key owners; it does not revoke Auth or erase independent signer/database handles.

## Set up, unlock and retain an explicit signer

Supply only keys the caller is intended to hold.
The locator below is an example value; real callers use the existing Account's root ID and obtain authorization separately.

```rust
use eidetica::{ID, account::{AccountSigner, CredentialBundle}, auth::crypto::generate_keypair};

let account_id = ID::from_bytes("example-account-locator");
let (installation_key, key_id) = generate_keypair();
let bundle = CredentialBundle::seal(&account_id, &[installation_key], "local password")?;
let saved_bytes = serde_json::to_vec(&bundle)?; // Write to local storage, never an Account Entry.

// Read the encrypted bytes later and unlock on the client.
let saved: CredentialBundle = serde_json::from_slice(&saved_bytes)?;
let unlocked = saved.unlock_for(&account_id, "local password")?;
let acting = AccountSigner::new(account_id.clone(), unlocked.signer(&key_id)?);
assert_eq!(acting.key_id(), key_id);
assert!(saved.unlock("wrong password").is_err());
assert!(saved.unlock_for(&ID::from_bytes("another-account"), "local password").is_err());

// Ending this unlock does not revoke independently retained signer handles.
drop(unlocked);
let relogin = saved.unlock_for(&account_id, "local password")?;
assert_eq!(relogin.key_ids(), vec![key_id]);
assert_eq!(acting.account_id(), &account_id);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`AccountSession::import_private_key(key, label)` adds an explicitly supplied key to that session's local bundle using the same observed-bundle update as key generation.
It rejects duplicate keys without replacing their metadata and does not enroll Identity membership, grant database Auth or activate a target.

`UnlockedCredentials::add_key` adds only an explicitly supplied key.
Call `seal()` and explicitly persist the result to save that addition; a failure to save does not update the old encrypted bundle.
`signer(key_id)` fails for an absent key rather than falling back to a personal or default signer.
`rewrap(new_password)` produces a new encrypted bundle with the same keys and a new salt.
It is **not key rotation or revocation**: old copies remain decryptable with their old password, and retained handles can still sign.
Losing every usable key/bundle/password has no automatic recovery guarantee.

## Full sessions without daemon login storage

Use a caller-unlocked bundle when the application, rather than the daemon login directory, owns credential storage.
The session checks Account Auth with the selected actual signer before returning.
This example uses an existing authorized key; provisioning a separate app Account/key and device enrollment are separate operations.

```rust
# use eidetica::{Instance, NewAccount, account::{AccountSession, CredentialBundle}, backend::database::InMemory, crdt::Doc};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (instance, owner) = Instance::create_backend(Box::new(InMemory::new()), NewAccount::with_password("example", "example password")).await?;
let account_id = owner.account().id().clone();
let key_id = owner.get_default_key()?;
let saved = CredentialBundle::seal(&account_id, &[owner.get_signing_key(&key_id)?], "client password")?;
owner.logout()?;
let unlocked = saved.unlock_for(&account_id, "client password")?;
let mut session = AccountSession::from_credentials(&instance, unlocked, &key_id).await?;
assert!(session.account_info().is_none()); // No daemon login registration was used.
let target = session.create_database(Doc::new(), &key_id).await?;
session.set_reference("example", "root", target.root_id()).await?;
let retained = session.acting_as(&key_id, None)?;
session.logout()?;
let reopened = AccountSession::from_signer(&instance, retained).await?;
assert_eq!(reopened.reference("example", "root").await?, Some(target.root_id().clone()));
assert!(reopened.databases().await?.is_empty()); // Activation was session-local.
# Ok(())
# }
```

For a service, create a fresh proof-authenticated connection first with `AccountSigner::connect`, then pass it to the full-session constructor.
The constructor does not reauthenticate an existing connection or fall back to a daemon login.

```rust,no_run
use eidetica::{ID, Result, account::{AccountSession, AccountSigner, CredentialBundle}, auth::crypto::PublicKey};

async fn connect_session(url: &str, account_id: &ID, bundle: &CredentialBundle, password: &str, key_id: &PublicKey) -> Result<AccountSession> {
    let unlocked = bundle.unlock_for(account_id, password)?;
    let signer = AccountSigner::new(account_id.clone(), unlocked.signer(key_id)?);
    let connected = signer.connect(url).await?;
    AccountSession::from_credentials(&connected, unlocked, key_id).await
}
```

Caller-held sessions keep key hints, tracking and activation preferences only in session memory.
They do not register logins, write native daemon credential records or enable background synchronization.
`login_id()` and `username()` are empty and `account_info()` is `None` for these sessions.
A new session can read portable references but does not inherit the previous session's local target mappings.
Use `track_database` with a held key or an explicit `AccountSigner` to open an authorized target.
A reference never supplies a missing signer or membership.

Keys added through a caller-held session belong only to that session until the caller explicitly saves them in its own credential store.
They do not update the original encrypted bundle.
To persist such a key, supply `session.get_signing_key(&new_id)?` to the caller's unlocked bundle with `add_key`, then seal and save it explicitly.
This is low-level storage control, not a recovery/export service.

## Embedded and service use

`acting.open_database(&instance, &root_id).await` opens the exact target under the selected signer and Auth identity.
Target Auth and verified proof availability still apply.
A direct key needs no Identity database.
For delegation, construct the handle with an explicit `DatabaseKey`/`SigKey` representing the chosen Identity path, or use `with_identity` to obtain a new handle.
An invalid path fails even if the same key has a separate direct grant.
Never turn an organizational Identity reference or lookup hint into an authorization path automatically.

`AccountSession::acting_as(&key_id, None)` selects a direct signer from an already unlocked session.
Passing `Some(identity)` selects that exact Auth identity.
Both return a new `AccountSigner`; existing handles retain their original keys and identities.
`logout()` consumes the session, not independent handles or other connections.

For a service, use `acting.connect("unix:///path/to/eidetica.sock").await` to create a fresh connection and prove possession without fetching a login record.
Then use `open_database` on that instance.
The daemon receives public keys and challenge signatures, not the caller's password or private keys.
A new selection does not reauthenticate or silently rebind existing connections.
No GUI split or untrusted-app sandbox is provided.

The existing socket is for filesystem-trusted local callers, not hostile clients.
Such callers can still request existing encrypted login records; processes sharing backend-file access are not isolated from each other.
The optional dashboard retains its existing daemon-process signing sessions during this transition.
See [local storage and session ownership](../internal/account_credentials.md) and [the service threat model](../internal/service.md#security-model).
