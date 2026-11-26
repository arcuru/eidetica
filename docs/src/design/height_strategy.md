# Height Strategy Design

In a historical Database, **parent IDs determine ancestry; height orders a traversal of that ancestry**. Main-tree traversal sorts Entries by increasing main height. To fold a Store's history, Eidetica orders its contributions by increasing _effective Store height_. In either traversal equal heights are broken by `ID` order (the CID tuple, not the alphabetical order of its printed string). The Store's CRDT determines what merging the ordered contributions means.

## What is stored

| Where                             | Meaning                                                                                                                                                             |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `_settings.height_strategy`       | The database's authoring strategy; the default is `Incremental`. Set it with `SettingsStore::set_height_strategy`.                                                  |
| `_index` registration for a Store | Optional override in `settings.height_strategy`. `Store::set_height_strategy(Some(strategy))` requests an independent height; `None` requests inheritance.          |
| Main `TreeNode`                   | The calculated height (`0` if the field is omitted).                                                                                                                |
| Participating `SubTreeNode`       | An explicit height for an independent Store; no height field when inheriting. `Entry::subtree_height(name)` returns the explicit value or this Entry's main height. |

The setters encode the strategy as JSON in a `Doc` text value. The reader of an old **Entry** does not consult today's `_settings` or `_index` to find its height. See [Settings storage](settings_storage.md) for the boundary between staged and historical settings, and [Subtree index](subtree_index.md) for the registration write.

## Calculating a commit

`Transaction::commit` uses the **resulting** database settings, including a strategy change staged in this transaction. This is different from the pre-write settings used to authorize the commit.

1. For each touched Store with an override, calculate its height from its own effective Store-parent heights. This does not raise the main height. Names beginning with `_` always inherit during height calculation.
2. For the main height, take the maximum over main-parent heights **and** the effective parent heights of every touched inheriting Store. Inheritance copies the resulting main height; it does not start a new counter.
3. In each calculation, `Incremental` gives that maximum plus one (zero without parents). `Timestamp` gives the greater of that result and the author's clock in Unix milliseconds. Neither the timestamp strategy nor the optional signed `ts` metadata proves real-time event order across writers. `ts` is not a sort key.

## A fork and two ways to join it

The integration test `test_store_height_transition_joins_all_effective_parent_heights` constructs this fork with a fixed clock. The database uses `Incremental`. On top of genesis, the first write to `transition` registers the Store in `_index` and sets its override to `Timestamp`. Two transactions opened with `Database::new_transaction_at` at that **same** first write produce sibling Entries. A transaction at a snapshot containing both siblings writes `transition` and changes its override in the same commit.

| Entry                                        | Main parents   | Main height | Effective `transition` height |
| -------------------------------------------- | -------------- | ----------: | ----------------------------: |
| First Store write                            | Genesis        |           1 |    Author clock (independent) |
| Left                                         | First write    |           2 |                          2000 |
| Right                                        | First write    |           2 |                          5000 |
| Join, request inheritance                    | Left and right |    **5001** |              5001 (inherited) |
| Alternatively: join, set `Some(Incremental)` | Left and right |       **3** |            5001 (independent) |

`set_height_strategy(None)` requests inheritance for the join being authored. It omits `settings` from the staged registration rather than writing a tombstone. A historical Doc merge preserves omitted children, so a fresh transaction can still read the previous override. Inspect the join's stored height to see what it used; do not treat the setter as a durable reset.

Neither sibling writes `_settings`, so the join pins the same settings frontier as the siblings, not their IDs. Both joins consume both Store parents. Both must exceed 5000 **in the Store**. Only the inheriting join must carry that jump into the main tree and its later descendants. The siblings' different Store heights do not make one an ancestor of the other, and the larger height does not discard the other tip.

## Verification is not strategy replay

Local and received Entries must grow strictly along every main-parent edge and along each actual Store-parent edge in effective Store height. Positive gaps are valid: the verifier does not reconstruct the author's clock or require the smallest possible increment. An ordinary Store parent must contribute to the same Store in the same Database and be reachable through main links. Observing another Store write does not require consuming it or exceeding its independent Store height. Empty, nonmaximal and redundant ordinary Store-parent sets can be valid. A `_settings` write, by contrast, must consume the complete settings frontier at its main parents.

See [Store-parent frontiers](verification.md#store-parent-frontiers) for a main descendant that leaves two Store tips, and [height monotonicity](verification.md#height-monotonicity) for received Entries and the offline procedure for previously verified history. `Incremental` and `Timestamp` are implemented authoring strategies, not a general-purpose custom strategy or a wall-clock query interface.
