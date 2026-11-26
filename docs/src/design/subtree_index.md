# Subtree Index (\_index)

`_index` is a DocStore in the Database's history. It records how to open a named Store. It is not a backend index of every Entry that mentions the name, nor is it the Database's [authority configuration](settings_storage.md#entry-metadata).

## From genesis to the first registration

`Database::create_with_init` stages `_settings` and `_root` before running the initializer. These two names and `_index` bypass auto-registration; the registry does not register itself. Opening a user Store through `Transaction::get_store` can register it during the initializer or in a later transaction. A settings-only rename needs no registration.

`get_store` initializes the named Store's parents at the transaction's main snapshot, then reads its registration:

- **Missing named key in `_index`:** call `Store::register`. The default loads the Store and calls `Registry::set_entry` with its type ID and default config.
- **Existing registration:** check `Registered::supports_type_id` and load the Store only if compatible. Reading a type ID does not migrate data.
- **Malformed registration or another read error:** return the error, not a fresh registration. Use `get_entry` when the distinction matters; `contains` reports only whether `get_entry` succeeded.

Each named key holds a nested `Doc`: `type` is a text identifier such as `docstore:v0`, `config` is the Store's configuration `Doc`, and optional `settings` carries the [height override](height_strategy.md#what-is-stored). For example, `PasswordStoreConfig` is an atomic `Doc` in `config`: its fields are replaced together. It belongs in the registration, not in the database-wide `_settings`. `Registry::get_entry` returns these as typed fields. When reading config, missing or non-Doc, non-text values default to an empty `Doc`; legacy text `"{}"` also means empty, but other text is an error. `Registry::list` reads the keys of the materialized index rather than scanning all Entry subtrees.

## Metadata travels with data

An index write names the Stores whose registrations it changes. Before committing, the transaction includes each of those subtrees, using its parents at the chosen main boundary if it was not already in the Entry builder. Even a registration with **no Store payload** participates in that Store's history. The Entry carries `_index` data and a named subtree whose `data` is `None`. This is not an encoded tombstone; an unreferenced, untouched Store need not appear at all.

For example, pre-register a DocStore without writing any documents, then open it in a second transaction:

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, NewAccount, backend::database::Sqlite, crdt::Doc, store::DocStore};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (_instance, mut user) = Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?), NewAccount::passwordless("alice"),
# ).await?;
# let key = user.get_default_key()?;
# let db = user.create_database(Doc::new(), &key).await?;
let txn = db.new_transaction().await?;
let index = txn.get_index().await?;
index.set_entry("notes", "docstore:v0", Doc::new()).await?;
let registration = txn.commit().await?;
let entry = db.get_entry(registration.clone()).await?;
assert!(entry.data("_index").is_ok());
assert!(entry.subtree_parents("notes")?.is_empty()); // present, with no parents
assert!(entry.data("notes").is_err()); // no payload

let txn = db.new_transaction().await?;
assert_eq!(txn.get_index().await?.get_entry("notes").await?.type_id, "docstore:v0");
let notes: DocStore = txn.get_store("notes").await?;
notes.set("title", "First note").await?;
let write = db.get_entry(txn.commit().await?).await?;
assert_eq!(write.subtree_parents("notes")?, vec![registration]);
# Ok(())
# }
```

The registration delta and the Store node are persisted in **one signed Entry**. The current registration and the Store's current tips are derived at a snapshot. Main parents record what the writer observed; ordinary Store parents select what it consumed. A Store branch can therefore omit an observed registration-bearing Entry from its own parent chain. The coupling above is an authoring rule, not a promise that a single Store chain contains all registry metadata.

Sync transfers whole Entries. Verification covers the Entry, not just the `_index` payload: promotion requires complete causal ancestry and the corresponding historical authority evidence. A type ID or a matching content hash is not that evidence. See [Store-parent frontiers](verification.md#store-parent-frontiers) for the implemented checks and [selective sync](synchronization.md#selective-sync-design-direction) for the proposed, not shipped, Store-only use case.
