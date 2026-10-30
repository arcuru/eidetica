# Local credential and signer ownership

This chapter describes the explicit bundle/signer API and native local-record substrate.
The normal login/key/tracking path is still on its historical stores; its migration is unfinished.
The distinction matters: adding a safe local storage mechanism alone does not remove secrets from existing or newly created legacy Account histories.

## Storage boundaries

| Data                                                                | Location and behavior                                                                                                                                                                    |
| ------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Portable application references                                     | Ordinary Account `refs:<namespace>` stores; references grant no authority and trigger no activation                                                                                      |
| Explicit encrypted bundle                                           | `CredentialBundle` bytes; authenticated Account locator and supplied keys; no Entry or CRDT semantics                                                                                    |
| Native local account record                                         | InMemory `local_accounts` map or SQL `local_accounts(name, data)`; mutable observed-value compare/exchange; not in any Entry DAG, tree enumeration, Store-state namespace or sync stream |
| Current convenience-login credentials, key mappings and preferences | Historical `_users`/Account stores; not yet moved onto the native substrate                                                                                                              |
| Acting authority                                                    | An immutable `AccountSigner` containing an Account ID and existing `DatabaseKey`; normal database Auth determines permission                                                             |

`BackendImpl::local_account_record` returns the exact stored bytes.
`compare_exchange_local_account_record(name, expected, replacement)` inserts only when absent (`expected = None`) or replaces only the exact previously observed bytes.
A conflicting write returns `false`; callers must reread/reconcile rather than overwrite a concurrent update.
Keep the original observed bytes, not a newly serialized approximation of the expected value.
This is installation-local atomicity, not distributed compare-and-set or a second request engine.
There is no deletion or live credential migration in this substrate.
Custom backends that do not implement it fail with `LocalAccountStorageUnsupported`, never fall back to a replicated store.

InMemory JSON revision `3` persists the map; it reads supported older revisions without inventing local records.
SQL revision `4` adds an empty native table when opening supported earlier formats, retaining existing Entries, metadata/secrets, verification and Store-state data.
Opening v3 does not redo the v2 cache backfill.
Missing credential storage under a v4 marker is corruption and refuses open rather than silently recreating an empty directory.
Older binaries refuse the new format counters instead of silently dropping credentials.
These are tested synthetic storage transitions, not permission to open/migrate live installations.
A binary rollback does not roll back data; see the [format matrix](../design/verification.md#persistence-restart-and-forward-migration).

## Bundle format and key lifetimes

The v1 encrypted bundle's logical JSON payload has only `account_id` and an explicit ordered, nonempty, unique list of private keys.
The external storage envelope contains `version`, `salt`, `nonce` and `ciphertext`.
Version 1 fixes AES-256-GCM, a random 96-bit nonce per sealing operation, and the existing Account Argon2id version 0x13 parameters: 19456 KiB, two iterations, parallelism one, 32-byte output.
It retains the Account crypto helper's base64-text salt convention: the generated 22-character salt text, not the decoded 16 bytes, enters Argon2.
The fixed format accepts no caller-supplied work-factor parameters.
PasswordStore's KDF envelopes/upgrades remain independent; this API does not migrate them or claim their upgrade policy.

The complete locator and key list are inside the authenticated ciphertext.
The AEAD associated data is the fixed domain/version `eidetica:local-credential-bundle:v1`.
`unlock_for` additionally compares the authenticated locator to the expected Account ID, so copying a directory hint or encrypted blob cannot rebind it to another Account.
Unknown versions/fields, malformed salts/nonces, wrong passwords, tampering and invalid key lists fail closed.
Decrypted parser errors use fixed text instead of echoing keys.

Serialization measures the payload before writing into a guarded, preallocated plaintext buffer.
The Account key manager calls its existing zeroizing cleanup on drop, including constructor errors after allocation.
The completed bundle plaintext buffer and wrapping key use zeroizing owners on success/error/unwind; decrypted JSON is guarded immediately after decryption and released after parsing.
The borrowed password remains caller-owned and is not retained.
`UnlockedCredentials` retains private keys and the wrapping key for explicit resealing, not the password.
Its `signer` method returns an independently owned private-key clone.
There is no plaintext `Debug`, `Clone` or serialization interface on `UnlockedCredentials`.

Dropping an unlock/session does not destroy independent clones, invalidate existing database handles or revoke Auth.
Signing keys use their existing zeroizing owner, but parser/serde/allocator/crypto internal copies, caller-owned passwords, swap, core dumps and a compromised process are outside this cleanup claim.
Password rewrap reseals the same keys; older bundles/backups remain usable under their old password.
Recovery/export, live revocation and background-device authority are separate work.

## Embedded execution

```text
caller-owned keys + Account ID -> CredentialBundle::seal -> encrypted local bytes
                                        |
client password + encrypted bytes -> unlock -> AccountSigner (actual key + SigKey)
                                                |
                            open_database -> normal Auth/verified proof -> signed Entry
```

A direct-key handle needs no Identity.
A delegated handle carries the exact supplied path, not a reference-derived guess or an automatic direct-key fallback.
A newly selected handle is a new value; old handles are never rebound.
A missing public-key hint is bound to the actual signer so name-based service reads cannot fall back to the connection login key; a mismatched supplied public-key hint fails open-time validation.
Identity code can supply an exact `DatabaseKey`/`SigKey` through this seam without storing local key selection or activation in portable metadata.
The Identity membership request lifecycle is not implemented here.

## Service execution

```text
client: encrypted bytes -> unlock -> private signer (remains in client)
                                      |
                           SignerChallenge { pubkey }
                                      v
                     daemon: fresh proof challenge only
                                      |
                         TrustedLoginProve { signature }
                                      v
                     session public-key set + cache scope
                                      |
               per-tree Auth gate / signed-entry verification -> storage
```

`AccountSigner::connect` creates a fresh connection, proves possession and receives no login credentials.
The proof-only path uses the existing challenge verifier, single-use challenges and per-tree authorization; it creates no daemon login record.
Its generated `signer:<UUID>` cache scope is distinct from registered-login UUIDs and is not a credential-storage identity.
A failed proof consumes its challenge and does not authenticate.
Older v0 peers may return Unsupported or disconnect for the new optional operation; there is no credential-login fallback.
Additional actual keys still require existing session-key possession proofs.
Reauthentication replaces the client's proof cache to match the daemon's reset keyset.

Connected delegated transactions sign on the client, then rely on the daemon's local proof resolver.
Submit stores untrusted input for verification, but a successful commit acknowledgment requires that exact Entry to become Verified.
An incomplete proof or rejection is an error, not rollback: stored unverified content can remain for later verification.
This reuses the retained Identity validation seam rather than adding an Identity-specific authorization protocol.

The socket remains a filesystem-trusted local API.
This does not protect login records from hostile socket callers or shared-file processes.
The optional dashboard still unlocks signing keys in daemon memory for its existing web sessions; extracting it into a socket client is not part of this implementation.
No stronger live revocation, private-key recovery or isolation guarantee follows from this split.

## Regression map

- `credential_bundle_tests`: explicit scope, wrong/missing signer, cross-Account locator mismatch, password/tamper/format refusal, rewrap and retained signers.
- `signer_tests`: actual direct and delegated signatures, retained/new handles, wrong-path no-fallback, unauthorized access and real Unix-socket proof-only use without bundle login.
- `local_accounts`: compare/exchange conflicts, racing inserts, separate records, persistence/reopen and absence from Entry history/tree enumeration; cache clearing/trust reset preserve records.
- SQL schema tests: synthetic v3→v4 retention without backfill and fail-closed missing v4 storage.
- Service signer proof and signed-submit tests: wrong proof, consumed challenges, valid fresh proof and propagation of actual verification refusal.

These do not yet prove the normal login path secret-free or two-installation activation independent; those require integration of the native substrate into the existing session writers.
