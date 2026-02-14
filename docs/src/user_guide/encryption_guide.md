# Encryption Guide

`PasswordStore` provides transparent password-based encryption for any Store type.

## Quick Start

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, Registered, backend::database::Sqlite, crdt::Doc, store::{PasswordStore, DocStore}};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "secrets_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
// Create and initialize an encrypted store
let tx = database.new_transaction().await?;
let mut encrypted = tx.get_store::<PasswordStore>("secrets").await?;
encrypted.initialize("my_password", DocStore::type_id(), Doc::new()).await?;

// Use the wrapped store normally
let docstore = encrypted.unwrap::<DocStore>().await?;
docstore.set("api_key", "sk-secret-12345").await?;
tx.commit().await?;
# Ok(())
# }
```

## Opening Existing Stores

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, Registered, backend::database::Sqlite, crdt::Doc, store::{PasswordStore, DocStore}};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
# let mut settings = Doc::new();
# settings.set("name", "secrets_db");
# let default_key = user.get_default_key()?;
# let database = user.create_database(settings, &default_key).await?;
# {
#     let tx = database.new_transaction().await?;
#     let mut encrypted = tx.get_store::<PasswordStore>("secrets").await?;
#     encrypted.initialize("my_password", DocStore::type_id(), Doc::new()).await?;
#     let docstore = encrypted.unwrap::<DocStore>().await?;
#     docstore.set("secret", "value").await?;
#     tx.commit().await?;
# }
// Use open() for existing stores instead of initialize()
let tx = database.new_transaction().await?;
let mut encrypted = tx.get_store::<PasswordStore>("secrets").await?;
encrypted.open("my_password")?;

let docstore = encrypted.unwrap::<DocStore>().await?;
let _secret = docstore.get("secret").await?;
tx.commit().await?;
# Ok(())
# }
```

## Wrapping Other Store Types

PasswordStore wraps any store type. Use `Registered::type_id()` to get the type identifier:

```rust
# extern crate eidetica;
# extern crate tokio;
# extern crate serde;
# use eidetica::{Instance, Registered, backend::database::Sqlite, crdt::Doc, store::{PasswordStore, Table}};
# use serde::{Serialize, Deserialize};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let backend = Box::new(Sqlite::in_memory().await?);
# let instance = Instance::open(backend).await?;
# instance.create_user("alice", None).await?;
# let mut user = instance.login_user("alice", None).await?;
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
let mut encrypted = tx.get_store::<PasswordStore>("credentials").await?;
encrypted.initialize("vault_password", Table::<Credential>::type_id(), Doc::new()).await?;

let table = encrypted.unwrap::<Table<Credential>>().await?;
table.insert(Credential {
    service: "github.com".to_string(),
    password: "secret_token".to_string(),
}).await?;
tx.commit().await?;
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
