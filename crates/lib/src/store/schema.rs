//! Types for opt-in structured Stores.
//!
//! A Store's Type string is its only version discriminator. It is the `type` of
//! the Store's `_index` row in the signed history of each contributing Entry,
//! never a tag inside the value.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    Result,
    crdt::{CRDT, Codec, Doc, dag_cbor},
    entry::ID,
};

/// Code support for exactly one persisted Store Type.
///
/// Implementing this for a Type string supplies neither permission nor
/// conversion. Mint a different string for an incompatible grammar or merge.
pub trait StructuredType: Send + Sync + 'static {
    /// Nonempty, stable, byte-exact `_index` Type, such as `contacts:v0.1`.
    const TYPE_ID: &'static str;

    /// The complete native contribution and its merge.
    type Data: CRDT + Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Interpret the Store config in the history of one contribution, or refuse.
    ///
    /// `configs` holds every distinct value left by concurrent config changes
    /// there. It has one element unless the history is ambiguous, and an absent
    /// config is an empty Doc. Return what distinguishes this Type's row grammar
    /// or merge parameters. Contributions with different results are never
    /// combined. A Type that does not use its config returns an empty Doc.
    fn grammar(configs: &[Doc]) -> Result<Doc>;
}

/// The Type under which one contribution was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionType {
    /// Type declared at the contributing Entry.
    pub type_id: String,
    /// Latest Entries, in causal order, that declared it.
    pub declared_by: Vec<ID>,
}

/// Whole-Store state decoded under one exact Type.
///
/// `Default` is an empty view with no source contribution. `new` is an actual
/// contribution, even when its value is the application's default.
pub struct StructuredState<T: StructuredType>(Option<T::Data>);

impl<T: StructuredType> Clone for StructuredState<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: StructuredType> Default for StructuredState<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T: StructuredType> StructuredState<T> {
    pub fn new(value: T::Data) -> Self {
        Self(Some(value))
    }

    /// Whether at least one actual contribution was decoded or staged.
    pub fn has_source(&self) -> bool {
        self.0.is_some()
    }

    pub fn value(&self) -> Option<&T::Data> {
        self.0.as_ref()
    }

    pub fn into_value(self) -> T::Data
    where
        T::Data: Default,
    {
        self.0.unwrap_or_default()
    }

    // Disposable cache framing. A digest keeps a wrong-Type record from being
    // decoded without making the encrypted record's size depend on the Type.
    pub(crate) fn encode_cached(&self) -> Result<Vec<u8>> {
        dag_cbor::encode(&Cached {
            data: self.0.as_ref().ok_or_else(no_source)?,
            kind: serde_bytes::Bytes::new(type_digest::<T>().as_bytes()),
        })
    }

    pub(crate) fn decode_cached(bytes: &[u8]) -> Result<Self> {
        let cached: Cached<ipld_core::ipld::Ipld, serde_bytes::ByteBuf> = dag_cbor::decode(bytes)?;
        if cached.kind != type_digest::<T>().as_bytes().as_slice() {
            return Err(invalid("cached value has a different Type"));
        }
        Self::decode(&dag_cbor::encode(&cached.data)?)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cached<T, B> {
    data: T,
    kind: B,
}

fn type_digest<T: StructuredType>() -> blake3::Hash {
    blake3::hash(T::TYPE_ID.as_bytes())
}

fn no_source() -> crate::Error {
    invalid("no source contribution")
}

pub(crate) fn invalid(reason: &str) -> crate::Error {
    crate::crdt::CRDTError::DeserializationFailed {
        reason: reason.into(),
    }
    .into()
}

impl<T: StructuredType> CRDT for StructuredState<T> {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(match (&self.0, &other.0) {
            (Some(left), Some(right)) => Self::new(left.merge(right)?),
            (None, _) => other.clone(),
            (_, None) => self.clone(),
        })
    }
}

/// The Type's native contribution, with no common framing. Callers establish the
/// Type from history before decoding.
impl<T: StructuredType> Codec for StructuredState<T> {
    fn encode(&self) -> Result<Vec<u8>> {
        dag_cbor::encode(self.0.as_ref().ok_or_else(no_source)?)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        dag_cbor::decode(bytes).map(Self::new)
    }
}

fn deserialize_16<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<[u8; 16], D::Error> {
    struct Bytes;
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = [u8; 16];
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a 16-byte string")
        }
        fn visit_bytes<E: serde::de::Error>(
            self,
            bytes: &[u8],
        ) -> std::result::Result<Self::Value, E> {
            bytes.try_into().map_err(|_| E::custom("expected 16 bytes"))
        }
    }
    deserializer.deserialize_bytes(Bytes)
}

/// Fixed-width big-endian u128 field representation, including small values.
pub mod u128_bytes {
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &u128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&value.to_be_bytes())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u128, D::Error> {
        super::deserialize_16(deserializer).map(u128::from_be_bytes)
    }
}

/// Fixed-width big-endian two's-complement i128 field representation.
pub mod i128_bytes {
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &i128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&value.to_be_bytes())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i128, D::Error> {
        super::deserialize_16(deserializer).map(i128::from_be_bytes)
    }
}

/// A non-text-keyed map represented as canonical-key-ordered `[key, value]` pairs.
/// Construct from either container's iterator. Duplicate encoded keys are refused,
/// not collapsed by Rust equality. Key schemas must be injective.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyedPairs<K, V>(pub Vec<(K, V)>);

impl<K: Serialize, V: Serialize> Serialize for KeyedPairs<K, V> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut pairs = self
            .0
            .iter()
            .map(|(k, v)| {
                dag_cbor::encode(k)
                    .map(|bytes| (bytes, k, v))
                    .map_err(S::Error::custom)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        pairs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
        if pairs.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(S::Error::custom("duplicate encoded map key"));
        }
        pairs
            .into_iter()
            .map(|(_, k, v)| (k, v))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
}

impl<'de, K: Deserialize<'de> + Serialize, V: Deserialize<'de>> Deserialize<'de>
    for KeyedPairs<K, V>
{
    fn deserialize<S: serde::Deserializer<'de>>(
        deserializer: S,
    ) -> std::result::Result<Self, S::Error> {
        use serde::de::Error;
        let pairs = Vec::<(K, V)>::deserialize(deserializer)?;
        let keys = pairs
            .iter()
            .map(|(key, _)| dag_cbor::encode(key).map_err(S::Error::custom))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if keys
            .windows(2)
            .any(|w| w[0].len().cmp(&w[1].len()).then(w[0].cmp(&w[1])) != std::cmp::Ordering::Less)
        {
            return Err(S::Error::custom(
                "map keys must be unique and canonically ordered",
            ));
        }
        Ok(Self(pairs))
    }
}
