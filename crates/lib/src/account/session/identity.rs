//! Identity references are portable; signer selection and request state are local.

use super::*;
use crate::{
    account::{Identity, IdentityRegistration, IdentityStatus, TrackedIdentity},
    auth::AuthKey,
};

const IDENTITY_REFERENCES: &str = "identities";

impl AccountSession {
    /// Read local activation/request state without opening or activating anything.
    /// `None` means untracked on this installation, not an absent Account reference.
    pub async fn tracked_identity(&self, name: &str) -> Result<Option<TrackedIdentity>> {
        Ok(self.read_local_state().await?.identities.get(name).cloned())
    }

    /// Create an ordinary globally readable Identity with the chosen initial Admin.
    /// Stores its named Account reference separately from installation-local choices.
    /// Serialize first-time initialization: reference writes are ordinary CRDT writes.
    /// A later reference/tracking failure does not delete an already-created root.
    pub async fn create_identity(&mut self, name: &str, key_id: &PublicKey) -> Result<Identity> {
        Self::validate_reference_name(IDENTITY_REFERENCES, name)?;
        if self.identity_id(name).await?.is_some() || self.tracked_identity(name).await?.is_some() {
            return Err(AccountError::IdentityAlreadyExists {
                name: name.to_owned(),
            }
            .into());
        }
        let signer = self.acting_as(key_id, None)?;
        let database = self
            .create_database_with_init(Doc::new(), key_id, async |tx| {
                tx.get_settings()?
                    .set_global_auth_key(AuthKey::active(None, Permission::Read))
                    .await
            })
            .await?;
        let setup = async {
            self.set_reference(IDENTITY_REFERENCES, name, database.root_id())
                .await?;
            self.replace_tracked_identity(
                name,
                None,
                Some(TrackedIdentity {
                    root_id: database.root_id().clone(),
                    status: IdentityStatus::Active,
                    key_id: key_id.clone(),
                    requested_key_id: key_id.clone(),
                    requested_permission: Permission::Admin(0),
                    sync_settings: SyncSettings::disabled(),
                }),
            )
            .await
        }
        .await;
        setup.map_err(|error| error.after_database_creation(database.root_id()))?;
        Ok(Identity::new(
            database,
            key_id.clone(),
            signer,
            name.to_owned(),
        ))
    }

    /// Request membership using the shared ticket/bootstrap machinery.
    /// The same name/root/request key/permission resumes; changed request parameters
    /// conflict. Settings are for enrollment, not for updating an active database. Connected clients sign
    /// proofs locally and use the daemon's Sync, never a client-side local engine.
    /// For an Active Identity, pass the original request key and permission even
    /// after rotation. Only the currently selected key needs to be owned and an
    /// active direct member. A retry repairs tracking without using ticket addresses
    /// or replacing the database's current preferences with the enrollment settings.
    ///
    /// A new name is not reserved by a ticket with no peer address hints.
    /// Initial network errors remove only newly provisional local tracking; an
    /// accepted/pending exchange retains state through mapping/activation failures.
    /// The Account reference is organizational and may remain after failure.
    pub async fn register_identity(
        &mut self,
        name: &str,
        ticket: &DatabaseTicket,
        key_id: &PublicKey,
        requested_permission: Permission,
        sync_settings: SyncSettings,
    ) -> Result<IdentityRegistration> {
        Self::validate_reference_name(IDENTITY_REFERENCES, name)?;
        let previous = self.tracked_identity(name).await?;
        let active = previous
            .as_ref()
            .filter(|tracked| tracked.status == IdentityStatus::Active);
        if active.is_none() {
            self.get_signing_key(key_id)?;
        }
        let reference = self.identity_id(name).await?;
        if previous.as_ref().is_some_and(|tracked| {
            tracked.root_id != *ticket.database_id()
                || tracked.requested_key_id != *key_id
                || tracked.requested_permission != requested_permission
        }) || reference
            .as_ref()
            .is_some_and(|root| root != ticket.database_id())
        {
            return Err(AccountError::IdentityRegistrationConflict {
                name: name.to_owned(),
            }
            .into());
        }
        // Keep the embedded exchange off the caller's stack while polling
        // verified history and routing.
        let bootstrap =
            || Box::pin(self.bootstrap_access(ticket, key_id, requested_permission, None, true));
        let preflight = previous.is_none() && reference.is_none() && ticket.addresses().is_empty();
        if preflight {
            // Both bootstrap paths require a route, even for existing members.
            // Let the normal path report its error before reserving the name.
            bootstrap().await?;
        }
        // The reference is not a membership grant or a key selection on another
        // installation. Do not overwrite a present, different reference.
        let original_database = self.tracked_database(ticket.database_id()).await?;
        if reference.is_none() {
            self.set_reference(IDENTITY_REFERENCES, name, ticket.database_id())
                .await?;
        }
        if let Some(previous) = active {
            return self
                .refresh_active_identity(name, previous, original_database)
                .await
                .map(IdentityRegistration::Active);
        }
        let tracked = TrackedIdentity {
            sync_settings: sync_settings.clone(),
            ..previous.clone().unwrap_or(TrackedIdentity {
                root_id: ticket.database_id().clone(),
                status: IdentityStatus::Pending,
                key_id: key_id.clone(),
                requested_key_id: key_id.clone(),
                requested_permission,
                sync_settings: sync_settings.clone(),
            })
        };
        self.replace_tracked_identity(name, previous.clone(), Some(tracked.clone()))
            .await?;
        let result = if preflight { Ok(()) } else { bootstrap().await };
        let pending = matches!(&result, Err(Error::Sync(error)) if matches!(error.as_ref(), SyncError::BootstrapPending { .. }));
        if !pending && let Err(error) = result {
            if previous.is_none()
                && let Err(cleanup) = self
                    .replace_tracked_identity(name, Some(tracked), None)
                    .await
            {
                return Err(AccountError::IdentityRegistrationCleanupFailed {
                    name: name.to_owned(),
                    registration: Box::new(error),
                    cleanup: Box::new(cleanup),
                }
                .into());
            }
            return Err(error);
        }
        if pending {
            return Ok(IdentityRegistration::Pending);
        }
        self.record_database_access_if_unchanged(
            ticket.database_id(),
            key_id,
            sync_settings,
            result,
            original_database,
        )
        .await?;
        self.activate_tracked_identity(name, tracked)
            .await
            .map(IdentityRegistration::Active)
    }

    async fn refresh_active_identity(
        &mut self,
        name: &str,
        previous: &TrackedIdentity,
        original_database: Option<TrackedDatabase>,
    ) -> Result<Identity> {
        let identity = self.open_tracked_identity(name, previous).await?;
        let root = &previous.root_id;
        let key_id = &previous.key_id;
        let database_key = root.to_string();
        let result = self
            .update_local_state(|state| {
                // Do not track a signer superseded while opening it.
                if state.identities.get(name) != Some(previous) {
                    return Err(AccountError::IdentityRegistrationConflict {
                        name: name.to_owned(),
                    }
                    .into());
                }
                Self::map_key_in_state(state, key_id, root, SigKey::from_pubkey(key_id))?;
                if state.databases.get(&database_key) == original_database.as_ref() {
                    state.databases.insert(
                        database_key.clone(),
                        TrackedDatabase {
                            database_id: root.clone(),
                            key_id: key_id.clone(),
                            sync_settings: original_database.as_ref().map_or_else(
                                || previous.sync_settings.clone(),
                                |db| db.sync_settings.clone(),
                            ),
                        },
                    );
                }
                Ok(())
            })
            .await;
        // As with generic tracking, re-read the mapping even if its write was not acknowledged.
        self.refresh_key_metadata(key_id).await?;
        result?;
        self.refresh_local_sync().await?;
        Ok(identity)
    }

    /// Select an owned, explicitly active member key and return a newly bound handle.
    /// Existing handles retain their actual signer. Import does not imply selection.
    pub async fn select_identity_key(&self, name: &str, key_id: &PublicKey) -> Result<Identity> {
        let previous = self.required_tracked_identity(name).await?;
        if previous.status == IdentityStatus::Pending {
            return Err(AccountError::DatabaseAccessPending {
                database_id: previous.root_id,
            }
            .into());
        }
        let tracked = TrackedIdentity {
            key_id: key_id.clone(),
            ..previous.clone()
        };
        let identity = self.open_tracked_identity(name, &tracked).await?;
        self.replace_tracked_identity(name, Some(previous), Some(tracked))
            .await?;
        Ok(identity)
    }

    /// Pure lookup; pending or locally untracked references return `None`.
    pub async fn get_identity(&self, name: &str) -> Result<Option<Identity>> {
        let Some(tracked) = self.tracked_identity(name).await? else {
            return Ok(None);
        };
        if tracked.status == IdentityStatus::Pending {
            return Ok(None);
        }
        self.open_tracked_identity(name, &tracked).await.map(Some)
    }

    /// Activate only after locally verified explicit membership is available.
    /// Public readability or an Account reference is never sufficient.
    pub async fn activate_identity(&self, name: &str) -> Result<Identity> {
        let previous = self.required_tracked_identity(name).await?;
        self.activate_tracked_identity(name, previous).await
    }

    async fn activate_tracked_identity(
        &self,
        name: &str,
        previous: TrackedIdentity,
    ) -> Result<Identity> {
        let identity = self.open_tracked_identity(name, &previous).await?;
        if previous.status == IdentityStatus::Pending {
            self.replace_tracked_identity(
                name,
                Some(previous.clone()),
                Some(TrackedIdentity {
                    status: IdentityStatus::Active,
                    ..previous
                }),
            )
            .await?;
        }
        Ok(identity)
    }

    async fn open_tracked_identity(
        &self,
        name: &str,
        tracked: &TrackedIdentity,
    ) -> Result<Identity> {
        if self.identity_id(name).await?.as_ref() != Some(&tracked.root_id) {
            return Err(AccountError::IdentityRegistrationConflict {
                name: name.to_owned(),
            }
            .into());
        }
        let signer = self.acting_as(&tracked.key_id, None)?;
        let database = signer
            .open_database(&self.instance, &tracked.root_id)
            .await?;
        let auth = database.live_auth_settings().await?;
        let Ok(member) = auth.get_key_by_pubkey(&tracked.key_id) else {
            return Err(AccountError::NoSigKeyFound {
                key_id: tracked.key_id.to_string(),
                database_id: tracked.root_id.clone(),
            }
            .into());
        };
        if !member.is_active() {
            return Err(AccountError::NoSigKeyFound {
                key_id: tracked.key_id.to_string(),
                database_id: tracked.root_id.clone(),
            }
            .into());
        }
        if tracked.status == IdentityStatus::Pending
            && member.permissions() < &tracked.requested_permission
        {
            return Err(SyncError::PermissionDenied(
                "the selected member does not yet have the requested permission".to_owned(),
            )
            .into());
        }
        Ok(Identity::new(
            self.with_local_account(database),
            tracked.key_id.clone(),
            signer,
            name.to_owned(),
        ))
    }

    /// Portable Account reference, independent of local activation or credentials.
    pub async fn identity_id(&self, name: &str) -> Result<Option<ID>> {
        self.reference(IDENTITY_REFERENCES, name).await
    }

    /// List only this installation's typed activation/request records.
    pub async fn identities(&self) -> Result<Vec<(String, TrackedIdentity)>> {
        Ok(self
            .read_local_state()
            .await?
            .identities
            .into_iter()
            .collect())
    }

    /// The locally selected key, not an implicit portable default.
    pub async fn identity_key(&self, name: &str) -> Result<PublicKey> {
        Ok(self.required_tracked_identity(name).await?.key_id)
    }

    /// Forget local activation/request state only. Retains the portable reference,
    /// membership, database sync preferences, the database and existing handles.
    pub async fn forget_identity(&mut self, name: &str) -> Result<()> {
        let previous = self.required_tracked_identity(name).await?;
        self.replace_tracked_identity(name, Some(previous), None)
            .await
    }

    async fn required_tracked_identity(&self, name: &str) -> Result<TrackedIdentity> {
        self.tracked_identity(name).await?.ok_or_else(|| {
            AccountError::IdentityNotFound {
                name: name.to_owned(),
            }
            .into()
        })
    }

    async fn replace_tracked_identity(
        &self,
        name: &str,
        expected: Option<TrackedIdentity>,
        replacement: Option<TrackedIdentity>,
    ) -> Result<()> {
        self.update_local_state(|state| {
            if state.identities.get(name) != expected.as_ref() {
                return Err(AccountError::IdentityRegistrationConflict {
                    name: name.to_owned(),
                }
                .into());
            }
            match &replacement {
                Some(tracked) => {
                    state.identities.insert(name.to_owned(), tracked.clone());
                }
                None => {
                    state.identities.remove(name);
                }
            }
            Ok(())
        })
        .await?;
        Ok(())
    }
}
