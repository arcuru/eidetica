//! The typed Account handle exposed by an [`AccountSession`](super::AccountSession).

use crate::{
    Database, Instance, Result, auth::AuthSettings, crdt::Doc, database::DatabaseKey, entry::ID,
};

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

    /// Create a portable Account with a descriptive name and explicit Auth.
    /// The actual signer (and its exact path, if delegated) must be authorized
    /// by that Auth. No login, Identity, or extra creator grant is created.
    pub async fn create(
        instance: &Instance,
        signer: impl Into<DatabaseKey>,
        name: &str,
        auth: AuthSettings,
    ) -> Result<Self> {
        let mut settings = Doc::new();
        settings.set("name", name);
        settings.set("auth", auth.as_doc().clone());
        Database::create(instance, signer, settings)
            .await
            .map(Self::new)
    }

    /// Read the portable descriptive name, not the local login name.
    pub async fn get_name(&self) -> Result<String> {
        self.database.get_name().await
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
