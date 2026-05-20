> ✅ **Status: Implemented** (entry verification status, pinned-settings
> validation, Verified-frontier reads).
> ⚠️ **Known gap:** authority _reduction_ (key revocation, permission
> downgrade, key removal) is **not yet enforced** — see
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

- **`Verified`** — this node ran the validation pass and the entry's
  signature and permissions check out against the settings the entry pins.
- **`Unverified`** — not yet checked, _or_ checked-but-undecidable because
  this node does not yet hold the settings ancestry the entry pins. A
  **transient, monotonic** state: it only ever resolves toward `Verified` or
  `Failed` as more of the DAG arrives.
- **`Failed`** — checked and **definitively rejected** (bad signature, or
  signed by a key without the claimed authority under the pinned settings).
  Terminal.

The `Unverified`/`Failed` split is load-bearing. A single "not Verified"
state would conflate "I can't tell yet" (normal under partial sync) with "I
checked and this is bad", forcing either false rejection of legitimate
in-flight data or acceptance of definitively-bad data. They are distinct
states for that reason.

## Status is never caller-assertable

The storage layer stores **every** entry as `Unverified` on write and
exposes no API — local or remote — for a caller to assert a status. An entry
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

Validation is always performed against the `_settings` state the entry
**pins** in its signed metadata (`settings_tips`), **not** against the
current settings. Because the pin is inside the entry's signed envelope it
cannot be forged: an attacker cannot rewrite which settings an entry claims
to adhere to without invalidating the signature.

This makes verification a **content-addressed** decision with **no staleness
ambiguity**. To verify entry `E` a node needs only the `_settings`
ancestor-closure that `E` pins — not the latest settings, not a tip-complete
view. The node either holds that ancestor set or it does not:

- holds it → run the check → `Verified` or `Failed` (terminal either way);
- does not hold it yet → `Unverified`, retried automatically when the pinned
  settings ancestry arrives.

"Latest settings" never enters the decision, so there is no window in which
the same entry verifies differently depending on sync timing. See
[Settings Storage › Entry Metadata](settings_storage.md#entry-metadata) for
the metadata mechanics.

A direct consequence: **granting authority later cannot retroactively
invalidate history.** An entry that pinned narrower settings stays valid
even after the signer is given more power. This is correct and intended —
and it is exactly why the _reduction_ case below is hard.

## Prefix-closed reads: the Verified frontier

Verification is **prefix-closed**: an entry is promoted to `Verified` only
once its entire ancestor history is `Verified`. A `Failed` ancestor taints
descendants to `Failed`; an `Unverified`/not-yet-held ancestor leaves the
entry `Unverified` for a later pass. The set of `Verified` entries is
therefore always ancestor-closed.

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
  tip. The liveness/DoS surface of this is accepted for now: it is bounded
  by who is allowed to write at all (connection-level authorization) and by
  the monotonic, self-resolving nature of `Unverified`.

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

This composes with [pinned-settings validation](#pinned-settings-validation):
the entry still pins the `_settings` ancestry it was authored against,
independent of which projection supplied its parents.

## Authority reduction (revocation) — the known gap

**Status: a known, deliberate hole in the model. Documented, not yet
designed in detail, not implemented.**

Pinned-settings validation is correct for authority **grants** (more power
later cannot retroactively invalidate an entry that pinned less). It is
**unsafe for authority removal**. A revoked key, a downgraded permission, or
a removed key can keep signing _new_ entries that pin _pre-removal_
settings, and those entries verify forever against their pinned snapshot.
Revocation, generalized — **any monotonic reduction of authority** — cannot
be expressed as a per-entry predicate, because the entry pins the very
settings that would need to change to reject it.

Grants are pin-safe; **removals are not**. Enforcing removals requires
evaluating against _current_ settings / a revocation set, on a path
distinct from pinned-settings verification.

### Plan of record (direction, not yet a detailed design)

Revocation is **not** a per-entry check. It is a **branch-validity
predicate evaluated against the latest settings**: a set of tips is valid
iff every change since their last common splits conforms to the _current_
settings. A branch carrying writes from a now-blocked key ceases to be a
valid branch; honest branches not carrying those writes stay valid, so the
system converges on the revoked contributions being orphaned. Competing
administrative actions (revoke vs. re-grant) resolve via **admin power
levels** — the existing `Admin(priority)` ordering — which is precisely why
power levels exist. See
[Authentication › Priority System](authentication.md#priority-system) and
[Authentication › Key Revocation](authentication.md#key-revocation) for the
authority and delegated-key-revocation primitives this would build on.

### Two-predicate consequence

This introduces a **second, orthogonal axis** beyond per-entry verification.
An entry can be `Verified` (signature good against its pinned settings) yet
sit on an **invalid branch** (it shares a since-split segment with a revoked
key's writes, or its own signer was later revoked). Entry verification
status and branch validity must not be conflated; a complete query/
disclosure model ultimately needs **both**.

### Open hard sub-problems (acknowledged, unsolved)

- **Which "latest settings"?** Settings is itself a forkable DAG; "latest"
  is undefined without a settings-head resolution rule. That rule is
  power-level conflict resolution — but computing valid settings requires
  knowing who is revoked, which requires valid settings. The grounding is
  presumably the unbroken highest-power admin chain; closing this recursion
  is the core hard problem.
- **Retroactive blast radius.** Invalidating every branch containing a
  revoked key's historical writes can orphan large amounts of legitimate
  co-mingled history. "Since last common splits" is meant to bound this, but
  a long-lived interleaved branch cannot be cleanly salvaged without
  re-merge. Revocation therefore implies a potential large history rewrite.
- **Convergence under partial sync.** Branch validity must be a
  deterministic pure function of (DAG, resolved-settings). A replica that
  has not yet received the revocation transiently accepts a branch it will
  later reject — acceptable under the disclosure posture (the branch-level
  analogue of `Unverified`), but it ties back to the same
  settings-sync-completeness story as entry verification.

Until this is designed and built, operators should treat key revocation as
**preventing new entries from building on revoked keys** (the delegated-key
mechanism in [Authentication › Key Revocation](authentication.md#key-revocation))
rather than as retroactively invalidating an attacker's continued
pin-against-stale-settings signing. The latter is the unbuilt work above.

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
