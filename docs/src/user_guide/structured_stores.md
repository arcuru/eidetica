# Structured Stores

`StructuredStore<T>` is an opt-in whole-state Store for applications that supply a Serde value, its CRDT merge, and one exact persisted Type through `StructuredType`.
Existing `DocStore`, `Table`, and their persisted formats are unchanged.

```rust,no_run
use eidetica::{Database, Result, Store, crdt::{Doc, Lww}};
use eidetica::store::{StructuredStore, schema::StructuredType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Message {
    body: String,
}

struct Messages;
impl StructuredType for Messages {
    const TYPE_ID: &'static str = "example/messages:v0.1";
    type Data = Lww<Message>;
    fn grammar(_configs: &[Doc]) -> Result<Doc> {
        Ok(Doc::new()) // this Type does not use its config
    }
}

# async fn example(db: Database) -> Result<()> {
let tx = db.new_transaction().await?;
let messages = StructuredStore::<Messages>::open(&tx, "messages").await?;
messages.apply(Lww::Set(Message { body: "Hello".into() })).await?;
let id = tx.commit().await?;

let tx = db.new_transaction().await?;
let found = tx.inspect_contribution("messages", &id).await?.unwrap();
assert_eq!(found.type_id, "example/messages:v0.1");
# Ok(())
# }
```

## A contribution keeps the Type its writer saw

The Type string is the only version discriminator. A contribution is the Type's native value, with no tag, schema name, or other common framing.
Its Type is the `type` in the Store's `_index` row in **that Entry's own verified main history, including a declaration in the Entry itself**.
Changing a declaration later does not change an earlier contribution, so use a different string for an incompatible grammar or merge.

A writer emits a value of the Type it observes, or the commit fails without publishing anything else it staged.
The check uses the transaction's actual parents and final staged row, so the order of API calls does not matter:

- An offline writer that has only seen `contacts:v0.1` can keep writing it on its own branch. Those values stay `v0.1` after a concurrent change to `v0.2`.
- A `v0.1` writer that has seen `v0.2` is refused. It cannot store its value under the new declaration.

`StructuredStore::declare` states a Type and its complete config from the current Entry on. It adds no value and converts nothing.
Use it to change a Type, or to settle conflicting declarations. Values staged under the previous Type in the same transaction make the commit fail.

If concurrent declarations disagree, or a removal races a declaration, there is no Type. Nothing picks a winner by height, ID, or arrival.
A new contribution there needs an explicit declaration in the same Entry or an earlier one that descends from the conflict. That choice does not repair a contribution already made at the conflict.
`Registry::set_subtree_settings` also refuses such a row, because saving the displayed row would choose one.

## Inspecting and reading

`Transaction::inspect_contribution` reports the Type of one contribution and the Entries that declared it, without decoding the value or having code for that Type.
`UnresolvedType` means the complete history gives no unique Type. `HistoryUnavailable` means the history could not be checked, which is not evidence that no Type exists.

Reading is stricter. It requires every contribution in the Store history it consumes to be of the reader's exact Type, and refuses the whole view otherwise. It does not skip contributions, convert them, or guess from their bytes.
Both peers' branches remain readable on their own. A mixed view needs a Type that supports both, which this Store does not provide.
Ordinary `open` also checks the registered Type, so use `get_store_viewer` or a transaction at an earlier snapshot to read values whose Store has since changed Type.

An Entry with no value for the Store is not a contribution, even if it changes the declaration. An empty view reports `has_source() == false`.
An explicitly written empty value or `Lww::NoOp` is a real contribution. A zero-length payload is malformed for these Types.

## Config belongs to the Type

Eidetica carries a Store's config but does not interpret it. `StructuredType::grammar` receives the config in the history of each contribution and returns whatever distinguishes its row grammar or merge parameters, or an error.
Values with different results are never combined, even under one Type. Return an empty Doc if the config does not matter.
Concurrent config changes are passed as separate values rather than a merged guess, so a Type that cannot resolve them can refuse the combined view.
If a value is staged before its Type is declared, it is taken as encoded for the default config, and the commit fails if the declared config means something else. Declare first when the config matters.
`set_config` changes only the config. It does not declare a Type, and commit refuses the edit if the final Type is not the one whose handle made it.

## Value encoding and compatibility

Values are canonical DAG-CBOR structural Serde data. Nested `Map`, `Lww`, and application values do not need their own byte wrappers or version headers.
Use stable field and variant names and preserve all state needed by later merges. Canonical encoding covers admitted wire values, not arbitrary histories that happen to produce the same application result.

The shared [DAG-CBOR domain](../internal/crdt.md) includes full `i64` and `u64` values. Use `serde_bytes` for byte strings.
`schema::u128_bytes` and `schema::i128_bytes` encode full-width integers as exactly 16 big-endian bytes; `schema::KeyedPairs` encodes non-text-keyed maps as sorted pairs and rejects duplicate encoded keys.

Use the original reader for legacy Stores. Declaring a new Type over one does not adopt its earlier values.
Older software can retain and relay the opaque Entry. These checks are in the typed API and do not change how Entries are verified or synced, so they do not prove that another writer's payload is well formed.

## PasswordStore and cache

For `PasswordStore<StructuredStore<T>>`, initialize or unlock as usual and use `inner()` for typed reads and writes.
Public history shows only the wrapper's Type. `PasswordStore::inspect_contribution` on an unlocked Store reports the inner Type from the protected metadata in that contribution's own history, and `declare_inner` changes the inner Type and config.
If the historical metadata cannot be opened with the unlocked key, it is unavailable. The current inner Type is not used instead. This path does not rotate keys.

Encrypted metadata is padded, so its size does not reveal the length of an inner Type name. This does not conceal whether an Entry has a Store value or a metadata update, the size of the application value, larger configs, or the existing public Store name and DAG shape.

Reads check the captured, verified history before using a cached value. The cache can skip the application fold but not that check; this is a whole-state API, not a bounded row lookup.
The service relays Entries and stores opaque cache bytes under its existing login boundary; it does not decrypt or execute application code.
