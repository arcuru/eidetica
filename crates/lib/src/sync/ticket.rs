//! URL-based shareable database link.
//!
//! A `DatabaseTicket` encodes a database ID and optional peer address hints
//! into a compact URL that can be shared between peers.
//!
//! # URL Format
//!
//! Magnet-style URI with the database ID and peer addresses as query parameters:
//!
//! ```text
//! eidetica:?db=<database_id>&pr=<peer_address>&pr=<peer_address>
//! ```
//!
//! - **`db`** (required): The database ID, passed through as-is (e.g., `sha256:hex`)
//! - **`pr`** (optional, repeatable): A peer address prefixed by its transport
//!   type (e.g., `http:host:port`, `iroh:endpointABC...`)
//!
//! # Examples
//!
//! ```
//! # use eidetica::sync::{DatabaseTicket, Address};
//! # use eidetica::entry::ID;
//! let id = ID::new("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
//! let ticket = DatabaseTicket::new(id);
//! let url = ticket.to_string();
//! assert!(url.starts_with("eidetica:?db=sha256:"));
//!
//! let parsed: DatabaseTicket = url.parse().unwrap();
//! assert_eq!(parsed.database_id(), ticket.database_id());
//! ```

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::entry::ID;
use crate::sync::SyncError;
use crate::sync::peer_types::Address;

/// Ticket URI scheme and query separator (`eidetica:?`).
const SCHEME: &str = "eidetica:?";

/// Query parameter key for the database ID.
const DB_PARAM: &str = "db";

/// Query parameter key for peer address hints.
const PR_PARAM: &str = "pr";

/// A shareable link containing a database ID and optional transport address hints.
///
/// `DatabaseTicket` can be serialized to and parsed from a magnet-style URI:
///
/// ```text
/// eidetica:?db=sha256:abc...&pr=http:192.168.1.1:8080&pr=iroh:endpointABC...
/// ```
///
/// The database ID is passed through as an opaque string. Peer addresses
/// use the transport's native encoding, prefixed by the transport name and
/// a colon if the encoding doesn't already include it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct DatabaseTicket {
    database_id: ID,
    addresses: Vec<Address>,
}

impl DatabaseTicket {
    /// Create a ticket with a database ID and no address hints.
    pub fn new(database_id: ID) -> Self {
        Self {
            database_id,
            addresses: Vec::new(),
        }
    }

    /// Create a ticket with a database ID and address hints.
    pub fn with_addresses(database_id: ID, addresses: Vec<Address>) -> Self {
        Self {
            database_id,
            addresses,
        }
    }

    /// Get the database ID.
    pub fn database_id(&self) -> &ID {
        &self.database_id
    }

    /// Get the transport address hints.
    pub fn addresses(&self) -> &[Address] {
        &self.addresses
    }

    /// Add a transport address hint.
    pub fn add_address(&mut self, address: Address) {
        self.addresses.push(address);
    }
}

/// Minimally encode a query-parameter value.
///
/// Only the characters that are structurally significant inside a query string
/// (`&`, `=`, `#`, `+`) and the escape character (`%`) are percent-encoded.
/// Everything else — including `:` — passes through verbatim, keeping tickets
/// human-readable.
///
/// Spaces are not encoded because database IDs and transport addresses do not
/// contain them. The `url::form_urlencoded::parse` decoder used in `FromStr`
/// treats `+` as a space (per the `application/x-www-form-urlencoded` spec),
/// so we encode literal `+` as `%2B` above to avoid that ambiguity. Tickets
/// produced by other implementations that percent-encode more aggressively
/// (e.g., encoding `:` or `/`) are accepted because the parser uses
/// `form_urlencoded::parse` which handles all standard percent-encoding.
fn encode_query_value(s: &str) -> Cow<'_, str> {
    if !s.contains(['%', '&', '=', '#', '+']) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            _ => out.push(ch),
        }
    }
    Cow::Owned(out)
}

/// Format a transport [`Address`] as a `type:value` string.
///
/// The transport name is prefixed before the transport's native encoding,
/// separated by a single colon: `http:192.168.1.1:8080`,
/// `iroh:endpointABC...`, etc.
fn encode_address(addr: &Address) -> String {
    format!("{}:{}", addr.transport_type, addr.address)
}

/// Parse a `type:value` string back into a transport [`Address`].
///
/// Splits on the first `:` to recover the transport name and the
/// transport-specific address. Returns `None` for values without a `:`
/// separator.
fn decode_address(value: &str) -> Option<Address> {
    let (transport_type, address) = value.split_once(':')?;
    Some(Address::new(transport_type, address))
}

impl fmt::Display for DatabaseTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SCHEME}{DB_PARAM}={}",
            encode_query_value(self.database_id.as_str())
        )?;

        for addr in &self.addresses {
            let encoded = encode_address(addr);
            write!(f, "&{PR_PARAM}={}", encode_query_value(&encoded))?;
        }

        Ok(())
    }
}

impl FromStr for DatabaseTicket {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self> {
        let query = s.strip_prefix(SCHEME).ok_or_else(|| {
            let preview: String = s.chars().take(20).collect();
            SyncError::InvalidAddress(format!("Expected '{SCHEME}' prefix, got: {preview}"))
        })?;

        let mut database_id = None;
        let mut addresses = Vec::new();

        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                DB_PARAM => {
                    // TODO: if `db` appears more than once, the last value
                    // silently wins.
                    database_id = Some(ID::new(value.to_string()));
                }
                PR_PARAM => {
                    if let Some(addr) = decode_address(&value) {
                        addresses.push(addr);
                    }
                    // Silently skip malformed pr values for forward compat
                }
                _ => {} // Unknown params ignored for forward compat
            }
        }

        let database_id = database_id.ok_or_else(|| {
            SyncError::InvalidAddress(format!("Ticket URL missing '{DB_PARAM}' parameter"))
        })?;

        Ok(Self {
            database_id,
            addresses,
        })
    }
}

impl From<DatabaseTicket> for String {
    fn from(ticket: DatabaseTicket) -> Self {
        ticket.to_string()
    }
}

impl TryFrom<String> for DatabaseTicket {
    type Error = crate::Error;

    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const BLAKE3_HEX: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    /// Create an iroh Address in EndpointTicket format.
    fn test_iroh_address() -> Address {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};
        use iroh_tickets::{Ticket, endpoint::EndpointTicket};

        let secret_key = SecretKey::from_bytes(&[1u8; 32]);
        let endpoint_addr = EndpointAddr::from_parts(
            secret_key.public(),
            vec![TransportAddr::Ip("127.0.0.1:1234".parse().unwrap())],
        );
        Address::iroh(Ticket::serialize(&EndpointTicket::new(endpoint_addr)))
    }

    fn sha256_id() -> ID {
        ID::new(format!("sha256:{SHA256_HEX}"))
    }

    fn blake3_id() -> ID {
        ID::new(format!("blake3:{BLAKE3_HEX}"))
    }

    #[test]
    fn round_trip_no_hints() {
        let ticket = DatabaseTicket::new(sha256_id());
        let url = ticket.to_string();
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(parsed.database_id(), ticket.database_id());
        assert!(parsed.addresses().is_empty());
    }

    #[test]
    fn round_trip_one_hint() {
        let ticket =
            DatabaseTicket::with_addresses(sha256_id(), vec![Address::http("192.168.1.1:8080")]);
        let url = ticket.to_string();
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(parsed.database_id(), ticket.database_id());
        assert_eq!(parsed.addresses().len(), 1);
        assert_eq!(parsed.addresses()[0].transport_type, "http");
        assert_eq!(parsed.addresses()[0].address, "192.168.1.1:8080");
    }

    #[test]
    fn round_trip_multiple_hints() {
        let iroh_addr = test_iroh_address();
        let ticket = DatabaseTicket::with_addresses(
            sha256_id(),
            vec![iroh_addr.clone(), Address::http("192.168.1.1:8080")],
        );
        let url = ticket.to_string();
        // Iroh address should be in EndpointTicket format in the URL
        assert!(url.contains("pr=iroh:endpoint"));
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(parsed.database_id(), ticket.database_id());
        assert_eq!(parsed.addresses().len(), 2);
        assert_eq!(parsed.addresses()[0].transport_type, "iroh");
        // Round-trip through EndpointTicket preserves the internal address
        assert_eq!(parsed.addresses()[0].address, iroh_addr.address);
        assert_eq!(parsed.addresses()[1].transport_type, "http");
        assert_eq!(parsed.addresses()[1].address, "192.168.1.1:8080");
    }

    #[test]
    fn database_id_passed_through_opaquely() {
        // sha256 ID round-trips without transformation
        let ticket = DatabaseTicket::new(sha256_id());
        let url = ticket.to_string();
        assert!(url.contains(&format!("db=sha256:{SHA256_HEX}")));
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(
            parsed.database_id().as_str(),
            format!("sha256:{SHA256_HEX}")
        );

        // blake3 ID round-trips without transformation
        let ticket = DatabaseTicket::new(blake3_id());
        let url = ticket.to_string();
        assert!(url.contains(&format!("db=blake3:{BLAKE3_HEX}")));
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(
            parsed.database_id().as_str(),
            format!("blake3:{BLAKE3_HEX}")
        );
    }

    #[test]
    fn wrong_scheme_error() {
        let result = "https:?db=sha256:abc123".parse::<DatabaseTicket>();
        assert!(result.is_err());
    }

    #[test]
    fn missing_db_param_error() {
        let result = "eidetica:?pr=http:localhost:8080".parse::<DatabaseTicket>();
        assert!(result.is_err());
    }

    #[test]
    fn unknown_hash_algorithm_round_trips() {
        // Future hash algorithms round-trip as opaque strings
        let id = ID::new("future_hash:deadbeef");
        let ticket = DatabaseTicket::new(id);
        let url = ticket.to_string();
        assert!(url.contains("db=future_hash:deadbeef"));
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(parsed.database_id().as_str(), "future_hash:deadbeef");
    }

    #[test]
    fn unknown_query_params_ignored() {
        let url =
            format!("eidetica:?db=sha256:{SHA256_HEX}&future_param=value&pr=http:localhost:8080");
        let parsed: DatabaseTicket = url.parse().unwrap();
        // Unknown params are silently ignored (forward compat)
        assert_eq!(parsed.addresses().len(), 1);
        assert_eq!(parsed.addresses()[0].transport_type, "http");
        assert_eq!(parsed.addresses()[0].address, "localhost:8080");
    }

    #[test]
    fn url_encoding_special_characters() {
        let ticket = DatabaseTicket::with_addresses(
            sha256_id(),
            vec![Address::new("http", "host:8080/path?q=1&r=2")],
        );
        let url = ticket.to_string();
        // The & and = in the address value should be encoded
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(parsed.addresses()[0].address, "host:8080/path?q=1&r=2");
    }

    #[test]
    fn multiple_values_same_transport() {
        let ticket = DatabaseTicket::with_addresses(
            sha256_id(),
            vec![
                Address::http("192.168.1.1:8080"),
                Address::http("10.0.0.1:8080"),
            ],
        );
        let url = ticket.to_string();
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert_eq!(parsed.addresses().len(), 2);
        assert_eq!(parsed.addresses()[0].address, "192.168.1.1:8080");
        assert_eq!(parsed.addresses()[1].address, "10.0.0.1:8080");
    }

    #[test]
    fn add_address_method() {
        let mut ticket = DatabaseTicket::new(sha256_id());
        assert!(ticket.addresses().is_empty());
        ticket.add_address(Address::http("localhost:8080"));
        assert_eq!(ticket.addresses().len(), 1);
    }

    #[test]
    fn display_format_no_hints() {
        let ticket = DatabaseTicket::new(sha256_id());
        let url = ticket.to_string();
        assert_eq!(url, format!("eidetica:?db=sha256:{SHA256_HEX}"));
    }

    #[test]
    fn display_format_with_hints() {
        let ticket =
            DatabaseTicket::with_addresses(sha256_id(), vec![Address::http("localhost:8080")]);
        let url = ticket.to_string();
        assert_eq!(
            url,
            format!("eidetica:?db=sha256:{SHA256_HEX}&pr=http:localhost:8080")
        );
    }

    #[test]
    fn malformed_pr_value_skipped() {
        // A pr value without : is silently skipped
        let url = format!("eidetica:?db=sha256:{SHA256_HEX}&pr=no_colon_here");
        let parsed: DatabaseTicket = url.parse().unwrap();
        assert!(parsed.addresses().is_empty());
    }

    #[test]
    fn display_format_multiple_transports() {
        let iroh_addr = test_iroh_address();
        let ticket = DatabaseTicket::with_addresses(
            sha256_id(),
            vec![iroh_addr, Address::http("192.168.1.1:8080")],
        );
        let url = ticket.to_string();
        // Iroh addresses appear as EndpointTicket format in ticket URLs
        assert!(url.starts_with(&format!(
            "eidetica:?db=sha256:{SHA256_HEX}&pr=iroh:endpoint"
        )));
        assert!(url.ends_with("&pr=http:192.168.1.1:8080"));
    }

    #[test]
    fn serde_round_trip() {
        let ticket =
            DatabaseTicket::with_addresses(sha256_id(), vec![Address::http("192.168.1.1:8080")]);
        let json = serde_json::to_string(&ticket).unwrap();
        // Serializes as a plain URL string
        assert!(json.starts_with('"'));
        assert!(json.contains("eidetica:?db="));

        let deserialized: DatabaseTicket = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ticket);
    }
}
