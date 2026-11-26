# Settings Storage Design

Historical databases store replicated settings in the `_settings` Store (`constants::SETTINGS`). It holds a `Doc` CRDT: the database name is stored as `name`, and authentication configuration is stored as `auth`. These settings travel with Entries; they are distinct from an installation's local [sharing preferences](../user_guide/database_management.md).

## Create, rename, and read

Pass the initial settings as a `Doc` when creating the database. After creation, use `Transaction::get_settings()` to get a `SettingsStore`, a typed wrapper over `DocStore`. Getting the handle is synchronous; reading and staging settings through it are async.

```rust
# extern crate eidetica;
# extern crate tokio;
# use eidetica::{Instance, NewAccount, backend::database::Sqlite, crdt::Doc};
#
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (_instance, mut user) = Instance::create_backend(
#     Box::new(Sqlite::in_memory().await?),
#     NewAccount::passwordless("alice"),
# ).await?;
let mut initial_settings = Doc::new();
initial_settings.set("name", "Draft Notes");
let key = user.get_default_key()?;
let database = user.create_database(initial_settings, &key).await?;

let txn = database.new_transaction().await?;
let settings = txn.get_settings()?;
assert_eq!(settings.get_name().await?, "Draft Notes");
settings.set_name("Project Notes").await?;
assert_eq!(settings.get_name().await?, "Project Notes");
txn.commit().await?;

let next = database.new_transaction().await?;
assert_eq!(next.get_settings()?.get_name().await?, "Project Notes");
# Ok(())
# }
```

A settings read combines the `_settings` history at the transaction's fixed main-parent snapshot with that transaction's staged settings. The first transaction sees its own rename before commit; the next transaction in the example reads it from committed history. A transaction opened with `Database::new_transaction_at()` at an earlier snapshot does not pick up settings written after that boundary.

## Genesis

`AccountSession::create_database()` passes the selected signing key and settings to `Database::create_with_init`. It stages the initial `_settings` Doc and `_root` marker, adds random entropy, and commits one signed root Entry. Its ID identifies the Database. A user Store opened in the genesis initializer can also have its [registration](subtree_index.md#from-genesis-to-the-first-registration) in that Entry. If `auth` is absent, creation grants the signer `Admin(0)`. If `auth` is supplied, it is used as given: the signer must already have active Admin authority in that configuration, and invalid or empty Auth does not receive a fallback creator grant.

Genesis has no main parents, so its settings pin and `_settings` parents are empty. It is the only entry that can bootstrap authority from its own settings payload.

## Entry Metadata

`Transaction::commit()` writes signed JSON metadata in the entry's main `TreeNode.metadata`. The internal `EntryMetadata.settings_snapshot` is serialized as `settings_tips`, an array of entry IDs. The metadata also carries root entropy and, when enabled, an advisory timestamp.

**`settings_tips` pins the pre-write settings frontier.** It must equal the complete canonical `_settings` frontier derived independently from the entry's main parents. The comparison is set equality, not list order. If the entry writes `_settings`, that Store's parents must equal the same frontier. Ordinary Stores may omit observed Store tips; `_settings` cannot omit causal authority context. The signed pin does not replace complete main ancestry or enable sparse verification.

Except at genesis, signature and permission checks use the pinned pre-write settings, not the live head or the settings produced by the entry. An entry cannot authorize itself by adding a grant, and a later grant or revocation does not change the authority of an earlier entry. The resulting settings are the merge of the pre-write state and the entry's settings contribution. They are used separately for delegation transitions, including pointer floors and effective removal. See the [causal authorization rules](authentication.md#causal-snapshot-validation) and [snapshot boundaries](authentication.md#local-state-and-snapshot-boundaries).

In the rename example, call the root G and the rename R. R has main parent G, `_settings` parent G, and `settings_tips = [G]`. A subsequent data-only child of R pins `[R]` without copying the name or writing `_settings`. The signed IDs identify a snapshot, not the merged settings document. [Height calculation](height_strategy.md#calculating-a-commit) uses the resulting settings instead.

The optional `ts` field is independent of authorization. The author emits it when the resulting settings contain `entry_timestamps: true`, including on the commit that enables it. `Entry::committed_at_ms()` reads this signed author-clock value; verification does not interpret it, and it does not determine ordering. See [Optional commit timestamps](../user_guide/transactions.md#optional-commit-timestamps).

## Verification and read limits

Default local transactions read from the `Verified` frontier. If a received entry is missing required main, settings, or delegated proof, or that proof is not yet `Verified`, the entry can remain `Unverified` and absent from default reads. A later local `Database::verify()` pass can promote it once the proof is available and validation succeeds. A conclusively invalid pin or settings-parent frontier instead makes the entry `Failed`.

`Database::allow_unverified()` selects a diagnostic, non-`Failed` frontier; it does not prove that the selected history is accepted or relax the checks for a signed commit. See the [verification model](verification.md#default-reads-and-allow_unverified).
