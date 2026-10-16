//! Ordered row operations, independent of Table and persistence.

use super::CRDT;

/// A row operation composed in the caller's deterministic left-to-right order.
///
/// [`Self::NoOp`] is the identity, not a row-creation policy. An initial
/// [`Self::Update`] supplies its payload without inventing a default `V`.
/// Ordinary live updates use `V::merge`; [`Self::Delete`] retains a whole-row
/// tombstone and suppresses subsequent updates. [`Self::Reset`] replaces any
/// preceding operation with a fresh payload, including after deletion. Later
/// updates preserve the reset marker so regrouping cannot restore older data.
/// There are no clocks or causal observed-delete rules.
///
/// This is replicated composition, not local-write validation: a future Table
/// API must reject ordinary writes when the transaction observes deletion.
/// Merge instead suppresses such updates without a deleted-row error during
/// history replay. An inner [`super::Lww::Delete`] deletes only that payload's
/// value; it is distinct from whole-row `RowLifecycle::Delete`.
///
/// Construction and default impose no payload bounds. Merging requires
/// `V: CRDT + Clone`. There is no built-in [`super::Codec`] or Serde
/// implementation, Table integration, or implicit conversion from `V` (choose
/// `Update` or `Reset` explicitly). Downstream matches require a wildcard
/// because this enum is non-exhaustive.
///
/// # Associativity and errors
///
/// Composition is associative on a closed payload domain where `V::merge`
/// succeeds and is associative, including all intermediate payloads. It is not
/// necessarily commutative or idempotent. Errors from actual payload merges
/// propagate unchanged. Fallible evaluation paths need not agree under
/// regrouping: `(Update(a) + Update(b)) + Delete` can fail on `a.merge(b)`,
/// while `Update(a) + (Update(b) + Delete)` returns `Delete` without that call.
/// Reset and deletion suppression can likewise skip failing payload merges.
/// This type does not make arbitrary failing payloads associative. The
/// `V: CRDT` bound alone cannot establish a closed successful domain; callers
/// must guarantee that domain before relying on regrouped replay or caching.
///
/// ```
/// use eidetica::crdt::{Lww, Map, RowLifecycle};
///
/// let mut rows = Map::new();
/// rows.insert_delta("row", RowLifecycle::Update(Lww::Set(1)))?;
/// rows.insert_delta("row", RowLifecycle::Delete)?;
/// rows.insert_delta("row", RowLifecycle::Update(Lww::Set(2)))?;
/// assert_eq!(rows.get("row"), Some(&RowLifecycle::Delete));
/// rows.insert_delta("row", RowLifecycle::Reset(Lww::Set(3)))?;
/// rows.insert_delta("row", RowLifecycle::Update(Lww::Set(4)))?;
/// assert_eq!(rows.get("row"), Some(&RowLifecycle::Reset(Lww::Set(4))));
/// let deleted = match rows.get("row").unwrap() {
///     RowLifecycle::Delete => true,
///     _ => false, // Required for downstream non-exhaustive matches.
/// };
/// assert!(!deleted);
/// # Ok::<(), eidetica::Error>(())
/// ```
///
/// Exhaustive downstream matching is intentionally rejected:
///
/// ```compile_fail
/// use eidetica::crdt::RowLifecycle;
/// fn inspect(op: RowLifecycle<()>) {
///     match op {
///         RowLifecycle::NoOp | RowLifecycle::Delete => (),
///         RowLifecycle::Update(_) | RowLifecycle::Reset(_) => (),
///     }
/// }
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RowLifecycle<V> {
    /// Identity operation; does not supply a row payload.
    #[default]
    NoOp,
    /// Compose with a live payload; suppressed after whole-row deletion.
    Update(V),
    /// Replace all preceding lifecycle and payload state with a fresh payload.
    Reset(V),
    /// Retain a whole-row tombstone until an explicit reset.
    Delete,
}

impl<V: CRDT + Clone> CRDT for RowLifecycle<V> {
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        Ok(match (self, other) {
            (_, Self::NoOp) => self.clone(),
            (_, Self::Reset(_) | Self::Delete) | (Self::NoOp, _) => other.clone(),
            (Self::Delete, Self::Update(_)) => Self::Delete,
            (Self::Update(value), Self::Update(delta)) => Self::Update(value.merge(delta)?),
            (Self::Reset(value), Self::Update(delta)) => Self::Reset(value.merge(delta)?),
        })
    }
}
