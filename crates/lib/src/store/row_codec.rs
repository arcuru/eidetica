//! Pluggable encoding of application rows as opaque bytes.
//!
//! Row encoding is separate from the Store's outer serialization format. A
//! codec must be able to decode its own output; applications are responsible
//! for choosing compatible codecs when reading previously encoded rows.

use serde::{Serialize, de::DeserializeOwned};

use crate::Result;

/// Stateless conversion between an application row and its byte representation.
///
/// Serde bounds belong to individual implementations, not to this trait, so
/// custom codecs can support types that do not implement Serde.
///
/// ```
/// use eidetica::store::{RowCodec, SerdeJson};
///
/// let row = vec![u128::MAX];
/// assert_eq!(<SerdeJson as RowCodec<Vec<u128>>>::FORMAT_ID, "json:v0");
/// let bytes = SerdeJson::encode(&row)?;
/// let decoded: Vec<u128> = SerdeJson::decode(&bytes)?;
/// assert_eq!(decoded, row);
/// # Ok::<(), eidetica::Error>(())
/// ```
pub trait RowCodec<T>: Send + Sync {
    /// Stable identity of the application byte format, such as `json:v0` or
    /// `myapp/invoice-binary:v2`. Use an application-owned format name, not a
    /// Rust type name or crate version; incompatible encodings need distinct IDs.
    ///
    /// Matching identities do not establish application schema compatibility:
    /// one format can support many row types, and callers still choose their T.
    const FORMAT_ID: &'static str;

    /// Encodes a row without adding a Store-level envelope.
    fn encode(row: &T) -> Result<Vec<u8>>;

    /// Decodes a row, returning an error for invalid or incompatible bytes.
    fn decode(bytes: &[u8]) -> Result<T>;
}

/// Direct Serde JSON row encoding, without canonicalization or a JSON value
/// intermediate. Integer fields retain the ranges supported by their Rust types.
#[derive(Debug, Clone, Copy, Default)]
pub struct SerdeJson;

impl<T: Serialize + DeserializeOwned> RowCodec<T> for SerdeJson {
    const FORMAT_ID: &'static str = "json:v0";

    fn encode(row: &T) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(row)?)
    }

    fn decode(bytes: &[u8]) -> Result<T> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// Identity encoding for opaque bytes, including empty and non-UTF-8 rows.
///
/// ```
/// use eidetica::store::{RawBytes, RowCodec};
///
/// let row = vec![0, 255, 0];
/// assert_eq!(RawBytes::FORMAT_ID, "raw:v0");
/// assert_eq!(RawBytes::encode(&row)?, row);
/// assert_eq!(RawBytes::decode(&row)?, row);
/// # Ok::<(), eidetica::Error>(())
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct RawBytes;

impl RowCodec<Vec<u8>> for RawBytes {
    const FORMAT_ID: &'static str = "raw:v0";

    fn encode(row: &Vec<u8>) -> Result<Vec<u8>> {
        Ok(row.clone())
    }

    fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }
}
