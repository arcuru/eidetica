//! Ordered, right-biased register operations.

use super::{CRDT, Data};

/// A register operation reduced in deterministic Entry order (height, then ID).
/// "Last" does not refer to wall-clock time. `NoOp` is the identity;
/// `Delete` is a retained tombstone, not absence.
///
/// ```
/// use eidetica::crdt::{CRDT, Lww};
/// let before = Lww::Set("old".to_owned());
/// assert_eq!(before.merge(&Lww::Delete).unwrap(), Lww::Delete);
/// assert_eq!(Lww::Delete.merge(&Lww::Set("new".to_owned())).unwrap(), Lww::Set("new".to_owned()));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum Lww<T> {
    /// Identity operation.
    #[default]
    NoOp,
    /// Replace the complete value.
    Set(T),
    /// Remove the visible value.
    Delete,
}

impl<T> Data for Lww<T> where T: Clone + serde::Serialize + serde::de::DeserializeOwned {}

impl<T> CRDT for Lww<T>
where
    T: Clone + serde::Serialize + serde::de::DeserializeOwned,
{
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        Ok(match other {
            Self::NoOp => self.clone(),
            _ => other.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_associativity_and_identity() {
        let operations = [Lww::NoOp, Lww::Set(1), Lww::Set(2), Lww::Delete];
        for a in &operations {
            assert_eq!(a.merge(&Lww::NoOp).unwrap(), *a);
            assert_eq!(Lww::NoOp.merge(a).unwrap(), *a);
            for b in &operations {
                for c in &operations {
                    assert_eq!(
                        a.merge(b).unwrap().merge(c).unwrap(),
                        a.merge(&b.merge(c).unwrap()).unwrap()
                    );
                }
            }
        }
        assert_eq!(
            serde_json::to_string(&Lww::Set(42)).unwrap(),
            r#"{"set":42}"#
        );
        assert_eq!(
            serde_json::to_string(&Lww::<i32>::Delete).unwrap(),
            r#""delete""#
        );
    }
}
