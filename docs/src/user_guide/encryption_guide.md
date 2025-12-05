# Encryption Guide

`PasswordStore` provides transparent password-based encryption for any Store type.

## Quick Start

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::{PasswordStore, DocStore, Store}};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set("name", "secrets_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key)?;
// Create and initialize an encrypted store
let tx = database.new_transaction()?;
let mut encrypted = tx.get_store::<PasswordStore>("secrets")?;
encrypted.initialize("my_password", DocStore::type_id(), "{}")?;

// Use the wrapped store normally
let docstore = encrypted.unwrap::<DocStore>()?;
docstore.set("api_key", "sk-secret-12345")?;
tx.commit()?;
# Ok(())
# }
```

## Opening Existing Stores

```rust
# extern crate eidetica;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::{PasswordStore, DocStore, Store}};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set("name", "secrets_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key)?;
# {
#     let tx = database.new_transaction()?;
#     let mut encrypted = tx.get_store::<PasswordStore>("secrets")?;
#     encrypted.initialize("my_password", DocStore::type_id(), "{}")?;
#     let docstore = encrypted.unwrap::<DocStore>()?;
#     docstore.set("secret", "value")?;
#     tx.commit()?;
# }
// Use open() for existing stores instead of initialize()
let tx = database.new_transaction()?;
let mut encrypted = tx.get_store::<PasswordStore>("secrets")?;
encrypted.open("my_password")?;

let docstore = encrypted.unwrap::<DocStore>()?;
let _secret = docstore.get("secret")?;
tx.commit()?;
# Ok(())
# }
```

## Wrapping Other Store Types

PasswordStore wraps any store type. Use `Store::type_id()` to get the type identifier:

```rust
# extern crate eidetica;
# extern crate serde;
# use eidetica::{Instance, backend::database::InMemory, crdt::Doc, store::{PasswordStore, Table, Store}};
# use serde::{Serialize, Deserialize};
#
# fn main() -> eidetica::Result<()> {
# let backend = Box::new(InMemory::new());
# let instance = Instance::open(backend)?;
# instance.create_user("alice", None)?;
# let mut user = instance.login_user("alice", None)?;
# let mut settings = Doc::new();
# settings.set("name", "creds_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key)?;
#[derive(Serialize, Deserialize, Clone)]
struct Credential {
    service: String,
    password: String,
}

let tx = database.new_transaction()?;
let mut encrypted = tx.get_store::<PasswordStore>("credentials")?;
encrypted.initialize("vault_password", Table::<Credential>::type_id(), "{}")?;

let table = encrypted.unwrap::<Table<Credential>>()?;
table.insert(Credential {
    service: "github.com".to_string(),
    password: "secret_token".to_string(),
})?;
tx.commit()?;
# Ok(())
# }
```

## Security Notes

- **No recovery**: Lost password = lost data (by design)
- **Encryption**: AES-256-GCM with Argon2id key derivation
- **Relay-safe**: Encrypted data can sync through untrusted relays

## See Also

- [PasswordStore API](../rustdoc/eidetica/store/struct.PasswordStore.html) - Full API documentation
- [Stores](concepts/stores.md) - Overview of all store types
