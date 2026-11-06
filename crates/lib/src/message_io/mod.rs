//! Logical message delivery shared by the service, Iroh and HTTP wires.
//!
//! Each transport already moves bytes reliably and in order, so this module adds
//! no packet protocol. It delimits one logical message on such a stream and moves
//! it without a second whole-message buffer:
//!
//! ```text
//! +----------------------+------------------------------+
//! | length: u64, big end | exactly `length` CBOR bytes  |
//! +----------------------+------------------------------+
//! ```
//!
//! The length is an admission and boundary declaration, not an I/O chunk size or
//! a rule about what is valid. Above [`codec::INLINE_MESSAGE_BYTES`] an encoder
//! or decoder runs on a blocking worker joined to the async stream by a small
//! bounded channel, so additional buffering is a few fixed chunks whatever the
//! message size. Smaller messages take a direct path through one bounded buffer:
//! they cost no worker hop and no worker slot. Their bytes still count against
//! the shared admission budget.
//!
//! The owned value being sent and the owned value being received are not
//! transport buffers and remain proportional to their contents.

mod bridge;
mod codec;
#[cfg(test)]
mod inline_tests;
mod policy;
mod progress;
#[cfg(test)]
mod tests;
pub(crate) mod wire;

use std::{sync::Arc, time::Duration};

pub use policy::{ResourceLimit, TransferBudget, TransferPolicy, TransferUsage};

pub(crate) use bridge::{
    Inbound, PreparedMessage, ReadOptions, encode_control, into_body_stream, prepare_admitted,
    prepare_message, read_message, reader_for_stream,
};
pub(crate) use codec::DiscriminantSeed;
pub(crate) use policy::Lease;
pub(crate) use progress::ProgressMonitor;

/// Marks the classifying decoder entry point; see [`codec::CLASSIFIER_NAME`].
pub(crate) use codec::CLASSIFIER_NAME;

use crate::sync::{SyncError, TimeoutPhase};

/// Why one logical message could not be moved.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TransferError {
    /// A configured limit refused the message. Says nothing about its validity.
    #[error("transfer refused by resource policy ({limit}): {detail}")]
    Resource {
        /// The limit that refused it.
        limit: ResourceLimit,
        /// What was asked for.
        detail: String,
    },
    /// A transfer policy was inconsistent.
    #[error("invalid transfer policy: {0}")]
    InvalidPolicy(String),
    /// The peer sent bytes that are not one well-formed message.
    #[error("malformed message: {0}")]
    Malformed(String),
    /// The stream ended inside a message.
    #[error("stream ended inside a message")]
    Truncated,
    /// A value could not be encoded.
    #[error("message encoding failed: {0}")]
    Encode(String),
    /// The native stream failed.
    #[error("transport failed: {0}")]
    Io(Arc<std::io::Error>),
    /// No body bytes moved for the stall interval.
    #[error("message body stalled for {elapsed:?}")]
    Stalled {
        /// The configured stall interval.
        elapsed: Duration,
    },
    /// A complete progress window moved too few bytes.
    #[error("message body moved {bytes} bytes in a {window:?} window")]
    InsufficientProgress {
        /// Bytes moved in the window.
        bytes: u64,
        /// Window length.
        window: Duration,
    },
    /// Sizing a message before sending it took too long.
    #[error("message preparation exceeded {elapsed:?}")]
    PreparationTimeout {
        /// The configured preparation bound.
        elapsed: Duration,
    },
    /// No response began within the first-response wait.
    #[error("no response within {elapsed:?}")]
    ResponseTimeout {
        /// The configured wait.
        elapsed: Duration,
    },
    /// The caller-selected overall deadline passed.
    #[error("exchange exceeded its overall deadline of {elapsed:?}")]
    OverallTimeout {
        /// The configured deadline.
        elapsed: Duration,
    },
    /// The transfer's worker stopped before finishing.
    #[error("message transfer was cancelled")]
    Cancelled,
}

impl TransferError {
    pub(crate) fn resource(limit: ResourceLimit, detail: impl Into<String>) -> Self {
        Self::Resource {
            limit,
            detail: detail.into(),
        }
    }

    pub(crate) fn is_resource(&self) -> bool {
        matches!(self, Self::Resource { .. })
    }

    /// The sync error for this failure against the peer at `address`.
    pub(crate) fn into_sync_error(self, address: &str) -> SyncError {
        let timeout = |phase, elapsed| SyncError::Timeout {
            address: address.to_string(),
            phase,
            elapsed,
        };
        match self {
            Self::Resource { .. } => SyncError::ResourceLimit(self.to_string()),
            Self::InvalidPolicy(reason) => SyncError::TransportInit(reason),
            Self::Stalled { elapsed } | Self::OverallTimeout { elapsed } => {
                timeout(TimeoutPhase::Transfer, elapsed)
            }
            Self::InsufficientProgress { window, .. } => timeout(TimeoutPhase::Transfer, window),
            Self::PreparationTimeout { elapsed } => timeout(TimeoutPhase::Prepare, elapsed),
            Self::ResponseTimeout { elapsed } => timeout(TimeoutPhase::Request, elapsed),
            Self::Encode(reason) => SyncError::SerializationError(reason),
            Self::Malformed(_) | Self::Truncated | Self::Io(_) | Self::Cancelled => {
                SyncError::Network(self.to_string())
            }
        }
    }
}

impl From<std::io::Error> for TransferError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(Arc::new(error))
    }
}

impl From<TransferError> for crate::Error {
    fn from(error: TransferError) -> Self {
        use std::io::{Error, ErrorKind};
        let kind = match &error {
            TransferError::Resource { .. } => {
                return SyncError::ResourceLimit(error.to_string()).into();
            }
            TransferError::InvalidPolicy(_) => ErrorKind::InvalidInput,
            TransferError::Malformed(_) | TransferError::Encode(_) => ErrorKind::InvalidData,
            TransferError::Truncated => ErrorKind::UnexpectedEof,
            TransferError::Io(source) => source.kind(),
            TransferError::Stalled { .. }
            | TransferError::InsufficientProgress { .. }
            | TransferError::PreparationTimeout { .. }
            | TransferError::ResponseTimeout { .. }
            | TransferError::OverallTimeout { .. } => ErrorKind::TimedOut,
            TransferError::Cancelled => ErrorKind::ConnectionAborted,
        };
        Error::new(kind, error).into()
    }
}
