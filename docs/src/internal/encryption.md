# Encryption

This chapter describes `PasswordStore` as implemented. It covers content encryption and encrypted
derived Store-state caches. User signing-key encryption is a separate use of Argon2id and
AES-256-GCM in `user/crypto.rs`; its salt handling, envelope, and lifetime are not the
`PasswordStore` format described here.

The main implementation is in `crates/lib/src/store/password_store.rs`. Transaction integration is
in `crates/lib/src/transaction/mod.rs`, and the Store-state substrate is in
`crates/lib/src/store/state.rs` plus `crates/lib/src/backend/`.

## Boundaries and ownership

`PasswordStore<S>` owns the public encryption configuration, encrypted wrapped-Store metadata, and
the locked/unlocked state. Opening or initializing it creates a `PasswordEncryptor` and registers
that encryptor with the current `Transaction` under the Store name.

`Transaction` owns the data-path boundary:

- it keeps staged Store deltas as plaintext;
- it decrypts historical Entry payloads before CRDT deserialization and merge;
- it encrypts a Store delta immediately before the Entry is built and persisted;
- for record projections, it maps logical keys to physical keys and encrypts/decrypts row values;
- it builds derived state from the immutable Entry history on cache misses.

The backend receives opaque Entry payload bytes and opaque Store-state record keys and values. It
compares and orders record keys as bytes and does not interpret the encrypted envelope. In service
mode the client performs password derivation, Entry decryption, encrypted-state materialization,
and row verification. The daemon stores Entries and user-scoped encrypted cache generations but
does not receive the Store password or plaintext content.

Plaintext therefore exists in the client process: wrapped Store configuration after `open`, staged
deltas, decrypted historical deltas, merged CRDT state, logical row keys and values, and values
returned to application code. "Encrypted at rest" does not mean plaintext never exists in memory.

## Password derivation

Initialization generates a `SaltString` with `SaltString::generate`. The value stored in `_index`
is its unpadded base64 text; for the generated v0 format it represents 16 random salt bytes.

The master key is 32 bytes produced by Argon2id version 0x13 with parameters stored in the config:

| Parameter     | v0 initialization value |
| ------------- | ----------------------- |
| memory        | 19 × 1024 KiB           |
| time cost     | 2                       |
| parallelism   | 1                       |
| output length | 32 bytes                |

A compatibility detail is important: the implementation parses the stored text with
`SaltString::from_b64`, then passes `salt.as_str().as_bytes()` to `hash_password_into`. The Argon2
salt input is therefore the bytes of the base64 salt string, not the decoded 16 random bytes.
Changing that would derive a different key and make existing Stores unreadable.

Older configuration without explicit cost fields uses the same defaults. `open` rejects unsupported
algorithm and KDF names, invalid salts and parameters, wrong-size config nonces, failed config
authentication, malformed decrypted metadata, and a wrapped Store type that does not match `S`.

`PasswordEncryptor` lazily derives the master key on first data operation and retains it behind the
transaction's encryptor. `Password`, the cached master-key buffer, and temporary row subkeys use
zeroizing owners. This is best-effort cleanup of those owned buffers, not a guarantee that every
copy or plaintext is erased from process memory.

## Four persistent representations

PasswordStore uses related cryptographic primitives but different byte formats for configuration,
Entries, opaque cached state, and projected row caches.

### `_index` configuration

The `_index` registry entry has type `encrypted:password:v0`. Its configuration is an atomic `Doc`
with this logical shape:

```text
{
  encryption: {
    algorithm: "aes-256-gcm",
    kdf: "argon2id",
    salt: <base64 text>,
    version: "v0",
    argon2_m_cost: 19456,
    argon2_t_cost: 2,
    argon2_p_cost: 1
  },
  wrapped_config: {
    ciphertext: <base64 text>,
    nonce: <base64 text>
  }
}
```

Algorithm, KDF, salt, version, and costs are public metadata. `wrapped_config` encrypts a JSON
serialization of `WrappedStoreInfo { type, config }` with the master AES-256-GCM key and a random
12-byte nonce. The nonce and ciphertext are separate fields and are base64-encoded only because
they live inside a `Doc`. This encryption call supplies no additional authenticated data.

### Entry payloads

At commit, the wrapped Store's serialized delta is encrypted with the master AES-256-GCM key and a
fresh random 12-byte nonce:

```text
nonce[12] || AES-GCM ciphertext-and-tag
```

These are raw subtree payload bytes inside `Entry`; they are not JSON or base64 wrappers. The
plaintext format belongs to the wrapped Store (for example, JSON for `Doc`-backed Stores or a Yrs
binary update). This encryption call also supplies no additional authenticated data. Entry
identity, signatures, parents, Store name, and database membership are protected by the Entry/DAG
authentication layers, not by row-envelope AAD.

Opaque derived cached state uses the same `Encryptor::encrypt` format. The merged CRDT state is
serialized to JSON, encrypted as `nonce || ciphertext-and-tag`, and stored at reserved key `0x00`.
`DocStore`, `YDoc`, and any other Store that keeps the default opaque model use this path. For a
binary CRDT such as YDoc, this cache plaintext is the CRDT state's serde JSON representation; it is
not necessarily the same bytes as an individual Entry delta.

### Projected Table row cache

`Table` declares projection `eidetica/table/rows/canonical-json:v0`, version 0. `PasswordStore<Table<T>>` namespaces
that descriptor as `eidetica/password/eidetica/table/rows/canonical-json:v0`, version 0. The encrypted projection is
built after decrypting Entry deltas and applying the Table projection.

Two 32-byte subkeys are derived from the master with BLAKE3's derive-key mode:

```text
record-key subkey   = derive_key("eidetica/password-store/record-key/v1", master)
record-value subkey = derive_key("eidetica/password-store/record-value/v1", master)
```

The physical key is BLAKE3 keyed-hash output:

```text
physical_key = keyed_hash(record-key subkey, exact UTF-8 logical key)  // 32 bytes
```

The value plaintext is a serde JSON byte serialization of an envelope containing byte arrays:

```text
EncryptedRecordEnvelope { key: logical_key_bytes, value: row_json_bytes }
```

Because `key` and `value` are Rust byte slices, serde JSON represents each as an array of integer
bytes. The outer persisted value is not JSON:

```text
nonce[12] || AES-GCM(record-value subkey, envelope_json, AAD)
```

The AAD is the exact concatenation:

```text
"eidetica/password-store/record-envelope/v1\0"
|| database_id_text_bytes || 0x00 || store_name_utf8
|| 0x00 || physical_key
```

`database_id_text_bytes` comes from `database_id.to_string()`. The Store identity separator and AAD
separator make the database/Store/key placement unambiguous for the current format.

On decryption, AES-GCM authenticates the envelope against that AAD. The client then deserializes the
envelope and recomputes the physical key from its logical key. Authentication failure is reported
as `StoreError::DataCorruption("record authentication failed")`; a successfully decrypted envelope
whose logical key hashes elsewhere is `DataCorruption("record envelope key does not match its
physical key")`. Too-short values and malformed JSON fail as deserialization errors.

The tests maintainers should start with are:

- unit tests at the end of `crates/lib/src/store/password_store.rs` for domain separation, nonce
  uniqueness, Store/physical-key binding, malformed values, and modification;
- `crates/lib/tests/it/store/password_store.rs` for encrypted Entries, opaque caches, Table row
  records, physical ordering, staged updates/deletes, and cache clearing;
- `crates/lib/tests/it/transaction/record_fallback.rs` for cache/history page and cursor equivalence;
- `crates/lib/tests/it/store/ydoc_operations.rs` for the opaque encrypted YDoc path;
- `crates/lib/tests/it/service.rs` for warm encrypted Table point reads over the service.

## Row semantics and leakage

For an unencrypted Table, a cache record key is the exact UTF-8 logical key and its value is
the JSON row bytes. For an encrypted Table, neither is stored in plaintext. The physical key is
stable for the same master key and exact UTF-8 logical key, so equality and access patterns remain
visible. The physical key also defines scan and cursor order; it intentionally does not preserve
logical-key sorting or range locality.

Within a transaction, logical mutations are the source for conflict/shadow rules and the physical
mutation map is the encrypted overlay. `get` checks staged logical state first and otherwise fetches
the one keyed record. Scans merge staged encrypted records with cached records in physical-key
order, decrypt returned records, and expose logical rows with an exclusive physical cursor. The
history fallback projects the full plaintext state, applies logical overlays, maps every remaining
row to its physical key, then filters and pages in the same physical order.

A same-key update replaces that key in the transaction's logical projection and in the next
materialized generation. It does not overwrite a record in an already published generation, and it
does not remove old encrypted Entry payloads or old historical state. Deletion is likewise a CRDT
delta/tombstone, not secure erasure of history.

## Generation lifecycle

A `StoreStateRequest` names a generation target by database, Store, lifecycle, scope, projection
descriptor, and source key. Historical state uses lifecycle `Derived`; its source key is the fixed
Store tips (a single Entry ID or the deterministic merge-tip cache ID).

A miss follows this sequence:

1. Begin a private `Staging` namespace.
2. Reconstruct and project verified Entry history in the client for an encrypted Store.
3. Write encrypted records into that private namespace.
4. Publish atomically as `Derived`.
5. If another builder already published the same target, adopt the winner and discard the private
   loser.

Published record sets are immutable. A transaction may keep an immutable `RecordView`, but it never
patches the published records during `set`, `update`, or `delete`. New Entry tips form a different
source key and therefore resolve or build a different generation.

Clearing derived state unlinks every published derived generation and reclaims the generation
unlinked by the previous clear. A reader that already resolved a view may finish against the
unlinked generation until the next clear; a new resolve misses and rebuilds from Entries. A view
that becomes invalid during a read is resolved once more before history reconstruction. Unsupported
custom backends fall back directly to history. Other storage or authentication failures propagate;
they are not treated as benign cache misses.

Over the service, encrypted materializations are client-computed and rebound to
`CacheScope::User(session_user)`. They cannot be published into shared scope or another user's
scope. Warm reads use record get/scan operations over opaque views; a warm point read need not scan
the row set or fetch Store history.

## Security properties and non-properties

PasswordStore provides confidentiality and ciphertext integrity under the password-derived key.
For projected rows, AAD and the inner logical-key check bind a valid envelope to the database,
Store, and physical key where the client expects it.

Those properties do **not** make a derived cache authoritative:

- A valid row proves that its envelope was produced under the key for that placement. It does not
  prove that the generation is the newest one.
- Authentication of individual rows does not prove that the backend returned every row or the
  correct page boundary.
- A client-computed encrypted cache cannot be semantically verified by the daemon because the daemon
  lacks plaintext and the password.
- Cache deletion, omission, or corruption can cause an error or force reconstruction; the signed,
  verified Entry DAG is the recoverable source of truth.
- PasswordStore does not conceal Entry/DAG topology, ciphertext lengths, operation timing, record
  counts, page sizes, or stable-key equality/access patterns.
- PasswordStore does not provide rollback detection, secure deletion of immutable history, password
  recovery, or password rotation.

Peer sync is unchanged: peers store and exchange immutable Entries and their authentication data.
Derived Store-state records are local/service cache data, not Entry or sync protocol data.
