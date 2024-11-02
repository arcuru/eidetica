use std::error::Error as StdError;
use std::fmt;

/// Error types that can occur during database operations
#[derive(Debug)]
pub enum Error {
    /// Database connection/query errors
    #[allow(dead_code)]
    Database(Box<dyn std::error::Error>),

    /// Entry not found
    NotFound,

    /// Invalid data format
    InvalidData,

    /// Entry already exists
    AlreadyExists,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Database(e) => write!(f, "Database error: {}", e),
            Error::NotFound => write!(f, "Entry not found"),
            Error::InvalidData => write!(f, "Invalid data format"),
            Error::AlreadyExists => write!(f, "Entry already exists"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Database(e) => Some(e.as_ref()),
            Error::NotFound => None,
            Error::InvalidData => None,
            Error::AlreadyExists => None,
        }
    }
}
