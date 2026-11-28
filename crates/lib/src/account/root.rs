//! The typed Account handle exposed by an [`AccountSession`](super::AccountSession).

use crate::{
    Database, Error, Instance, Result, Transaction,
    auth::AuthSettings,
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    store::{DocStore, Store, StoreError},
};

/// An Account: the private root database of a person, group or application.
///
/// Its stable identity is the root ID returned by [`Account::id`], not a
/// display name or the instance-local login ID. The backing [`Database`] is
/// encapsulated: applications can read named references and open their targets
/// through a session or an explicit Identity, but a reference is not a grant.
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

    pub(crate) fn reference_store(namespace: &str) -> String {
        format!("refs:{namespace}")
    }

    pub(crate) fn validate_reference_name(store: &str, name: &str) -> Result<()> {
        if name.is_empty() || name.contains('.') {
            return Err(StoreError::InvalidOperation {
                store: store.to_string(),
                operation: "reference".to_string(),
                reason: "reference names must be nonempty and contain no dots".to_string(),
            }
            .into());
        }
        Ok(())
    }

    /// Find the reference namespaces registered on this Account, without opening their targets.
    pub async fn reference_namespaces(&self) -> Result<Vec<String>> {
        Self::reference_namespaces_at(&self.reference_read_transaction().await?).await
    }

    pub(crate) async fn reference_namespaces_at(tx: &Transaction) -> Result<Vec<String>> {
        Ok(tx
            .get_index()
            .await?
            .list()
            .await?
            .into_iter()
            .filter_map(|name| name.strip_prefix("refs:").map(str::to_owned))
            .collect())
    }

    /// List named references in this namespace without opening any target.
    /// A row without an ID has a malformed reference value.
    pub async fn references(&self, namespace: &str) -> Result<Vec<(String, Option<ID>)>> {
        Self::references_at(&self.reference_read_transaction().await?, namespace).await
    }

    pub(crate) async fn references_at(
        tx: &Transaction,
        namespace: &str,
    ) -> Result<Vec<(String, Option<ID>)>> {
        let store = Self::reference_store(namespace);
        let references = DocStore::load(tx, store.clone()).await?.get_all().await?;
        Ok(references
            .iter()
            .map(|(name, value)| {
                let target = Self::validate_reference_name(&store, name)
                    .ok()
                    .and_then(|()| value.as_text())
                    .and_then(|value| serde_json::from_str(value).ok());
                (name.clone(), target)
            })
            .collect())
    }

    /// Read one named reference after checking current Account Auth.
    /// A reference grants no access to the target. Malformed data is an error, not absence.
    pub async fn reference(&self, namespace: &str, name: &str) -> Result<Option<ID>> {
        Self::reference_at(&self.reference_read_transaction().await?, namespace, name).await
    }

    pub(crate) async fn reference_at(
        tx: &Transaction,
        namespace: &str,
        name: &str,
    ) -> Result<Option<ID>> {
        let store = Self::reference_store(namespace);
        Self::validate_reference_name(&store, name)?;
        // Typed Table projection omits non-text values. Read raw data so
        // corruption cannot look like an absent or stale projected reference.
        let references = DocStore::load(tx, store.clone()).await?;
        match references.get_string(name).await {
            Ok(value) => serde_json::from_str(&value).map(Some).map_err(|e| {
                StoreError::DeserializationFailed {
                    store: store.clone(),
                    reason: format!("Invalid database reference '{name}': {e}"),
                }
                .into()
            }),
            Err(Error::Store(e))
                if matches!(e.as_ref(), StoreError::KeyNotFound { store: s, key }
                    if *s == store && key == name) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn reference_read_transaction(&self) -> Result<Transaction> {
        let tx = self.database.new_transaction().await?;
        // The service already checks every read against the handle's exact
        // identity. Delegation proofs are resolved on the server, not by a
        // client-side current_permission() call through the remote backend.
        #[cfg(all(unix, feature = "service"))]
        if self.database.instance()?.remote_connection().is_some() {
            return Ok(tx);
        }
        self.database.current_permission().await?;
        Ok(tx)
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
