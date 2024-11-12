use std::error::Error as StdError;
use std::fmt;

/// Custom Result type for database operations
pub type Result<T> = std::result::Result<T, Error>;

/// Error types that can occur during database operations
#[derive(Debug)]
pub enum Error {
    /// Database connection/query errors
    Database(Box<dyn std::error::Error>),

    // Wrap the common errors from the IO type
    IO(std::io::Error),

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
            Error::IO(e) => write!(f, "IO error: {}", e),
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
            Error::IO(e) => Some(e),
            Error::NotFound => None,
            Error::InvalidData => None,
            Error::AlreadyExists => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IO(e)
    }
}
