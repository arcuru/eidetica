# How a Table reads a snapshot

A historical `Table` read answers a question about immutable Entries: what value did this key have at the transaction's boundary, after applying this transaction's own changes? A cached row is one way to answer that question. It does not choose the boundary.

## Two prices for the same key

Suppose a transaction commits a price of 10 for tea. Another transaction changes it to 12. A reader anchored at the first commit must still see 10, even if it first opens the Table **after** the second commit:

```rust
# use eidetica::{Instance, NewAccount, Snapshot, backend::database::InMemory, crdt::Doc, store::Table};
# #[tokio::main]
# async fn main() -> eidetica::Result<()> {
# let (_instance, mut user) = Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("alice")).await?;
# let key = user.get_default_key()?;
# let db = user.create_database(Doc::new(), &key).await?;
let tx = db.new_transaction().await?;
tx.get_store::<Table<u32>>("prices").await?.set("tea", 10).await?;
let first = Snapshot::from([tx.commit().await?]);

let tx = db.new_transaction().await?;
tx.get_store::<Table<u32>>("prices").await?.set("tea", 12).await?;
tx.commit().await?;

let reader = db.new_transaction_at(&first).await?;
let prices = reader.get_store::<Table<u32>>("prices").await?;
assert_eq!(prices.get("tea").await?, 10);
prices.set("coffee", 8).await?; // visible to this reader, not part of `first`
let page = prices.scan_page(None, 1).await?;
assert_eq!(page.rows, [("coffee".to_owned(), 8)]);
let next = prices.scan_page(page.next.as_ref(), 1).await?;
assert_eq!(next.rows, [("tea".to_owned(), 10)]);
# Ok(())
# }
```

`Table::set` stages the serialized row in the transaction. At commit, `encode_entry_delta` turns those staged changes into the Store's canonical `Doc` delta: the row's JSON is a text value at its key, and a deletion is a tombstone. The first Entry therefore contains the change to `tea`, not a dump of a row cache. Neither the Entry encoding nor what peers must exchange depends on whether a reader ever materializes `prices`.

## Which history counts?

The example supplies a main `Snapshot`. Without one, `Database::new_transaction` captures the database's selected main tips (normally the Verified frontier for a local handle). On first access to `prices`, the transaction calls `store_snapshot_at` relative to **those main parents**, not today's main tips. The resulting Store tips are the source of both the cached read and the history read.

Main ancestry alone cannot identify the last price changes to fold. For example, suppose B observes A and sets `tea`, then C observes B and sets `coffee`, but both B and C name A as their `prices` parent. At main tip C, reading just C's Store history would lose B's price for tea. The `prices` frontier is **both B and C**: inside that main cut neither Store contribution consumes the other. A later Entry that explicitly joins them can replace both tips. See [Store-parent frontiers](verification.md#store-parent-frontiers) for the acceptance rules, including the different rule for `_settings`.

## Three ways to answer the read

- **Cold build.** If no matching row set is published, `Transaction::record_view` requests the ordered Store history reachable from all captured Store tips. Entries are ordered by ascending Store height, then by Entry ID. Applying the `Doc` deltas to a row map in this order gives `TableProjection` the same result even when concurrent branches set the same key. The builder stages surviving rows privately, then publishes the complete set atomically. A reader can resolve a complete generation, never a half-built one.
- **Warm read.** The derived namespace identifies the database, Store, trust scope, projection name and version, and a digest of the complete sorted, deduplicated Store snapshot. A generation for the second price change cannot satisfy the request for the first. `get("tea")` needs one record if no staged change shadows it. `scan_page` merges staged changes with ordered records: the new `coffee` sorts before `tea`; a staged deletion would hide its row. The continuation key is exclusive. It marks a position, not a snapshot; use the same transaction and leave its changes alone between pages to iterate the same view.
- **No usable cache.** A backend may lack record support or decline retention. The transaction then folds the `Doc` from the captured Store parents and serves the point or page from history, respecting the same local changes and cursor. If a resolved view goes stale, the reader discards the partial result and retries once from the original key or cursor. A second invalidation takes the history path; it does not splice pages or switch to newer tips. A missing row is different from an unreadable row: storage, decryption and decode errors are not invitations to bypass verification or return an empty result. An uncertain publish is an error, not a confirmed cache refusal.

Verification determines which history a default read may select. A change of verification status may select a different frontier, but status is not part of the identity of an immutable snapshot. A cache hit is neither evidence of verification nor permission to read. Clearing derived data can change the cost of the next lookup; it removes neither the Entries needed to rebuild it nor authoritative Store-state records.

## What the projection does not change

The shipped `eidetica/table/rows` version 0 describes **derived records**, not a Table wire format. The plain Table uses UTF-8 key bytes and JSON row values; a `PasswordStore` wrapper orders records and cursors by transformed physical keys instead. The default Store model uses one opaque record for its entire CRDT state, so row-addressable reads are not a promise about every Store.

Paging bounds the number of rows returned. Even a warm scan can process more staged changes than it returns. Cold construction has a different cost: the builder fetches historical Entries and accumulates a whole row map; the history fallback can fold a whole `Doc` even for a single key. Neither incremental maintenance nor bounded-memory history streaming follows from a page limit or cache budget. For backend record operations see [Store state](../internal/store_state.md); for retention, service scopes and invalidation see the [historical cache guide](../internal/cache.md).
