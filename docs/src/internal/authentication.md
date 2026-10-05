# Authentication

Ed25519 signatures and causal settings determine whether an entry is authorized.
The [authentication design](../design/authentication.md#delegated-database-references)
is the canonical explanation of delegated floors, trust boundaries and examples.
The [verification model](../design/verification.md) owns status and visibility;
the [CLI guide](../user_guide/cli.md#db-reset-local-verification-offline-trust-reset)
owns the offline reset and dependency-first recovery procedure.

## Authentication States

| State                   | `_settings.auth`          | Unsigned entry                      | Signed entry                                   |
| ----------------------- | ------------------------- | ----------------------------------- | ---------------------------------------------- |
| Unauthenticated history | Missing or empty          | Accepted by the low-level validator | No authority, except genuine genesis bootstrap |
| Authenticated history   | Keys or global permission | Rejected                            | Signature and permission checks required       |
| Corrupt or deleted auth | Wrong type or tombstone   | Rejected                            | Rejected                                       |

Normal transaction APIs require a signing key.
A non-genesis write cannot use newly staged settings to authorize itself.
Transactions prevent auth corruption before storage; remote verification also
rejects corrupt historical auth.

## Permission and Key Resolution

| Permission | Settings/keys | Write | Read | Priority                          |
| ---------- | ------------- | ----- | ---- | --------------------------------- |
| Admin      | Yes           | Yes   | Yes  | Lower number has higher privilege |
| Write      | No            | Yes   | Yes  | Lower number has higher privilege |
| Read       | No            | No    | Yes  | None                              |

Direct keys live in `_settings.auth.keys`, indexed by public key; a name is a hint,
not an identity. The wildcard supplies global permission for an actual signer.
Delegations live in `_settings.auth.delegations`, indexed by database root ID.
At each delegation step permission is clamped to its configured minimum/maximum;
the final hint must resolve a concrete key, not the wildcard.
Priority authorizes administrative actions; it does not change Doc merge ordering.

## Validation Seams

- `database/mod.rs`: derive and match the signed causal pre-write settings pin;
  check main parents before remote promotion; retain undecidable entries.
- `transaction/mod.rs`: fixed main-parent settings/subtree reads and metadata;
  genuine-genesis-only staged-auth bootstrap; shared signature validator.
- `auth/validation/floors.rs`: join Verified parent projections, derive effective
  resulting settings, preserve separate direct/nested root observations, prove
  complete delegated ancestry, and rebuild cold derived state.
- `auth/validation/delegation.rs`: declaration lookup at historical snapshots,
  coverage of configured/inherited floors, bounds and key resolution.
- `service/server.rs`: store submitted entries Unverified and verify server-side;
  clients cannot assert a verification label.
- `backend/database/{in_memory,sql}/traversal.rs`: anchored store boundaries;
  SQL reads both current tip sets in one statement, InMemory uses its lock.

A projection can be published after authentication succeeds but before the Entry
is stored/promoted. Consumers must still check the parent's local status before
using it. Unsupported derived-state storage rebuilds from Entries; cache I/O
failure is not an empty state. No recursive cross-tree verification belongs in
these seams while a per-tree verification lock is held.

## Invariant-to-Regression Map

Names below are runnable test filters, not a test-count target.
`floor_tests` refers to `crates/lib/src/auth/validation/floor_tests.rs`;
`validation::tests` to its sibling `tests.rs`. Integration paths are relative to
`crates/lib/tests/it/`.

| Invariant                                                                                                               | Regressions                                                                                                                                                                                                                                                                                                                                                |
| ----------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Equal/old causal snapshots remain valid; observed reductions cannot regress                                             | `floor_allows_equal_snapshot`, `floor_allows_stale_snapshot_when_ancestry_is_older`, `floor_blocks_removed_member_below_acknowledged_removal` (`floor_tests`)                                                                                                                                                                                              |
| Join every parent, including incomparable snapshots and observations carried through other signers                      | `floor_joins_all_parents_over_incomparable_frontiers`, `floor_carries_through_direct_key_intermediate`, `floor_carries_through_other_identity_intermediate`, `floor_is_keyed_by_tree_not_signer` (`floor_tests`)                                                                                                                                           |
| Direct Admin rewind allowed; descendants retain floors; same-entry delegated pointer write covers new pointer           | `pointer_add_delegated_tree_allows_admin_rewind`, `pointer_raw_settings_write_allows_admin_rewind`, `pointer_advance_raises_committed_floor_for_descendants`, `same_entry_delegated_pointer_write_must_cover_new_pointer` (`floor_tests`)                                                                                                                  |
| Effective removal resets direct floors, not a losing delta; merged absence clears active-parent observations            | `pointer_effective_removal_resets_floor`, `losing_removal_does_not_clear_active_branch_floor`, `merged_absence_clears_even_an_active_branch_claim` (`floor_tests`)                                                                                                                                                                                         |
| All path roots are tracked; overlapping direct removal cannot erase nested observations                                 | `floor_applies_to_every_step_of_a_nested_path`, `nested_floor_survives_first_hop_removal_and_readd`, `overlapping_direct_and_nested_root_removal_keeps_nested_floor` (`floor_tests`)                                                                                                                                                                       |
| Empty/foreign tips invalid; Verified tip alone is not complete proof; unsettled ancestry can recover                    | `nested_empty_configured_pointer_is_invalid`, `floor_check_still_rejects_wrong_tree_tips`, `delegated_present_unverified_tip_is_retryable`, `delegated_verified_tip_requires_every_ancestor_verified` (`floor_tests`); missing-root/tip/intermediate rejection-and-retry tests in `database/tests.rs`                                                      |
| Bounds apply before path/tip amplification                                                                              | `test_delegation_path_length_capped`, `test_delegation_tips_count_capped` (`validation::tests`)                                                                                                                                                                                                                                                            |
| Cache miss cannot erase a floor; published state is not a status promotion                                              | `verified_parent_cache_miss_rebuilds_floor`, `published_projection_cannot_promote_unverified_parent` (`floor_tests`)                                                                                                                                                                                                                                       |
| Main-parent frontier matches signed pre-write pin and settings subtree parents; revocation does not reject old siblings | `settings_parent_frontier_omission_and_extraneous_edge_fail_remote`, `non_settings_pin_cannot_promote_unsigned_child`, `revoked_signer_cannot_pin_old_settings_below_revocation`, `missing_main_ancestor_defers_forged_pin_decision_until_retry` (`floor_tests`)                                                                                           |
| Local historical pin agrees with later verification; bootstrap is genesis-only                                          | `test_historical_transaction_pins_main_parent_settings` (`database/settings_metadata.rs`); `test_non_genesis_first_auth_cannot_authorize_its_signer_locally`, `test_genesis_first_auth_survives_reverification`, `test_local_historical_commit_rejects_unverified_parent_without_storage` (`database/tests.rs`)                                            |
| Fixed boundary survives concurrent writes/grants                                                                        | `fixed_parent_subtree_read_ignores_concurrent_live_write`, `fixed_parent_auth_settings_read_ignores_concurrent_grant` (`transaction/tests.rs`); `fixed_parent_store_snapshot_ignores_concurrent_live_write`, `current_tips_query_returns_its_own_snapshot_after_concurrent_write` (SQL traversal tests). These use forced interleavings, not timing sleeps |
| Historical backend traversal rejects partial/foreign ancestry; empty store name denotes main tree                       | `historical_store_snapshot_rejects_missing_or_foreign_ancestry`, `historical_tree_tip_store_returns_boundary_tips` (`backend/subtree_operations.rs`); `test_backend_get_tree_from_tips_rejects_missing_intermediate`, `test_backend_get_tree_from_tips_rejects_foreign_ancestor` (`backend/tree_operations.rs`)                                            |
| Service retains undecidable entries; proven regressions fail; nonempty NotFound is not EMPTY                            | `test_submit_missing_delegated_history_stays_retryable_and_invisible`, `test_submit_delegated_snapshot_regression_is_failed`, `remote_snapshot_at_preserves_missing_boundary_error` (`service.rs`)                                                                                                                                                         |
| Offline reset retains Entries, clears terminal labels/caches, and dependency-first retry rebuilds usable data           | `explicit_reset_keeps_entries_and_forgets_both_terminal_statuses`, `sqlite_reset_failure_rolls_back_cache_and_status_then_retries` (`backend/verification.rs`); `test_delegated_entry_synced_unverified_then_verified` (`auth/delegated_trees.rs`, including reset/rebuild)                                                                                |
| Signed wire/IDs unchanged, including nested paths; Snapshot remains Vec-compatible                                      | `test_dagcbor_wire_format_is_pinned`, `test_delegated_dagcbor_wire_format_is_pinned` (`entry/tests.rs`); `serializes_as_bare_id_array` (`snapshot.rs`)                                                                                                                                                                                                     |

Floor unit tests use InMemory.
Backend conformance tests and the delegated reset/rebuild receiver run on the
local InMemory/SQLite/Postgres matrix; the latter uses InMemory when the service
matrix is selected because verification is server-local.
`service.rs` separately exercises the real Unix-socket boundary.
Do not interpret a service matrix count as every raw backend test running over RPC.

For iteration, select these filters with `cargo nextest run --all-features -E
'...'` inside the Nix dev shell.
The completion gate is `nix develop -c nix run .#fix` followed by
`nix develop -c just nix full`, including all backend matrices and integration VMs.
A green suite is evidence for these invariants, not a proof of arbitrary security
properties or automatic dependency acquisition.
