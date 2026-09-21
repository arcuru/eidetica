//! User-scoped sharing preference support for capability-backed database handles.

use crate::{
    Result,
    entry::ID,
    sync::{DatabaseTicket, SyncError},
};

/// Confirmation that a sharing preference was durably accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferenceWriteReceipt {
    /// ID of the signed preference entry, absent when the value was already current.
    pub entry_id: Option<ID>,
}

/// Outcome of a sharing-preference write.
#[derive(Debug)]
pub enum PreferenceWriteOutcome {
    /// The signed preference entry was acknowledged as durable.
    Written(PreferenceWriteReceipt),
    /// The connection was lost after submission may have reached the owner.
    /// Read the current preference or retry the idempotent write.
    Unknown { source: crate::Error },
}

impl PreferenceWriteOutcome {
    /// Receipt for an acknowledged write, or the original error when unknown.
    pub fn into_result(self) -> std::result::Result<PreferenceWriteReceipt, crate::Error> {
        match self {
            Self::Written(receipt) => Ok(receipt),
            Self::Unknown { source } => Err(source),
        }
    }
}

pub(crate) async fn ticket_locator(
    instance: &crate::Instance,
    database_id: &ID,
) -> Result<DatabaseTicket> {
    let sync = instance.sync().ok_or(SyncError::SyncNotEnabled)?;
    sync.create_ticket(database_id).await
}
