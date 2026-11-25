//! Exact historical interpretation contracts for opt-in structured Stores.
//!
//! The effective descriptor is a contribution's historical default or explicit
//! override. It is not an Entry protocol version or a conversion request.

use std::marker::PhantomData;

use ipld_core::ipld::Ipld;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_bytes::{ByteBuf, Bytes};

use crate::{
    Result,
    crdt::{CRDT, Codec, dag_cbor},
};

/// Complete interpretation identity. Names are opaque, nonempty, byte-exact UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractId {
    pub store: &'static str,
    pub data: &'static str,
    pub schema: &'static str,
}

/// Owned descriptor recorded in a contribution, suitable for authorized inspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractDescriptor {
    #[serde(rename = "store")]
    pub store: String,
    #[serde(rename = "data")]
    pub data: String,
    #[serde(rename = "schema")]
    pub schema: String,
}

impl From<ContractId> for ContractDescriptor {
    fn from(id: ContractId) -> Self {
        Self {
            store: id.store.into(),
            data: id.data.into(),
            schema: id.schema.into(),
        }
    }
}

impl ContractDescriptor {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.store.is_empty() || self.data.is_empty() || self.schema.is_empty() {
            return Err(invalid("empty contract identifier"));
        }
        Ok(())
    }
}

/// Static code support for exactly one Store/data/application contract.
/// Merely naming a schema supplies neither its implementation nor permission.
pub trait StoreContract: Send + Sync + 'static {
    const ID: ContractId;
}

pub(crate) fn invalid(reason: &str) -> crate::Error {
    crate::crdt::CRDTError::DeserializationFailed {
        reason: reason.into(),
    }
    .into()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Explicit<T> {
    #[serde(rename = "contract")]
    contract: ContractDescriptor,
    #[serde(rename = "value")]
    value: T,
}

// Staging and the opaque projection carry an explicit tuple; they are not
// contribution encodings. Parsing the outer record before D prevents a cache
// hit (or bad staging bytes) from bypassing the interpretation gate.
pub(crate) fn inspect_explicit(bytes: &[u8]) -> Result<ContractDescriptor> {
    let envelope: Explicit<Ipld> = dag_cbor::decode(bytes)?;
    envelope.contract.validate()?;
    Ok(envelope.contract)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contribution<T> {
    #[serde(rename = "override", default, skip_serializing_if = "Option::is_none")]
    override_id: Option<ContractDescriptor>,
    value: T,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Protected<T, B> {
    metadata: B,
    value: T,
}

const ENCRYPTED_DESCRIPTOR_CAP: usize = 512;
pub(crate) const CONTROL_SIZE: usize = 2 + ENCRYPTED_DESCRIPTOR_CAP;

// A zero length denotes omission, not an encoded descriptor. Length and
// capacity bytes are all inside the existing authenticated encryption boundary.
pub(crate) fn protect(id: Option<&ContractDescriptor>) -> Result<[u8; CONTROL_SIZE]> {
    let mut region = [0; CONTROL_SIZE];
    if let Some(id) = id {
        id.validate()?;
        let encoded = dag_cbor::encode(id)?;
        if encoded.len() > ENCRYPTED_DESCRIPTOR_CAP {
            return Err(invalid(
                "encrypted attribution descriptor exceeds 512 bytes",
            ));
        }
        region[..2].copy_from_slice(&(encoded.len() as u16).to_be_bytes());
        region[2..2 + encoded.len()].copy_from_slice(&encoded);
    }
    Ok(region)
}

pub(crate) fn unprotect(region: &[u8]) -> Result<Option<ContractDescriptor>> {
    let region: &[u8; CONTROL_SIZE] = region
        .try_into()
        .map_err(|_| invalid("invalid encrypted attribution footprint"))?;
    let length = usize::from(u16::from_be_bytes([region[0], region[1]]));
    if length > ENCRYPTED_DESCRIPTOR_CAP || region[2 + length..].iter().any(|&b| b != 0) {
        return Err(invalid("invalid encrypted attribution padding"));
    }
    if length == 0 {
        return Ok(None);
    }
    let id: ContractDescriptor = dag_cbor::decode(&region[2..2 + length])?;
    id.validate()?;
    Ok(Some(id))
}

fn protected_value<T: Serialize>(id: Option<&ContractDescriptor>, value: T) -> Result<Vec<u8>> {
    dag_cbor::encode(&Protected {
        metadata: Bytes::new(&protect(id)?),
        value,
    })
}

/// Finalization changes only the framing of the already validated staged value.
pub(crate) fn finalize_contribution(
    bytes: &[u8],
    staged: &ContractDescriptor,
    default: &ContractDescriptor,
    encrypted: bool,
) -> Result<Vec<u8>> {
    let Explicit { contract, value } = dag_cbor::decode::<Explicit<Ipld>>(bytes)?;
    if &contract != staged || contract.store != default.store {
        return Err(invalid("staged contract or default Store family mismatch"));
    }
    let override_id = (contract != *default).then_some(contract);
    if encrypted {
        protected_value(override_id.as_ref(), value)
    } else {
        dag_cbor::encode(&Contribution { override_id, value })
    }
}

pub(crate) fn contribution(
    bytes: &[u8],
    default: &ContractDescriptor,
    encrypted: bool,
) -> Result<(Option<ContractDescriptor>, Ipld)> {
    let (override_id, value) = if encrypted {
        let Protected { metadata, value } = dag_cbor::decode::<Protected<Ipld, ByteBuf>>(bytes)?;
        (unprotect(&metadata)?, value)
    } else {
        let Contribution { override_id, value } = dag_cbor::decode::<Contribution<Ipld>>(bytes)?;
        (override_id, value)
    };
    if let Some(id) = &override_id {
        id.validate()?;
        if id == default || id.store != default.store {
            return Err(invalid(
                "redundant override or incompatible override Store family",
            ));
        }
    }
    Ok((override_id, value))
}

/// The resolved identity of an actual, authorized contribution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionAttribution {
    pub database: crate::entry::ID,
    pub store: String,
    pub entry: crate::entry::ID,
    pub declaration_origins: Vec<crate::entry::ID>,
    pub default: ContractDescriptor,
    pub override_id: Option<ContractDescriptor>,
}

impl ContributionAttribution {
    pub fn effective(&self) -> &ContractDescriptor {
        self.override_id.as_ref().unwrap_or(&self.default)
    }
}

/// Common whole-Store adapter. Nested values remain structural Serde values.
///
/// Default is an *unattributed* empty view; it must not be persisted as a schema
/// declaration. `new` creates an actual contribution, even for a default value.
pub struct Attributed<D, C: StoreContract> {
    value: Option<D>,
    contract: PhantomData<C>,
}

impl<D: Clone, C: StoreContract> Clone for Attributed<D, C> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            contract: PhantomData,
        }
    }
}

impl<D, C: StoreContract> Default for Attributed<D, C> {
    fn default() -> Self {
        Self {
            value: None,
            contract: PhantomData,
        }
    }
}

impl<D, C: StoreContract> Attributed<D, C> {
    pub fn new(value: D) -> Self {
        Self {
            value: Some(value),
            contract: PhantomData,
        }
    }
    /// Whether at least one actual contribution was decoded or explicitly staged.
    pub fn has_source(&self) -> bool {
        self.value.is_some()
    }
    pub fn value(&self) -> Option<&D> {
        self.value.as_ref()
    }
    pub fn into_value(self) -> D
    where
        D: Default,
    {
        self.value.unwrap_or_default()
    }
    pub fn descriptor(&self) -> Option<ContractDescriptor> {
        self.has_source().then(|| C::ID.into())
    }
}

impl<D: CRDT + Clone, C: StoreContract> CRDT for Attributed<D, C> {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(match (&self.value, &other.value) {
            (Some(left), Some(right)) => Self::new(left.merge(right)?),
            (None, _) => other.clone(),
            (_, None) => self.clone(),
        })
    }
}

impl<D: Serialize + DeserializeOwned, C: StoreContract> Codec for Attributed<D, C> {
    fn encode(&self) -> Result<Vec<u8>> {
        let contract = ContractDescriptor::from(C::ID);
        contract.validate()?;
        let value = self
            .value
            .as_ref()
            .ok_or_else(|| invalid("no source contribution"))?;
        dag_cbor::encode(&Explicit { contract, value })
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let contract = inspect_explicit(bytes)?;
        if contract != ContractDescriptor::from(C::ID) {
            return Err(invalid("unsupported Store/data/application contract"));
        }
        // Only after the complete structural/identity gate may D run. N1's
        // round-trip admission also rejects lossy application deserialization.
        let envelope: Explicit<D> = dag_cbor::decode(bytes)?;
        Ok(Self::new(envelope.value))
    }
}

impl<D: Serialize + DeserializeOwned, C: StoreContract> Attributed<D, C> {
    pub(crate) fn encode_cached(&self, encrypted: bool) -> Result<Vec<u8>> {
        if !encrypted {
            return self.encode();
        }
        let value = self
            .value
            .as_ref()
            .ok_or_else(|| invalid("no source contribution"))?;
        protected_value(Some(&C::ID.into()), value)
    }

    pub(crate) fn decode_cached(bytes: &[u8], encrypted: bool) -> Result<Self> {
        if !encrypted {
            return Self::decode(bytes);
        }
        let Protected { metadata, value } = dag_cbor::decode::<Protected<Ipld, ByteBuf>>(bytes)?;
        if unprotect(&metadata)?.as_ref() != Some(&C::ID.into()) {
            return Err(invalid(
                "unsupported cached Store/data/application contract",
            ));
        }
        Self::decode_value(&value)
    }

    pub(crate) fn decode_value(value: &Ipld) -> Result<Self> {
        Ok(Self::new(dag_cbor::decode(&dag_cbor::encode(value)?)?))
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
