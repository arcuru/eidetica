use ed25519_dalek::{PUBLIC_KEY_LENGTH, SECRET_KEY_LENGTH};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

pub type DeviceId = [u8; PUBLIC_KEY_LENGTH];
pub type PrivateKey = [u8; SECRET_KEY_LENGTH];

/// A single entry in the metadata table
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    /// UUIDv7 that serves as unique identifier across all devices
    pub id: Uuid,

    /// An ed25519 public key used to identify the data stream that wrote this data.
    pub device_id: DeviceId,

    /// Whether this entry has been superseded by a newer version
    pub archived: bool,

    /// Whether this entry is configured to sync to the local device
    pub local: bool,

    /// Optional reference to parent entry's UUID
    pub parent_id: Option<Uuid>,

    /// JSON metadata about the referenced data
    pub metadata: Value,

    /// Hash of the data with algorithm prefix
    /// Current format: "b3_" + hex(BLAKE3)
    /// Allows for future hash algorithms with different prefixes
    pub data_hash: String,
}

/// A single entry in the data table
/// This data is not all directly synced
#[derive(Debug, Clone, PartialEq)]
pub struct DataEntry {
    /// Hash entry that is the unique identifier for this data
    /// Current format: "b3_" + hex(BLAKE3)
    /// Allows for future hash algorithms with different prefixes
    pub hash: String,

    /// Reference Count
    /// How many metadata entries expect this data
    pub ref_count: i32, // TODO: I'm not sure how safe this type is...

    /// Inline data for small entries
    pub inline_data: Option<Vec<u8>>,

    /// List of device IDs that _may_ have this data
    pub devices: Vec<DeviceId>,

    /// Local file path to the data
    pub local_path: Vec<String>,

    /// Path to S3 object
    /// The bucket/login info stuff is stored a level up at the Data Store level.
    pub s3_path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamType {
    Stream,
    User,
    Instance,
    Store,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamEntry {
    /// Local index to use for local references
    pub index: i64,

    pub id: DeviceId,

    pub secret_key: Option<PrivateKey>,

    pub stream_type: StreamType,
}

impl From<sqlx::postgres::PgRow> for StreamEntry {
    fn from(row: sqlx::postgres::PgRow) -> Self {
        Self {
            index: row.get("index"),
            id: row.get("id"),
            secret_key: row.get("secret_key"),
            stream_type: match row.get::<String, _>("stream_type").as_str() {
                "Stream" => StreamType::Stream,
                "User" => StreamType::User,
                "Instance" => StreamType::Instance,
                "Store" => StreamType::Store,
                _ => StreamType::Stream,
            },
        }
    }
}

impl DataEntry {
    /// Create a new DataEntry with the given hash
    ///
    /// This is the entry created on default insertion into the database.
    #[allow(dead_code)]
    pub fn new<S: Into<String>>(hash: S) -> Self {
        Self {
            hash: hash.into(),
            ref_count: 0,
            inline_data: None,
            devices: Vec::new(),
            local_path: Vec::new(),
            s3_path: Vec::new(),
        }
    }
}
