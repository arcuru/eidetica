//! Ordered, right-biased register operations.

use ipld_core::ipld::Ipld;

use super::{CRDT, Codec, envelope};

/// A register operation composed in the caller's deterministic order.
///
/// The rightmost non-`NoOp` operation wins. "Last" does not refer to a clock
/// or timestamp. `NoOp` is the identity; `Delete` is a retained tombstone,
/// not absence. A later `Set` replaces `Delete` at the value level; this does
/// not define Table row lifecycle or resurrection semantics.
///
/// The payload need not implement [`CRDT`], `Default`, or any encoding trait.
/// Merging requires only `T: Clone`. Its explicit [`Codec`] requires only
/// `T: Codec`; there is no Serde implementation, and the codec does not
/// select a format for any existing Store.
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

const NOOP: i128 = 0;
const SET: i128 = 1;
const DELETE: i128 = 2;

/// DAG-CBOR operation envelope: `[0, 0]` is `NoOp`, `[0, 1, payload]` is
/// `Set` with the payload's own [`Codec`] bytes, and `[0, 2]` is `Delete`.
///
/// Only `T: Codec` is required; the payload is never merged or interpreted.
/// Decoding rejects unknown revisions or tags, wrong arity or field types,
/// trailing data and payload bytes that `T` rejects.
impl<T: Codec> Codec for Lww<T> {
    fn encode(&self) -> crate::Result<Vec<u8>> {
        let fields = match self {
            Self::NoOp => vec![Ipld::Integer(NOOP)],
            Self::Set(value) => vec![Ipld::Integer(SET), Ipld::Bytes(value.encode()?)],
            Self::Delete => vec![Ipld::Integer(DELETE)],
        };
        envelope::encode(fields)
    }

    fn decode(bytes: &[u8]) -> crate::Result<Self> {
        match envelope::decode(bytes, "Lww")?.as_slice() {
            [Ipld::Integer(NOOP)] => Ok(Self::NoOp),
            [Ipld::Integer(SET), Ipld::Bytes(payload)] => Ok(Self::Set(T::decode(payload)?)),
            [Ipld::Integer(DELETE)] => Ok(Self::Delete),
            [Ipld::Integer(tag @ (NOOP | SET | DELETE)), ..] => Err(envelope::malformed(
                "Lww",
                format!("wrong fields for operation tag {tag}"),
            )),
            [Ipld::Integer(tag), ..] => Err(envelope::malformed(
                "Lww",
                format!("unknown operation tag {tag}"),
            )),
            _ => Err(envelope::malformed("Lww", "missing integer operation tag")),
        }
    }
}
