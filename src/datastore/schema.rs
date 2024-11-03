use serde_json::Value;
use uuid::Uuid;

/// A single entry in the metadata table
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    /// UUIDv7 that serves as unique identifier across all devices
    pub id: Uuid,

    /// UUID identifying the device that created this entry
    pub device_id: Uuid,

    /// Whether this entry has been superseded by a newer version
    pub archived: bool,

    /// Whether this entry is configured to sync to the local device
    pub local: bool,

    /// Optional reference to parent entry's UUID
    pub parent_id: Option<Uuid>,

    /// JSON metadata about the referenced data
    pub metadata: Value,

    /// Hash of the data with algorithm prefix
    /// Current format: "b2_" + hex(BLAKE2b-256)
    /// Allows for future hash algorithms with different prefixes
    pub data_hash: String,
}

/// A single entry in the data table
/// This data is not all directly synced
#[derive(Debug, Clone, PartialEq)]
pub struct DataEntry {
    /// Hash entry that is the unique identifier for this data
    /// Current format: "b2_" + hex(BLAKE2b-256)
    /// Allows for future hash algorithms with different prefixes
    pub hash: String,

    /// Inline data for small entries
    pub inline_data: Option<Vec<u8>>,

    /// List of device IDs that _may_ have this data
    pub devices: Vec<Uuid>,

    /// Local file path to the data
    pub local_path: Vec<String>,

    /// Path to S3 object
    /// The bucket/login info stuff is stored a level up at the Data Store level.
    pub s3_path: Vec<String>,
}

impl DataEntry {
    /// Create a new DataEntry with the given hash
    ///
    /// This is the entry created on default insertion into the database.
    #[allow(dead_code)]
    pub fn new<S: Into<String>>(hash: S) -> Self {
        Self {
            hash: hash.into(),
            inline_data: None,
            devices: Vec::new(),
            local_path: Vec::new(),
            s3_path: Vec::new(),
        }
    }
}
