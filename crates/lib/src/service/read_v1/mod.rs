//! Opt-in read-only service v1 over a Unix domain socket.
//!
//! This adapter does not replace `Instance::connect` or expose login, writes,
//! watches or peer operations. Every read proves a caller-held signer. Results
//! identify the serving incarnation within the locally trusted socket, not an
//! independently authenticated remote or an independently verified replica.

mod car;
mod client;
mod server;
mod wire;

pub use client::{HistoryStream, ReadClient, StateStream};
pub use server::ReadServer;

use crate::{ID, state::ReadContext};

/// Source and exact root cut of a read. Producer identity is local to this
/// socket peer and server incarnation; it is not a portable trust credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub producer: [u8; 16],
    pub at: ReadContext,
}

/// A daemon-built projection, not independently reconstructed state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    pub source: Source,
    pub store: String,
    pub format: String,
}

/// A collected result that reached a matching Complete terminal and clean
/// transport end. The caller chooses whether to use a daemon-built projection.
#[derive(Debug)]
pub struct Delivered<C, T> {
    pub context: C,
    pub value: T,
}

/// Read-side failures. No failure implicitly retries at another cut or with
/// weaker authorization. A transport failure never implies server refusal.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadError {
    #[error("daemon refused State computation")]
    Refused,
    #[error("read permission or signer proof denied")]
    Denied,
    #[error("invalid database-root boundary")]
    InvalidBoundary,
    #[error("requested history is unavailable")]
    UnavailableHistory,
    #[error("unsupported read request or service revision")]
    Unsupported,
    #[error("read resource limit exceeded")]
    Limit,
    #[error("invalid service read data: {0}")]
    InvalidData(&'static str),
    #[error("read did not reach clean completion")]
    Incomplete,
    #[error("cannot connect to the local read service: {0}")]
    Connect(#[from] tonic::transport::Error),
}

impl Source {
    pub fn database(&self) -> &ID {
        self.at.database()
    }
}

#[cfg(test)]
mod car_fixture;
#[cfg(test)]
mod tests;
