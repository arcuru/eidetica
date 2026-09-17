# Encryption Guide

`PasswordStore<S>` adds password-based encryption to a Store. It protects Store configuration,
Entry payloads, and derived cached state while preserving the wrapped Store's API.

This is separate from password protection for a user's signing key. Logging in unlocks a signing
key; it does not unlock a `PasswordStore`. Each encrypted Store has its own password and salt and
must be opened in each transaction that uses it.

## Quick start

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::{PasswordStore, DocStore}};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let (instance, mut user) = eidetica::Instance::create_backend(
#     backend,
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "secrets_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
let tx = database.new_transaction().await?;
let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
encrypted.initialize("my_password", Doc::new()).await?;

let docstore = encrypted.inner().await?;
docstore.set("api_key", "sk-secret-12345").await?;
tx.commit().await?;
# Ok(())
# }
```

`initialize` is for a new Store. It records the public encryption parameters and an encrypted copy
of the wrapped Store's type and configuration. The transaction is then unlocked and can return the
inner Store.

## Opening an existing Store

Open the wrapper before asking for the inner Store:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::{PasswordStore, DocStore}};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let (instance, mut user) = eidetica::Instance::create_backend(
#     backend,
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "secrets_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# {
#     let tx = database.new_transaction().await?;
#     let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
#     encrypted.initialize("my_password", Doc::new()).await?;
#     let docstore = encrypted.inner().await?;
#     docstore.set("secret", "value").await?;
#     tx.commit().await?;
# }
let tx = database.new_transaction().await?;
let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
encrypted.open("my_password")?;

let docstore = encrypted.inner().await?;
let _secret = docstore.get("secret").await?;
tx.commit().await?;
# Ok(())
# }
```

A wrong password cannot decrypt the wrapped Store metadata, so `open` fails before the inner Store
is available. There is no recovery key or password-reset operation. Losing the password loses
access to the Store; back up the password separately from the database.

The password and derived key remain in the client process for the lifetime of the opened
transaction. Their owning buffers are cleared when dropped, but applications should not assume
that every temporary, allocator copy, serialized result, or plaintext value is erased from memory.
Keep transactions short and avoid logging passwords or decrypted values.

## Choosing a wrapped Store

`PasswordStore<S>` can wrap any Store type. Its persistent layout depends on the wrapped Store's
state model.

### `DocStore` and other opaque Stores

Opaque Stores cache one encrypted whole-state value. Reads still return the normal `DocStore`
values, but a cold read may decrypt and merge the Store's history before publishing that encrypted
cache value.

```text
caller                         backend
"api_key" -> "secret"         reserved cache key 0x00 -> nonce || ciphertext
```

The backend does not receive the plaintext document through the Store-state cache.

### `Table<T>` row caches

`Table<T>` supports individually addressable cached rows. A password-wrapped Table transforms both
parts of each cached row:

```text
logical row                         derived cache record
"account-42" -> JSON row     =>    keyed_hash("account-42") -> nonce || ciphertext
```

The ciphertext contains both the logical key and the row bytes. This lets the client verify that a
record was returned from the physical key where it belongs before returning plaintext to the
Table.

Point reads compute the 32-byte physical key and fetch one encrypted record. `set`, `insert`,
`update`, and `delete` operate on logical keys inside the transaction; commit writes the normal
encrypted Entry delta. They do not edit a published cache generation. A later read at the new tips
uses another derived generation, built from immutable Entries.

Scans and `search` use physical-key order. This order is deterministic for one encrypted Store but
is not lexicographic primary-key order. `TablePage::next` is an exclusive cursor in that physical
order. Treat it as opaque: pass it unchanged to the next `scan_page` call for the same Store and
password. The cached path and the history fallback use the same physical order, so a continuation
can cross between them without changing cursor domains.

### Other Store types

A wrapped Store that does not define a row projection uses the opaque whole-state format. For
example, `PasswordStore<YDoc>` does not expose YDoc fields as cached rows. Wrapping a type does not
invent a record projection for it.

## Service mode and synchronization

With a connected `Instance`, password derivation, decryption, CRDT merging for encrypted content,
and row-envelope verification run in the client. Entry payloads and encrypted cache records cross
the service socket as ciphertext. The daemon can serve a warm encrypted Table point read with
point-record requests rather than a row scan or Store-history reconstruction.

Encrypted cache materializations uploaded by a client are scoped to that authenticated user. The
daemon cannot validate their decrypted contents. The signed, content-addressed Entry DAG remains
the source of truth, and peer synchronization still exchanges Entries rather than treating the
cache as authoritative data.

## What encryption hides and reveals

`PasswordStore` uses Argon2id and AES-256-GCM. It prevents a backend or relay without the password
from reading wrapped configuration, Entry payloads, opaque cached state, Table logical keys, or
Table row values.

It does not hide all metadata. A backend can observe:

- the database and Store receiving an operation;
- Entry graph structure, timing, and ciphertext sizes;
- cached record counts, access patterns, and page sizes;
- equality of repeated Table logical keys within and across cache generations for the same Store,
  because their keyed physical keys are stable;
- the public encryption algorithm, KDF parameters, salt, and format version in `_index`.

Authenticated encryption detects modification or relocation of a cached row between physical keys
or Stores. It does not prove that a cache is fresh or complete, or that the backend returned every
row. Derived cached state is disposable; when its correctness is in doubt, clear it and rebuild it
from verified Entries.

## Example: encrypted Table

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{Instance, backend::database::Sqlite, crdt::Doc, store::{PasswordStore, Table}};
# use serde::{Serialize, Deserialize};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let (instance, mut user) = eidetica::Instance::create_backend(
#     backend,
#     eidetica::NewUser::passwordless("alice"),
# ).await?;
# let mut settings = Doc::new();
# settings.set("name", "creds_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
#[derive(Serialize, Deserialize, Clone)]
struct Credential {
    service: String,
    password: String,
}

let tx = database.new_transaction().await?;
let mut encrypted = tx.get_store::<PasswordStore<Table<Credential>>>("credentials").await?;
encrypted.initialize("vault_password", Doc::new()).await?;

let table = encrypted.inner().await?;
table.insert(Credential {
    service: "github.com".to_string(),
    password: "secret_token".to_string(),
}).await?;
tx.commit().await?;
# Ok(())
# }
```

## See also

- [PasswordStore API](../rustdoc/eidetica/store/struct.PasswordStore.html) — API reference
- [Stores](concepts/stores.md) — Store types and transaction usage
- [Service mode](service.md) — local daemon trust boundary
- [Encryption internals](../internal/encryption.md) — formats, derivation, and security boundaries
