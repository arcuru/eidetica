# Explicit credential bundles and signers

An Account locator, private credentials and database authorization are different things.
A locator identifies the Account; it does not grant access or select an Identity.
An installation should hold its own independently authorized keys, not another installation's personal bundle.
An app receives a separate scoped bundle with its own Account locator and key(s).
Its scope comes from database Auth, not a bundle label.

## Available interfaces and current transition boundary

`CredentialBundle` encrypts exactly the supplied locator and keys with one password.
`AccountSigner` binds a locator, an actual signing key and an explicit Auth identity immutably.
These interfaces work without registering a daemon login; they do not discover, enroll or import keys from Account references.

The ordinary `Instance::login_account` convenience path still uses the existing login/key tables.
It has **not yet been switched to the native local record store**.
Do not infer secret-free Account histories, independent installation preferences, or a shipped client-owned credential-store backend from these low-level APIs.
Existing Account histories are not made secret-free by removing their current key rows.
No live key/data migration or deletion is performed by these APIs.

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

`UnlockedCredentials::add_key` adds only an explicitly supplied key.
Call `seal()` and explicitly persist the result to save that addition; a failure to save does not update the old encrypted bundle.
`signer(key_id)` fails for an absent key rather than falling back to a personal or default signer.
`rewrap(new_password)` produces a new encrypted bundle with the same keys and a new salt.
It is **not key rotation or revocation**: old copies remain decryptable with their old password, and retained handles can still sign.
Losing every usable key/bundle/password has no automatic recovery guarantee.

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
