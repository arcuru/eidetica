//! User-scoped sharing preference support for capability-backed database handles.

use crate::{
    Result,
    entry::ID,
    sync::{DatabaseTicket, SyncError},
};

pub(crate) async fn ticket_locator(
    instance: &crate::Instance,
    database_id: &ID,
) -> Result<DatabaseTicket> {
    let sync = instance.sync().ok_or(SyncError::SyncNotEnabled)?;
    sync.create_ticket(database_id).await
}
