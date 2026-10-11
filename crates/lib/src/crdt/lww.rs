//! Ordered, right-biased register operations.

use super::CRDT;

/// A register operation composed in the caller's deterministic order.
///
/// The rightmost non-`NoOp` operation wins. "Last" does not refer to a clock
/// or timestamp. `NoOp` is the identity; `Delete` is a retained tombstone,
/// not absence. A later `Set` replaces `Delete` at the value level; this does
/// not define Table row lifecycle or resurrection semantics.
///
/// The payload need not implement [`CRDT`], `Default`, or any encoding trait.
/// Merging requires only `T: Clone`. This type has no built-in [`super::Codec`]
/// or Serde implementation and does not select a persistence format.
///
/// ```
/// use eidetica::crdt::{CRDT, Lww};
///
/// #[derive(Clone, Debug, PartialEq)]
/// struct Payload(&'static str);
///
/// let before = Lww::Set(Payload("old"));
/// assert_eq!(before.merge(&Lww::Delete)?, Lww::Delete);
/// assert_eq!(Lww::Delete.merge(&Lww::Set(Payload("new")))?, Lww::Set(Payload("new")));
/// assert_eq!(before.merge(&Lww::default())?, before);
/// # Ok::<(), eidetica::Error>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Lww<T> {
    /// Identity operation; leaves the preceding operation unchanged.
    #[default]
    NoOp,
    /// Replace the complete value, without merging the payload.
    Set(T),
    /// Remove the visible value while retaining the deletion operation.
    Delete,
}

impl<T> From<T> for Lww<T> {
    /// Wrap a payload as a replacement operation without cloning it.
    fn from(value: T) -> Self {
        Self::Set(value)
    }
}

impl<T: Clone> CRDT for Lww<T> {
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        Ok(match other {
            Self::NoOp => self.clone(),
            _ => other.clone(),
        })
    }
}
