//! The typed Account handle exposed by an [`AccountSession`](super::AccountSession).

use crate::{Database, entry::ID};

/// An Account: the private root database of a person, group or application.
///
/// Its stable identity is the root ID returned by [`Account::id`], not a
/// display name or the instance-local login ID. The backing [`Database`] is
/// encapsulated: applications reach their stored references and the databases
/// they name through [`AccountSession`](super::AccountSession) operations,
/// which keep the Account's internal key and tracking stores out of reach.
///
/// Obtain it with [`AccountSession::account`](super::AccountSession::account).
#[derive(Debug)]
pub struct Account {
    database: Database,
}

impl Account {
    pub(crate) fn new(database: Database) -> Self {
        Self { database }
    }

    /// Stable Account identity: the root ID of the Account database.
    pub fn id(&self) -> &ID {
        self.database.root_id()
    }

    /// The backing Account database, bound to the session's key.
    ///
    /// Test-only access for inspecting Account history; applications use the
    /// session's reference operations instead.
    #[cfg(any(test, feature = "testing"))]
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// The backing Account database, bound to the session's key.
    #[cfg(not(any(test, feature = "testing")))]
    pub(crate) fn database(&self) -> &Database {
        &self.database
    }
}
