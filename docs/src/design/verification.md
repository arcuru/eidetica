# Verification Model

**Implemented:** local entry status, causal authorization, ancestor-closed
promotion, retained Verified tips, cascading demotion and conservative repair.
**Not implemented:** retroactive branch invalidation, peer-attested `Trusted`
status, automatic dependency acquisition or dependency-first verification.

This is the canonical contract for local verification status and visibility.
The [authentication design](authentication.md#causal-snapshot-validation) defines
signature, permission and delegation validity. Verification status records the
local outcome of applying those rules; it is not part of an immutable Entry or
an assertion accepted from its author.

## The three states

| Status       | Meaning on this node                                                                                                             |
| ------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| `Unverified` | New or undecidable: not checked yet, or main/settings/delegated proof is missing or not yet locally `Verified`.                  |
| `Verified`   | Accepted by the local validator against causal pre-write settings, with locally `Verified` main ancestry.                        |
| `Failed`     | Definitively rejected: for example, an invalid signature, insufficient authority, a forged causal pin or a `Failed` main parent. |

`Unverified` is retryable, not a weaker acceptance. It can remain unsettled
indefinitely without the required dependencies and another verification attempt.
`Failed` is terminal during ordinary operation; repeated receipt or verification
does not clear it. An explicit offline trust reset is the exception.

### State transitions

| Operation                             | Transition and boundary                                                                                                       |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Ingest a new Entry                    | Store `Unverified`, regardless of source.                                                                                     |
| Ingest an already-held Entry          | No change to content, status or retained Verified tips.                                                                       |
| Local validation succeeds             | `Unverified` → `Verified`, subject to the backend's atomic parent guard.                                                      |
| Local validation cannot decide        | Remain `Unverified`; retry after dependencies arrive or verify.                                                               |
| Local validation definitively rejects | `Unverified` → `Failed`.                                                                                                      |
| Privileged local demotion             | `Verified` → `Unverified` or `Failed`; dependent `Verified` descendants become `Unverified` atomically with the new frontier. |
| Conservative repair                   | Only inconsistent `Verified` entries become `Unverified`; no promotion and no clearing `Failed`.                              |
| Offline trust reset                   | All labels, including `Failed`, become `Unverified`; all retained Verified tips and disposable Store state are cleared.       |

Assigning the same status is a no-op. Ordinary `Database::verify()` skips settled
`Verified` and `Failed` boundaries: it is not a rule-upgrade revalidation pass.
A descendant demoted to `Unverified` after a parent fails is not immediately
labelled `Failed` by backend maintenance. A later validator pass can make that
rejection once it examines the descendant.

## Trust boundaries and responsibilities

### Callers, peers and service clients

A **new** Entry enters storage as `Unverified`. The wire carries no verification
label, and neither a sync peer nor a service client can invoke status promotion,
repair or trust reset. Re-receiving an Entry cannot overwrite this node's prior
decision. Authenticating a peer or a service session does not authenticate every
Entry it submits.

A local `Transaction::commit()` validates before storage, then stores the Entry
through the Unverified-only ingest path and promotes it locally. On the service
wire, `SubmitSignedEntry` stores `Unverified` and invokes the **server's**
verification pass. Client-side signing is not server-side acceptance. A submit
can retain an undecidable Entry without making it visible in default reads.

Privileged `BackendImpl` status methods are a trusted, local validator/maintenance
surface, not an alternative public authorization API. They enforce ancestry but
do not independently prove a signer's authority. Code with direct backend access
can read stored Entries regardless of status. Verification is not a storage
confidentiality boundary; read/session permissions and encryption remain separate.

### Validator: signatures, settings and delegation

The signed `settings_tips` metadata must equal the complete canonical `_settings`
frontier derived from the Entry's **main parents**, excluding settings written by
that Entry. A signature protects the pin's bytes; it does not make a writer's
choice of pin authoritative. A complete but stale or non-settings pin is invalid.
Only genuine genesis can bootstrap against its own initial settings.

Both local commits and remote verification use this pre-write authority, not the
live head or a non-genesis Entry's newly staged grants. The resulting post-entry
settings separately determine delegation-pointer floors and effective removal.
See the [causal authorization contract](authentication.md#causal-snapshot-validation)
and [settings metadata](settings_storage.md#entry-metadata).

For delegation, configured pointers and signed claims require correct tree
membership and complete, locally `Verified` ancestry at every required snapshot.
Claims must cover the configured pointer and inherited per-root floors; a
`Verified` tip alone is not sufficient proof. A published derived authorization
projection is not a status promotion and cannot substitute for this check.

Missing or present-but-`Unverified` proof leaves the dependent Entry `Unverified`.
A complete invalid proof produces `Failed`. Operational storage/cache errors
propagate rather than becoming a denial, an empty floor or an acceptance.

`Database::verify()` holds a per-tree lock, walks the Unverified region from raw
tips and processes it parents-before-children. Access-time verification is
suppressed within that pass. The validator neither fetches dependencies nor
recursively verifies another tree under the lock. Verify delegated dependencies
first, then retry their dependents. A `VerifyReport` counts entries considered
in that pass, not all stored failures; an empty report is not a health inventory.

### Height monotonicity

Before promotion, the verifier requires the main-tree height to be strictly
greater than each locally `Verified` main parent's height. Equal or decreasing
heights produce `Failed`, as does a `Failed` parent; missing or `Unverified`
parents otherwise leave the Entry retryable. Heights need not equal
`max(parent heights) + 1`: timestamp heights and gaps remain valid.

This checks main-tree heights only. Store heights have independent strategy
and inheritance rules. Height-based consumers such as `Database::ids_added`
retain their best-effort behavior for raw, unsettled history. Existing
`Verified` entries are not rechecked automatically; use the
[offline trust reset](#explicit-trust-reset-on-verification-rule-upgrades)
before relying on the new rule for previously accepted history.

### Backend: same-tree ancestor closure

Promotion requires every direct main parent to exist in the Entry's canonical
storage tree and already be `Verified`. A root's tree ID is its own ID; a
non-root's is its declared root ID. An Entry's own ID does not create another
verification namespace in which that non-root can masquerade as a root.

The parent check, label update and retained-tip publication share one backend
serialization boundary. A missing, foreign or unsettled parent rejects promotion
without changing labels or tips. There is no provisional `Verified` label
waiting for its parent. Checking direct parents suffices inductively because
supported promotion and demotion maintain closure. It does **not** replace
signature, causal-settings or delegated-proof validation.

## Prefix-closed reads: the Verified frontier

For one tree, let `V` be the accepted set: entries labelled `Verified` whose
parents are all in `V`. The **Verified frontier** is the entries in `V` that
have no child in `V`. An unverified root therefore has an empty frontier. A
frontier can include an interior Entry of the raw DAG when its children are
not accepted.

The raw frontier instead contains entries with no held child in that tree,
regardless of verification status. Raw and Verified frontiers can differ even
when every Entry byte is present. Ingest and verification are separate events.

Concrete InMemory and SQL backends retain **only the Verified tips**, not a
second membership set for the entire accepted history. Authoritative per-entry
labels establish interior membership. Promotion removes direct parents from the
retained tips and inserts the child; forks retain multiple tips and a verified
join replaces them. This operation does not scan accepted history.

### Default reads and `.allow_unverified()`

A local `Database::snapshot()` normally returns the retained Verified frontier.
When raw and Verified tips agree, it performs retained lookups without a history
walk or per-entry status probes. If a raw tip is unsettled, access can attempt
best-effort verification before selecting the read boundary. A read is therefore
not a promise that no validation work occurs; only the settled path is scan-free.

`.allow_unverified()` opts into raw tips with `Failed` **tips** removed, rather
than the Verified frontier. It is a diagnostic/partial-data view, not proof of
an accepted prefix. The filter does not recursively prune every failed or
missing ancestor from arbitrary raw history. Low-level Entry retrieval also
retains rejected data. No status deletes immutable bytes.

A remote service handle's `snapshot()` returns the server-selected Verified
frontier; the client does not run local status machinery. Service operations
that carry a `ReadScope`, such as transaction context and Store-entry queries,
let the server select the requested scope. Calling `.allow_unverified()` is
not a wire-level status override. See [service read scope](../internal/service.md#read-scope).

This is the **disclosure model**: retain the DAG, project locally accepted
history by default. Freshly synced or bootstrapped data can be absent from
default reads until local verification succeeds. Retrying undecidable Entries
improves availability; it is not permission to expose them as Verified.

### Writes inherit the selected boundary, not weaker validation

Transaction parents come from the caller's selected snapshot (or an explicit
historical boundary). A default writer can extend the Verified frontier while
an unsettled raw branch exists, creating a fork around that branch rather than
silently incorporating it. Its settings pin is derived from those exact parents.

An opt-in transaction can **select** Unverified parents, but a signed local
commit rejects them before storage, even if a derived projection for them was
previously published. The backend parent guard also prevents promotion if a
parent loses Verified status before publication. Selecting a raw boundary does
not grant a successful commit against unsettled ancestry. Remote ingest can
retain such a child as `Unverified`; it cannot promote it until the proof settles.

## Atomic publication, demotion and repair

| Backend    | Serialization boundary for status and Verified tips                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| InMemory   | One write lock; compute a repair plan before mutating live maps.                                                                                     |
| SQLite     | One writer transaction acquired with `BEGIN IMMEDIATE`; failures roll back labels and tips together.                                                 |
| PostgreSQL | One transaction with a schema-and-tree-scoped advisory transaction lock, acquired before mutable eligibility checks; released on commit or rollback. |

These guarantees apply to each status mutation or tree repair, not to an entire
multi-entry verification pass. Readers can observe progress between promotions,
but not half-published label/tip state from one mutation. Raw ingest is separate
and may be visible before promotion.

Demotion of a Verified Entry recomputes closure for the affected tree and changes
its dependent Verified region to `Unverified`. Other accepted forks remain.
There is no cross-tree cascade through delegation references. Demotion and repair
may load the entire affected tree temporarily; the retained state still scales
with frontier width, not history size.

`BackendImpl::repair_verification(tree)` reconstructs closure from immutable main
ancestry and current labels, not stored heights or retained tips. Missing/foreign
parents exclude their dependent region. Malformed Entries, cycles or storage
errors abort without partial publication. Repair preserves `Failed`, never
promotes, and does not rerun cryptographic authorization. Repeating it is
idempotent. Entries and Store-state records are unchanged.

The default custom-backend `verified_snapshot` is a read-only history-based
reconstruction oracle, not the concrete backends' retained fast path or a label
repair. Custom backends must implement atomic repair explicitly; the default
repair method returns unsupported instead of attempting non-atomic mutation.

The offline [repair command](../user_guide/cli.md#db-repair-verification-offline-conservative-repair)
backs this local maintenance surface. It is different from both verification and
[trust reset](#explicit-trust-reset-on-verification-rule-upgrades).

### Existing snapshots and events

A previously acquired Snapshot, transaction boundary or resolved historical
Store view may keep its old boundary after demotion. This change does not revoke
already-acquired readers or retroactively invalidate cached historical state.
Acquire a new default boundary to observe the repaired frontier; offline trust
reset requires stopping all readers as well as writers.

`WriteEvent`/notification cursors remain **raw DAG frontiers**. Verification can
emit a batched settled-write notification without changing those raw tips.
A cursor is not a Verified membership proof; consumers use the appropriate
Database read projection rather than treating notification IDs as accepted data.

## Worked examples

- **Fork and join.** Start with Verified `R`. Ingest siblings `A` and `B`, then
  verify only `A`. Raw tips are `{A, B}`, Verified tips `{A}`. A join `J` with
  parents `{A, B}` cannot promote while `B` is Unverified; rejection changes
  nothing. After `B` verifies, tips are `{A, B}`; promoting valid `J` yields
  `{J}`. Demoting `A` demotes Verified `J` to Unverified and leaves Verified
  tips `{B}`, not `{R, B}`: `R` still has the accepted child `B`. The retained
  frontier records maximal accepted entries, not every surviving ancestor.
- **Missing parent.** A received child `C` names an absent parent `P` in tree
  `R`. It stays Unverified. Receiving `P` alone is insufficient: verify `P`
  and its ancestry first, then retry `C`. Conservative repair would demote an
  old inconsistent Verified `C`, not guess its signature validity or fetch `P`.
- **Revocation.** Writer `K` is authorized at `R`. A valid sibling write from
  `R` remains historically valid even if another branch later removes `K`.
  A descendant whose parents include that removal cannot evade it by signing
  an older settings pin. This is causal enforcement, not retroactive revocation.

## Authority reduction (revocation) — the known gap

**Causal reductions are enforced; retroactive branch invalidation is not.** A
later grant cannot authorize an invalid historical Entry. Conversely, holding a
newer revocation elsewhere does not invalidate a valid pre-revocation sibling.
Delegated claims cannot regress below configured pointers or inherited per-root
floors. An Admin can intentionally rewind a pointer; retained observations still
constrain descendants until effective direct removal. The
[causal examples](authentication.md#causal-snapshot-validation) explain the joins
and removal rules.

A policy that rejects historical branches containing a now-revoked signer's
writes would be a separate predicate. It needs decisions about concurrency,
partial sync and orphaned legitimate history. It is not implemented or specified
here. Today's `Verified` is neither live-head freshness nor immediate revocation
across a partition. `Admin(priority)` authorizes settings changes; it does not
replace Doc's deterministic conflict resolution.

## Persistence, restart and forward migration

SQL `schema_version` and InMemory JSON `_v` are **independent internal integer
format revision counters**. Both currently equal `2`; they are not release
versions, SemVer or a stability scale. A format change requiring migration
advances the relevant counter and supplies an explicit supported forward path.
Do not renumber existing data or infer a migration merely from integer ordering.
The current supported matrix is:

| Stored format                                             | Open/load behavior                                                                                                                            |
| --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| Fresh SQL database (no version row)                       | Initialize current schema and marker `2`. This is initialization, not a promise to repair unmarked populated SQL databases.                   |
| Mainline SQL `0`                                          | Migrate directly to `2`: create retained-tip storage, conservatively repair each tree, publish schema/labels/tips/version in one transaction. |
| SQL `2`                                                   | Use persisted labels and Verified tips; no startup history repair.                                                                            |
| SQL `1`, or any SQL database containing `verified_prefix` | Refuse the unsupported experimental prefix-tracking layout before schema initialization.                                                      |
| SQL above `2`, or below `0`                               | Refuse: newer schema or unknown migration path.                                                                                               |
| InMemory absent `_v` or explicit `_v: 0`                  | Reconstruct Verified tips from Entries and labels on load, conservatively demoting inconsistent Verified regions. Next save writes `2`.       |
| InMemory `_v: 2`                                          | Load persisted labels/tips without history repair.                                                                                            |
| InMemory other `_v`                                       | Refuse; this includes experimental `1` and future versions. Invalid/out-of-range encodings also fail deserialization.                         |

SQL migration preserves immutable Entry bytes, parent/subtree records, raw tips,
instance metadata/secrets and existing Store-state namespaces/records. It changes
only inconsistent Verified labels and the retained Verified frontier alongside
schema/version publication. Failed decisions remain Failed. Failure rolls back
the migration; version refusal is not a wholly read-only open, since ownership
bookkeeping and SQLite WAL setup can precede the schema check.

InMemory reconstruction builds a new backend before exposing it and does not
rewrite the source file. Entries, metadata/secrets and raw tips survive; legacy
CRDT cache bytes are discarded and Store-state namespaces are not persisted by
this backend. A successful save writes a sibling temporary file and replaces the
snapshot by rename (atomic replacement on POSIX; see the
[backend guide](../user_guide/concepts/backends.md#inmemory) for limits). A failed
load cannot partially migrate the original file.

Current-format opens trust retained state; they do not silently repair a missing
frontier. Use explicit repair for inconsistent local ancestry, or reset for a
verification-rule upgrade. Neither command converts experimental version `1`.
Keep an offline backup and follow the
[unsupported-format recovery guidance](../user_guide/cli.md#storage-compatibility-and-older-candidate-data)
rather than editing markers or dropping tables to bypass refusal. Downgrading a
binary does not undo persisted migrations; restore a compatible backup instead.

## Explicit trust reset on verification-rule upgrades

Format migration is **not** authorization revalidation. When upgrading to new
verification rules, stop all users, back up the backend and run the offline
[`db reset-local-verification` command](../user_guide/cli.md#db-reset-local-verification-offline-trust-reset)
before trusting old decisions under the new rules. It clears old Verified and
Failed labels, retained tips, derived and incomplete Store state, while keeping
immutable Entries and authoritative Store state. It does not verify or fetch.

Then use the [dependency-first SDK procedure](../user_guide/cli.md#re-verification-after-reset-or-incomplete-proof),
inspect unresolved/rejected branches and acquire new read boundaries. There is
no automatic verification-rule version marker or startup reset. Clearing only
caches, repairing closure or advancing a format counter cannot substitute for
this operator-triggered trust reset.

## Deferred work

`Trusted` peer attestations remain an undesigned future possibility, not an
accepted fourth state or an alternate promotion route. Retroactive revocation,
automatic dependency acquisition/retry, incremental demotion and materialization
cache optimization are not implemented by this verification contract. None may
weaken local validation or silently reinterpret existing statuses.

## See also

- [Authentication](authentication.md) — signatures, causal settings and delegation.
- [Synchronization](synchronization.md#verification-on-receipt) — receipt and local verification.
- [Bootstrap & Access Control](bootstrap.md#verification-of-transferred-entries) — transferred Entries and genesis trust.
- [Internal authentication](../internal/authentication.md#invariant-to-regression-map) — implementation seams and regression filters.
