> ✅ **Status: Implemented** (entry verification status, pinned-settings
> validation, Verified-frontier reads).
> ⚠️ **Known boundary:** causal authority reduction is enforced, but
> retroactive branch invalidation is **not implemented** — see
> [Authority Reduction](#authority-reduction-revocation--the-known-gap).

# Verification Model

Eidetica entries carry a **verification status** that records whether _this
node_ has checked the entry's signature and authorization, and what the
outcome was. This document is the canonical description of that model: the
three-state enum, why a status can never be asserted by a caller, how
pinned-settings validation makes verification a content-addressed (not
time-sensitive) decision, what reads expose by default, the accepted
trade-offs of the disclosure posture, and the one piece the model
deliberately does **not** yet solve.

Verification status is **orthogonal to signature/permission validity** — the
latter answers "is this entry correctly signed by an authorized key, given
some auth settings?"; the former records whether and with what result this
node ran that check. The validity rules themselves live in the
[authentication design doc](authentication.md#verification-status-vs-signature-validity);
this doc covers the status that wraps them.

## The three states

`VerificationStatus` is an honest three-state enum:

- **`Verified`** — this node accepted the entry against its causal pre-write
  settings, including its signature, permissions and delegated snapshot proofs.
- **`Unverified`** — not yet checked, _or_ checked-but-undecidable because
  main/settings/delegated ancestry is missing or delegated proof is not yet locally `Verified`. A
  **transient, normally monotonic** state: it resolves toward `Verified` or
  `Failed` as more of the DAG arrives.
- **`Failed`** — checked and **definitively rejected** (bad signature, or
  signed by a key without the claimed authority under the pinned settings).
  Terminal during ordinary verification; an explicit offline trust reset clears it.

The `Unverified`/`Failed` split is load-bearing. A single "not Verified"
state would conflate "I can't tell yet" (normal under partial sync) with "I
checked and this is bad", forcing either false rejection of legitimate
in-flight data or acceptance of definitively-bad data. They are distinct
states for that reason.

## Status is never caller-assertable

The storage layer stores **every** entry as `Unverified` on write and
accepts no caller-chosen status on ingest. A service client or sync peer has no
status-promotion operation; privileged local backend status APIs are for the
validator and offline maintenance, not untrusted callers. An entry
becomes `Verified` only through a **local** validation pass (a `Transaction`
commit, or an explicit `Database::verify()`), which stores via the normal
write path and then promotes the entry locally.

This is a hard boundary, not a convenience default: a sync peer or a
service-protocol client **cannot** inject a "pre-verified" entry. Entries
arriving from sync, bootstrap, or the service wire are consequently stored
`Unverified` and must earn `Verified` locally. See
[Synchronization › Verification on Receipt](synchronization.md#verification-on-receipt)
and [Bootstrap › Verification of Transferred Entries](bootstrap.md#verification-of-transferred-entries)
for how this plays out across peers.

## Pinned-settings validation

The signed `settings_tips` metadata must equal the complete canonical `_settings`
frontier derived from the entry's **main parents**. A signature protects those
bytes from alteration but does not make a writer-selected pin authoritative.
Verification derives the causal frontier independently; a complete but older or
non-settings pin is invalid, even if its signature is cryptographically correct.
Only a genuine genesis entry can bootstrap against its own initial settings.

Both local commits and remote verification use that pre-write authority, not the
live head and not a non-genesis entry's newly written settings. The resulting
post-entry settings separately determine delegation-pointer floors and effective
removal. The [causal authorization contract](authentication.md#causal-snapshot-validation)
defines those rules and examples; [Settings Storage](settings_storage.md#entry-metadata)
describes the metadata.

The decision needs complete main/settings ancestry and, for delegation, complete
locally `Verified` ancestry at every claimed/configured snapshot. Missing or
present-but-`Unverified` proof leaves the dependent entry `Unverified`; a complete
invalid proof is `Failed`. Operational backend/cache errors propagate instead
of becoming a denial or an empty floor. A later grant cannot make an unauthorized
historical entry valid, and an unrelated later revocation cannot invalidate a
valid pre-revocation sibling.

Receiving dependency bytes does not necessarily settle them. Access/sync may
attempt verification, but the validator does not fetch dependencies or recursively
verify another tree under its lock. Explicitly verify dependencies first, then
retry the dependent database. Automatic acquisition and dependency-first retry
are deferred to [PR #126](https://github.com/arcuru/eidetica/pull/126).

## Prefix-closed reads: the Verified frontier

Verification is **prefix-closed**: an entry is promoted to `Verified` only
once its entire ancestor history is `Verified`. A `Failed` ancestor taints
descendants to `Failed`; an `Unverified`/not-yet-held ancestor leaves the
entry `Unverified` for a later pass. Supported validation therefore maintains an ancestor-closed `Verified` set.
This assumes validator-owned labels: manual low-level promotion or legacy labels
retained without the required trust reset are not proof of that invariant.

By default a `Database` read exposes only the **Verified frontier** — the
maximal ancestor-closed all-`Verified` prefix. `Failed` entries are dropped
from reads in all cases. A caller that explicitly wants the
pre-verification view (including `Unverified` tips) opts in with
`.allow_unverified()`.

This is the **disclosure model**: the DAG stays complete and trust is a
query-time projection over it (the same posture as git signatures or DKIM —
nothing is hidden from storage; the _trust label_ is computed on read).
Only `Failed` is ever hard-dropped; `Unverified` data is retained, just not
surfaced by the safe-default getter.

## Accepted trade-offs of the disclosure posture

These are deliberate consequences of the model, not defects:

- **Verified-frontier computation cost.** Resolving the Verified frontier on
  a default read walks verification status across the relevant DAG region
  rather than returning raw tips directly. This is the price of a
  safe-by-default read; it is a known performance characteristic of the
  disclosure posture, optimisable behind the same API without changing
  semantics.
- **Sync no longer makes data visible by default.** Before this model, a
  synced entry was immediately readable. Now freshly synced or freshly
  bootstrapped data is **invisible to default reads until verified** — a
  database may briefly read as empty in the instant between transfer and the
  local verification pass. This is an intentional behaviour change; callers
  that need the old semantics use `.allow_unverified()`. Integrators
  upgrading across this change should treat it as a migration-relevant
  behaviour change, not a regression.
- **`Unverified` tips are admitted into normal operation.** Normal writes
  may build on `Unverified` tips, and an `Unverified` entry may itself be a
  tip. Selecting such a parent is not permission to promote its child while
  that parent remains unsettled. The liveness/DoS surface is bounded
  by ingest/resource controls, not by assuming every submitter is authorized.
  An undecidable proof can remain `Unverified` indefinitely without dependency
  acquisition and a subsequent verification attempt.

The default-safe posture (Verified-frontier-by-default, opt-in to see
`Unverified`) is the intended steady state; the re-verification/promotion
pass that drains `Unverified` over time is a **quality** feature (the signal
de-noises as the DAG completes), not a correctness prerequisite.

## Writes inherit the caller's read projection

A write's parent tips are the tips of **the same projection the caller is
reading** — this is not a separate policy. A caller on the default
(Verified-frontier) posture parents new entries onto the Verified frontier; a
caller that opted into `.allow_unverified()` parents onto raw tips. _What you
can see is what you build on._

This is deliberate and removes any global parent-selection rule:

- It keeps default-posture history **ancestor-closed `Verified` by
  construction**: a default writer never extends from an `Unverified` tip it
  cannot see, so it neither forks history away from in-flight unverified data
  nor silently entangles itself with it.
- Building on `Unverified` tips stays possible but is now the caller's
  **explicit, owned** choice (it asked for `.allow_unverified()`), not a
  silent default — and only such a caller is ever exposed to `Unverified`
  tip identifiers.
- Parent tips are additionally bounded by the caller's
  authorization/settings: a write can only parent onto tips the caller is
  permitted to read. Read scope and authority scope jointly define the
  buildable frontier.

The signed local commit gate rejects an `Unverified` parent even if its derived
projection was published earlier. Remote ingest can retain a child of unsettled
parents, but verification defers its promotion. Thus `.allow_unverified()` changes
read/parent selection, not the proof required to earn `Verified`.

This composes with [pinned-settings validation](#pinned-settings-validation):
the entry pins the `_settings` frontier derived from exactly those parents,
not arbitrary settings the writer chose.

## Authority reduction (revocation) — the known gap

**Causal reductions are enforced; retroactive branch invalidation is not.**
A revoked/removed key or reduced permission cannot authorize a descendant whose
main parents carry that reduction by choosing an older settings pin. Delegated
claims likewise cannot regress below the configured pointer or inherited
per-root floor. This closes stale-pin evasion on a branch that has observed the
reduction.

A sibling rooted before the reduction can still be valid, even when a verifier
already holds a newer revocation elsewhere. Snapshot validation does not assert
live-head freshness or retroactively erase valid history. An Admin can also
intentionally rewind a delegation pointer; retained signature observations still
constrain descendants until effective direct removal. See the
[causal examples](authentication.md#causal-snapshot-validation).

A policy that retrospectively rejects branches containing a now-revoked signer's
writes would be a separate predicate, not a reinterpretation of `Verified`.
It is not implemented or specified here. It would need decisions about concurrent
settings, partial sync and the legitimate history orphaned with the revoked
contributions. The existing `Admin(priority)` rules authorize settings changes;
they do not replace Doc's deterministic conflict resolution.
Operators must not treat today's causal verification as such a retroactive
policy or as immediate revocation across a network partition.

## Future: a `Trusted` peer-attested state

**Status: potential future direction, not designed, not implemented.** This
section records the intent so the status representation can be designed to
accommodate it as a non-breaking extension; the mechanics are explicitly
TBD.

Today an entry is either locally `Verified` (this node ran the full check
against the entry's pinned settings) or `Unverified` (this node has not, or
cannot yet). There is no way to express _"a peer Eidetica node I trust has
told me it verified this entry."_ A **`Trusted`** state would be that middle
ground.

**Sketch.** `Trusted` = a peer `Instance` this node trusts has asserted that
_it_ verified the entry, and this node accepts that attestation in lieu of
re-running full signature/permission validation to the roots itself. It is
strictly weaker than local `Verified` (we did not check it ourselves) and
strictly stronger than `Unverified` (a party we trust did). It lets a node
short-circuit expensive ancestor-closure re-verification when a trusted peer
has already done the work, while still keeping "I checked it" distinct from
"someone I trust checked it" — the same instinct as the disclosure model,
one notch up the trust spectrum.

**Where it sits.** Between `Unverified` and `Verified`. The default read
posture (Verified frontier) and the `.allow_unverified()` opt-in would need
a policy decision on whether `Trusted` is surfaced by default, opt-in, or
configurable per trust relationship.

**Open questions (unsolved — design TBD):**

- _What makes a peer "trusted"?_ Sync-peer identity, an explicit trust list,
  or a trust level keyed into the existing authentication / priority model?
- _Is the attestation itself signed and verifiable_, so a trusted peer
  cannot be impersonated and the assertion is non-repudiable — and does it
  carry _which settings_ the peer verified against?
- _When is local re-verification still forced despite `Trusted`_ — e.g. for
  security-sensitive operations, or once this node later acquires the pinned
  settings ancestry and could check the entry itself?
- _Does `Failed` collapse into `Unverified` under this model, or stay a
  distinct terminal state?_ A trusted peer asserting `Failed` is itself
  meaningful information.
- _Transitivity and trust depth._ If peer A trusts peer B, does an A→us sync
  convey B's attestation, or only A's own verification? Trust depth must be
  bounded.
- _Interaction with the [authority-reduction gap](#authority-reduction-revocation--the-known-gap)._
  A trusted peer's attestation is only as good as that peer's own revocation
  awareness; `Trusted` does **not** bypass the branch-validity question.

Near-term work uses `Verified` / `Unverified` / `Failed` only. The status
representation should be chosen so that introducing `Trusted` later is a
non-breaking extension.

## See also

- [Authentication](authentication.md) — signature/permission validity, the
  priority system, and key revocation primitives.
- [Synchronization](synchronization.md#verification-on-receipt) — how
  verification status behaves for entries arriving from peers.
- [Bootstrap & Access Control](bootstrap.md#verification-of-transferred-entries)
  — verification of a freshly transferred database.
- [Settings Storage](settings_storage.md#entry-metadata) — the
  `settings_tips` pin mechanics.

## Explicit trust reset on verification-rule upgrades

A verification status is local, not an immutable property of an Entry. When
an operator upgrades to new delegated-authorization verification rules, the
old `Verified` and `Failed` decisions and disposable derived Store-state must
be cleared **before** starting the new version. Use the offline
[`db reset-local-verification` command](../user_guide/cli.md#db-reset-local-verification-offline-trust-reset)
and follow its dependency-first re-verification procedure from immutable Entries.
This is deliberately operator-triggered,
not a schema migration or automatic legacy-version gate. Skipping the procedure
can leave historical `Verified` labels trusted under the new rules.
