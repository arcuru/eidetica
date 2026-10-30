# Key Management

Private credentials, local key selection and database Auth have separate owners.
Account references organize databases; they do not supply private keys, enroll Identity members or grant authority.
See [Accounts](accounts.md) for the portable organization and [local record formats](../internal/account_credentials.md) for the concrete storage layout.

## Stored credentials

Ordinary login registration and key metadata use native installation-local records, outside the immutable Entry DAG and Store-state caches.
A login identifies an Account root and that installation's explicitly configured keys.
A second installation must obtain independently authorized keys; syncing AccountDB does not transfer private credentials or activation choices.
App credentials must be scoped separately from the owner's personal credentials.
The library does not infer key custody from an Identity reference.

Two wrapping interfaces currently coexist:

- Convenience login uses `AccountCredentials` and per-key `KeyStorage` values in native records.
  One password unlocks the root and additional keys using the login's salt.
  Passwordless logins deliberately retain plaintext local storage.
- `CredentialBundle` authenticates the Account locator and explicit key list together under AES-GCM.
  `UnlockedCredentials` supplies caller-held signers independently of daemon login storage.
  This format is not yet the ordinary convenience-login encoding, and full AccountSession construction from it remains unfinished.

Both reuse the existing Account Argon2id derivation: version 0x13, 19456 KiB, two iterations, parallelism one and a 32-byte output.
The generated base64 salt text is passed to Argon2, not decoded first.
AES-256-GCM uses fresh 96-bit nonces.
The authenticated bundle adds fixed domain/version AAD; convenience per-key wrapping retains its existing format.
PasswordStore's separately owned KDF/AAD formats and upgrades are not changed here.
There is no separate password-verification hash: decryption verifies the password.

No existing live records, private keys or historical Entries are migrated or deleted.
An old Account history can still contain credentials even if current fields are removed.
A binary rollback does not restore an earlier storage format.
Operators need a separately approved rollout/migration plan for existing data.

## Signer lookup and authority

`AccountKeyManager` owns decrypted keys and metadata during a login.
Per-database `database_sigkeys` mappings are installation-local lookup hints.
`track_database` discovers an applicable SigKey for the explicitly supplied key; explicit `map_key` records the caller's choice.
`find_key` searches those mappings; it is not a precomputed reverse access index.
Neither stored hints nor Account access confer permission: target Auth and the actual signature remain the boundary.

`AccountSigner` binds one Account locator, actual private signer and explicit `DatabaseKey` identity immutably.
Direct-key access needs no Identity database.
Delegation uses an explicit SigKey path and verified proof history, with no silent direct-key fallback.
Selecting another handle leaves existing handles unchanged.
Service clients prove possession before the daemon accepts an acting key; they still sign Entries on the client.
The socket remains filesystem-trusted, not a hostile-client sandbox.
The optional dashboard retains its documented daemon-process signing exception.

## Updates, lifetime and loss

Native observed-byte compare/exchange prevents unrelated concurrent local mapping/preference changes from being overwritten.
It is not a transaction with peer grants, database creation or a distributed enrollment protocol.
A later bookkeeping failure may leave earlier durable effects; callers must retain the error and retry explicitly.
A lost acknowledgement is not rollback, and caches are refreshed from persisted local metadata.

Session keys and wrapping-key owners are zeroized when dropped.
Independent signer and database handles retain their own key clones: logout is not Auth revocation.
This does not promise erasure of serde/parser/allocator copies, swap, core dumps or memory held by a compromised process.
The concrete buffer ownership and limits are described in the [local credential chapter](../internal/account_credentials.md#bundle-format-and-key-lifetimes).

`UnlockedCredentials::rewrap` changes a bundle password without rotating or revoking signing keys.
Old copies can remain decryptable under their old password, and retained signers still work.
There is no ordinary convenience-login password-change API, recovery/export guarantee, automatic key backup, or stronger live revocation in this change.
Loss of every usable key or unlock credential can make an Account inaccessible.

## Verification

Bundle regressions check tampering, formats, wrong passwords, locator substitution, scoped key lists and retained signers.
Local-state regressions inspect fresh Account/system histories, preserve concurrent choices and lost-ack outcomes, and test ordinary logout/returning login.
Distinct-key fixtures use the same Account on two installations, with different activation choices and actual embedded/socket signatures.
They preload public history and independently authorized registrations; they do not claim the separate enrollment workflow is shipped.
