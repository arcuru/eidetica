//! Blobs an Account's Database registration wants this Instance to keep.
//!
//! The wanted set belongs to the existing registration: an Account root's
//! installation-local login record and a Database it tracks. It is recorded in
//! the backend's instance-local retention records, keyed by that owner and
//! Database, so it survives app exit, service reconnect and restart without
//! replicating to other Instances. A blob is kept while any registration wants
//! it. Fetching missing bytes asks only the registered Database's peers, and
//! only while that registration is syncing.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Mutex,
};

use super::Instance;
use crate::{
    Result,
    account::AccountError,
    blob::{BlobFetchAttempt, MAX_WANTED_BLOBS, WantedBlobState, WantedBlobStatus},
    entry::ID,
    sync::peer_types::PeerInfo,
};

/// Fetch bookkeeping for requested blobs. Not durable: it describes what this
/// running Instance observed, never what it owns.
#[derive(Default)]
pub(crate) struct BlobFetchState {
    outcomes: HashMap<ID, BlobFetchAttempt>,
    in_flight: HashSet<ID>,
}

/// Covers a whole fetch, including publication. Dropping a cancelled caller
/// must not leave the CID permanently ineligible for reconciliation.
struct BlobFetchGuard<'a> {
    state: &'a Mutex<BlobFetchState>,
    cid: &'a ID,
}

impl<'a> BlobFetchGuard<'a> {
    fn claim(state: &'a Mutex<BlobFetchState>, cid: &'a ID) -> Option<Self> {
        if !state.lock().unwrap().in_flight.insert(cid.clone()) {
            return None;
        }
        Some(Self { state, cid })
    }
}

impl Drop for BlobFetchGuard<'_> {
    fn drop(&mut self) {
        self.state.lock().unwrap().in_flight.remove(self.cid);
    }
}

/// Concurrent wanted fetches across all rounds, including local publication.
pub(super) const MAX_CONCURRENT_BLOB_FETCHES: usize = 4;

/// The local owner of a registration: an Account root and the
/// installation-local login record that holds its Database registrations.
///
/// This is the existing registration's identity, not a separate principal.
/// The login name is never part of it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BlobOwner {
    account_id: ID,
    login_id: String,
}

impl BlobOwner {
    pub(crate) fn new(login_id: &str, account_id: &ID) -> Result<Self> {
        // Explicit signer connections have no login record and own nothing.
        if login_id.is_empty() || login_id.starts_with("signer:") {
            return Err(AccountError::InvalidLocalState.into());
        }
        Ok(Self {
            account_id: account_id.clone(),
            login_id: login_id.to_owned(),
        })
    }

    /// The owner key in the engine's retention records. An Account root's
    /// text form never contains `/`, so the first one separates the parts.
    fn key(&self) -> String {
        format!("{}/{}", self.account_id, self.login_id)
    }

    /// Parse an owner key. Records that do not name a registration owner are
    /// not this module's to manage and are left untouched.
    fn parse(key: &str) -> Option<Self> {
        let (account, login) = key.split_once('/')?;
        let account_id = ID::parse(account).ok()?;
        Self::new(login, &account_id).ok()
    }
}

/// A registration: its owner and the Database it tracks.
type Registration = (BlobOwner, ID);

/// Validate and deduplicate a named blob set.
pub(crate) fn normalize_wanted(cids: impl IntoIterator<Item = ID>) -> Result<BTreeSet<ID>> {
    let mut set = BTreeSet::new();
    for cid in cids {
        crate::backend::validate_blob_cid(&cid)?;
        set.insert(cid);
        check_wanted_count(set.len())?;
    }
    Ok(set)
}

fn check_wanted_count(count: usize) -> Result<()> {
    if count > MAX_WANTED_BLOBS {
        return Err(AccountError::TooManyWantedBlobs {
            count,
            max: MAX_WANTED_BLOBS,
        }
        .into());
    }
    Ok(())
}

pub(crate) fn validate_status_cids(cids: &[ID]) -> Result<()> {
    check_wanted_count(cids.len())?;
    for cid in cids {
        crate::backend::validate_blob_cid(cid)?;
    }
    Ok(())
}

impl Instance {
    pub(crate) fn blob_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.inner.blob_lock
    }

    /// Read whether a registration still exists and whether it is syncing.
    ///
    /// `None` means the login record no longer belongs to the Account root or
    /// no longer tracks the Database.
    async fn registration_sync_enabled(
        &self,
        owner: &BlobOwner,
        database: &ID,
    ) -> Result<Option<bool>> {
        let linked = crate::account::local_logins::records(self)
            .await?
            .into_iter()
            .any(|(login_id, info)| {
                login_id == owner.login_id && info.account_id == owner.account_id
            });
        if !linked {
            return Ok(None);
        }
        let state = crate::account::local_state::read(self, &owner.login_id).await?;
        Ok(state
            .databases
            .get(&database.to_string())
            .map(|tracked| tracked.sync_settings.sync_enabled))
    }

    /// Replace a registration's wanted set for a caller holding `blob_lock`.
    ///
    /// Additions are recorded before removals, under the lock GC holds for its
    /// whole sweep, so a CID present in both the old and new set is never
    /// collectible during the update. Only this registration's records change.
    async fn replace_wanted_blobs_locked(
        &self,
        owner: &BlobOwner,
        database: &ID,
        wanted: &BTreeSet<ID>,
    ) -> Result<()> {
        let owner_key = owner.key();
        let database_key = database.to_string();
        let engine = self.local_blob_engine()?;
        let current = self.registration_wanted(owner, database).await?;
        for cid in wanted.difference(&current) {
            engine.pin_blob(&owner_key, &database_key, cid).await?;
        }
        for cid in current.difference(wanted) {
            engine.unpin_blob(&owner_key, &database_key, cid).await?;
        }
        Ok(())
    }

    /// Replace a registration's wanted set after checking that it exists.
    ///
    /// The registration is read under `blob_lock`, so this serializes with an
    /// untrack's release: either the release runs after these records and
    /// clears them, or this call sees the registration gone and records
    /// nothing. An empty set releases the registration's ownership.
    pub(crate) async fn set_wanted_blobs_local(
        &self,
        owner: &BlobOwner,
        database: &ID,
        wanted: BTreeSet<ID>,
    ) -> Result<()> {
        {
            let _guard = self.inner.blob_lock.lock().await;
            if self
                .registration_sync_enabled(owner, database)
                .await?
                .is_none()
            {
                return Err(AccountError::DatabaseNotTracked {
                    database_id: database.clone(),
                }
                .into());
            }
            self.replace_wanted_blobs_locked(owner, database, &wanted)
                .await?;
        }
        if !wanted.is_empty() {
            self.spawn_registration_fetch(owner, database);
        }
        Ok(())
    }

    /// Release a registration's wanted set if it no longer exists.
    ///
    /// Re-reads the registration under `blob_lock`, so a registration that was
    /// tracked again and given a new set after an earlier read is left intact.
    pub(crate) async fn release_if_unregistered(
        &self,
        owner: &BlobOwner,
        database: &ID,
    ) -> Result<()> {
        let _guard = self.inner.blob_lock.lock().await;
        self.release_if_unregistered_locked(owner, database).await
    }

    async fn release_if_unregistered_locked(&self, owner: &BlobOwner, database: &ID) -> Result<()> {
        if self
            .registration_sync_enabled(owner, database)
            .await?
            .is_none()
        {
            self.replace_wanted_blobs_locked(owner, database, &BTreeSet::new())
                .await?;
        }
        Ok(())
    }

    /// Called before the local CAS creates a registration, with the blob lock
    /// held and the absence of those registrations confirmed. Abandoned pins
    /// from an interrupted untrack cannot carry over to the new registration.
    pub(crate) async fn clear_retracked_blob_pins_locked(
        &self,
        login_id: &str,
        databases: &BTreeSet<&str>,
    ) -> Result<()> {
        let engine = self.local_blob_engine()?;
        for (key, db, cid) in engine.all_blob_pins().await? {
            if databases.contains(db.as_str())
                && BlobOwner::parse(&key).is_some_and(|owner| owner.login_id == login_id)
            {
                engine.unpin_blob(&key, &db, &cid).await?;
            }
        }
        Ok(())
    }

    /// Explicit GC also works with synchronization disabled.
    pub(super) async fn prune_unregistered_blob_pins_locked(&self) -> Result<()> {
        for (owner, db) in self.wanted_registrations().await?.into_keys() {
            self.release_if_unregistered_locked(&owner, &db).await?;
        }
        Ok(())
    }

    /// Start a fetch round for a registration without waiting for it.
    ///
    /// The durable wanted set is already recorded; this only shortens the wait
    /// before the periodic reconciliation would fetch it.
    pub(crate) fn spawn_registration_fetch(&self, owner: &BlobOwner, database: &ID) {
        if self.sync().is_none() {
            return;
        }
        let instance = self.clone();
        let owner = owner.clone();
        let database = database.clone();
        tokio::spawn(async move {
            if let Err(e) = instance.fetch_registration_blobs(&owner, &database).await {
                tracing::debug!(?owner, %database, error = %e, "Wanted blob fetch round failed");
            }
        });
    }

    /// Start fetch rounds for every wanted set of one login without waiting.
    ///
    /// Called after that login's registrations change, so resuming sharing
    /// resumes fetching without waiting for periodic reconciliation.
    pub(crate) fn spawn_login_blob_fetch(&self, login_id: &str, account_id: &ID) {
        if self.sync().is_none() || self.inner.backend.local_engine().is_none() {
            return;
        }
        let Ok(owner) = BlobOwner::new(login_id, account_id) else {
            return;
        };
        let instance = self.clone();
        tokio::spawn(async move {
            let result = async {
                let databases: Vec<ID> = instance
                    .wanted_registrations()
                    .await?
                    .into_keys()
                    .filter(|(candidate, _)| *candidate == owner)
                    .map(|(_, database)| database)
                    .collect();
                for database in databases {
                    instance.fetch_registration_blobs(&owner, &database).await?;
                }
                Ok::<_, crate::Error>(())
            }
            .await;
            if let Err(e) = result {
                tracing::debug!(error = %e, "Wanted blob fetch after registration change failed");
            }
        });
    }

    /// The registration's current wanted set, sorted.
    pub(crate) async fn wanted_blobs_local(
        &self,
        owner: &BlobOwner,
        database: &ID,
    ) -> Result<Vec<ID>> {
        let _guard = self.inner.blob_lock.lock().await;
        if self
            .registration_sync_enabled(owner, database)
            .await?
            .is_none()
        {
            return Ok(Vec::new());
        }
        let set = self.registration_wanted(owner, database).await?;
        Ok(set.into_iter().collect())
    }

    /// One registration's recorded wanted set.
    async fn registration_wanted(&self, owner: &BlobOwner, database: &ID) -> Result<BTreeSet<ID>> {
        let owner_key = owner.key();
        let database_key = database.to_string();
        Ok(self
            .local_blob_engine()?
            .all_blob_pins()
            .await?
            .into_iter()
            .filter(|(user, db, _)| *user == owner_key && *db == database_key)
            .map(|(_, _, cid)| cid)
            .collect())
    }

    /// Local availability of explicitly named blobs, in the given order.
    pub(crate) async fn blob_status_local(&self, cids: &[ID]) -> Result<Vec<WantedBlobStatus>> {
        validate_status_cids(cids)?;
        let mut statuses = Vec::with_capacity(cids.len());
        for cid in cids {
            // A zero-byte read checks that disk-backed records have a backing
            // file without loading the blob or updating access times.
            let present = {
                let _guard = self.inner.blob_lock.lock().await;
                self.local_blob_engine()?
                    .get_blob_range(cid, 0..0)
                    .await?
                    .is_some()
            };
            let state = if present {
                WantedBlobState::Available
            } else {
                let last_fetch = self
                    .inner
                    .blob_fetch
                    .lock()
                    .unwrap()
                    .outcomes
                    .get(cid)
                    .copied();
                WantedBlobState::Pending { last_fetch }
            };
            statuses.push(WantedBlobStatus {
                cid: cid.clone(),
                state,
            });
        }
        Ok(statuses)
    }

    /// Every registration with recorded wanted blobs, with its wanted set.
    async fn wanted_registrations(&self) -> Result<BTreeMap<Registration, BTreeSet<ID>>> {
        let engine = self.local_blob_engine()?;
        let mut registrations: BTreeMap<Registration, BTreeSet<ID>> = BTreeMap::new();
        for (owner, database, cid) in engine.all_blob_pins().await? {
            // Engine records that do not name a registration are not wanted
            // sets; leave them untouched rather than guess an owner.
            let (Some(owner), Ok(database)) = (BlobOwner::parse(&owner), ID::parse(&database))
            else {
                continue;
            };
            registrations
                .entry((owner, database))
                .or_default()
                .insert(cid);
        }
        Ok(registrations)
    }

    /// Reconcile every registration's wanted blobs.
    ///
    /// Releases records whose registration no longer exists (a removal that
    /// was interrupted before its release ran), then fetches missing blobs for
    /// registrations that are syncing. Paused registrations keep ownership and
    /// contribute no fetches.
    pub(crate) async fn reconcile_wanted_blobs(&self) -> Result<()> {
        let mut active = Vec::new();
        let mut wanted = HashSet::new();
        for ((owner, database), cids) in self.wanted_registrations().await? {
            match self.registration_sync_enabled(&owner, &database).await {
                Ok(None) => {
                    self.release_if_unregistered(&owner, &database).await?;
                    continue;
                }
                Ok(Some(true)) => active.push((owner, database)),
                Ok(Some(false)) => {}
                // An unreadable registration is not evidence it was removed.
                Err(e) => {
                    tracing::debug!(?owner, %database, error = %e, "Skipping unreadable blob registration")
                }
            }
            wanted.extend(cids);
        }
        // Forget outcomes for blobs no registration wants any more.
        self.inner
            .blob_fetch
            .lock()
            .unwrap()
            .outcomes
            .retain(|cid, _| wanted.contains(cid));
        self.fetch_wanted_blobs(&active).await
    }

    /// Fetch one registration's missing blobs if it is syncing.
    pub(crate) async fn fetch_registration_blobs(
        &self,
        owner: &BlobOwner,
        database: &ID,
    ) -> Result<()> {
        if self.registration_sync_enabled(owner, database).await? == Some(true) {
            self.fetch_wanted_blobs(&[(owner.clone(), database.clone())])
                .await?;
        }
        Ok(())
    }

    /// Fetch missing wanted blobs for the given active registrations.
    ///
    /// Each missing CID is fetched once, from the deduplicated union of the
    /// peers of every listed registration's Database that wants it. A CID
    /// already being fetched is skipped. Fetched bytes are published through
    /// the verified persist path; a fetch never records ownership, so a
    /// completion that lands after its registration released the CID cannot
    /// restore it.
    async fn fetch_wanted_blobs(&self, registrations: &[Registration]) -> Result<()> {
        let Some(sync) = self.sync() else {
            return Ok(());
        };
        let all = self.wanted_registrations().await?;
        let mut registrations_by_cid: BTreeMap<ID, Vec<&Registration>> = BTreeMap::new();
        let mut providers_by_database: HashMap<ID, Vec<PeerInfo>> = HashMap::new();
        for registration in registrations {
            let Some(wanted) = all.get(registration) else {
                continue;
            };
            let database = &registration.1;
            if !providers_by_database.contains_key(database) {
                let providers = sync.blob_providers(database).await?;
                providers_by_database.insert(database.clone(), providers);
            }
            for cid in wanted {
                if self.has_blob_local(cid).await? {
                    continue;
                }
                registrations_by_cid
                    .entry(cid.clone())
                    .or_default()
                    .push(registration);
            }
        }

        use futures_util::stream::StreamExt;
        futures_util::stream::iter(registrations_by_cid)
            .for_each_concurrent(MAX_CONCURRENT_BLOB_FETCHES, |(cid, registrations)| {
                let sync = &sync;
                let providers_by_database = &providers_by_database;
                async move {
                    let Some(_claim) = BlobFetchGuard::claim(&self.inner.blob_fetch, &cid) else {
                        return;
                    };
                    let Ok(_slot) = self.inner.blob_fetch_slots.acquire().await else {
                        return;
                    };
                    if !matches!(self.has_blob_local(&cid).await, Ok(false)) {
                        return;
                    }
                    // The earlier snapshot can outlive many network attempts.
                    // Do not start another CID using a registration paused or
                    // removed while this round was waiting for a slot.
                    let mut active = false;
                    let mut providers: Vec<PeerInfo> = Vec::new();
                    for (owner, database) in registrations {
                        match self.registration_sync_enabled(owner, database).await {
                            Ok(Some(true)) => active = true,
                            Ok(_) => continue,
                            Err(e) => {
                                tracing::debug!(%database, error = %e, "Skipping unreadable blob registration");
                                continue;
                            }
                        }
                        for provider in &providers_by_database[database] {
                            if !providers.iter().any(|p| p.id == provider.id) {
                                providers.push(provider.clone());
                            }
                        }
                    }
                    if !active {
                        return;
                    }
                    let (bytes, mut attempt) = sync
                        .fetch_blob_from_providers(&providers, &cid, 0..u64::MAX)
                        .await;
                    let mut published = false;
                    if let Some(bytes) = bytes {
                        // The persist path re-verifies that the bytes hash to
                        // the CID, so only a whole verified blob is published.
                        match self.persist_blob(&cid, bytes).await {
                            Ok(()) => published = true,
                            Err(e) => {
                                tracing::warn!(%cid, error = %e, "Fetched blob failed to publish");
                                attempt.failed += 1;
                            }
                        }
                    }
                    let mut state = self.inner.blob_fetch.lock().unwrap();
                    if published {
                        state.outcomes.remove(&cid);
                    } else {
                        state.outcomes.insert(cid.clone(), attempt);
                    }
                }
            })
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod blob_ownership_tests {
    use super::*;
    use crate::{
        Error, NewAccount, account::AccountSession, backend::database::InMemory, crdt::Doc,
        instance::GcOptions,
    };

    async fn instance_with_account() -> Result<(Instance, AccountSession)> {
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("owner")).await
    }

    fn owner_of(account: &AccountSession) -> BlobOwner {
        BlobOwner::new(account.login_id(), account.account().id()).unwrap()
    }

    #[tokio::test]
    async fn blob_status_bounds_duplicate_rows() -> Result<()> {
        let (_, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let database = account.create_database(Doc::new(), &key).await?;
        let cid = ID::from_bytes(b"duplicated status row");
        assert!(
            matches!(database.blob_status(&vec![cid; MAX_WANTED_BLOBS + 1]).await,
                Err(Error::Account(e)) if matches!(*e, AccountError::TooManyWantedBlobs { .. })
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn blob_set_stops_consuming_after_capacity() -> Result<()> {
        let (_, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let database = account.create_database(Doc::new(), &key).await?;
        let over_limit = (0..=MAX_WANTED_BLOBS + 1).map(|i| {
            assert!(
                i <= MAX_WANTED_BLOBS,
                "consumed input after rejecting the set"
            );
            ID::from_bytes(i.to_le_bytes())
        });
        assert!(matches!(database.set_wanted_blobs(over_limit).await,
            Err(Error::Account(e)) if matches!(*e, AccountError::TooManyWantedBlobs { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn blob_interrupted_untrack_does_not_keep_pins_without_sync() -> Result<()> {
        let (instance, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let database = account.create_database(Doc::new(), &key).await?;
        let cid = instance.put_blob(vec![1]).await?;
        database.set_wanted_blobs([cid]).await?;
        remove_row_only(&instance, &account, database.root_id()).await;
        assert!(account.wanted_blobs(database.root_id()).await?.is_empty());
        let report = instance
            .gc_blobs(GcOptions {
                min_age_ms: 0,
                ..Default::default()
            })
            .await?;
        assert_eq!(report.evicted_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn blob_retrack_does_not_inherit_interrupted_untracks_set() -> Result<()> {
        let (instance, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let database = account.create_database(Doc::new(), &key).await?;
        database
            .set_wanted_blobs([ID::from_bytes(b"old set")])
            .await?;
        remove_row_only(&instance, &account, database.root_id()).await;
        account
            .track_database(
                database.root_id(),
                &key,
                crate::account::SyncSettings::disabled(),
            )
            .await?;
        assert!(database.wanted_blobs().await?.is_empty());
        Ok(())
    }

    /// Remove a registration row without the untrack's blob release, as an
    /// untrack interrupted between its two steps would.
    async fn remove_row_only(instance: &Instance, account: &AccountSession, database: &ID) {
        crate::account::local_state::update(instance, account.login_id(), |state| {
            state.databases.remove(&database.to_string());
            Ok(())
        })
        .await
        .unwrap();
    }

    /// An interrupted untrack leaves records with no registration;
    /// reconciliation releases exactly those and keeps every live
    /// registration's ownership and unrelated engine records.
    #[tokio::test]
    async fn blob_reconcile_releases_only_removed_registrations() -> Result<()> {
        let (instance, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let kept_db = account.create_database(Doc::new(), &key).await?;
        let removed_db = account.create_database(Doc::new(), &key).await?;
        let kept = instance.put_blob(b"still wanted".to_vec()).await?;
        let orphaned = instance.put_blob(b"left behind".to_vec()).await?;
        let foreign = instance.put_blob(b"engine record".to_vec()).await?;
        kept_db.set_wanted_blobs([kept.clone()]).await?;
        removed_db.set_wanted_blobs([orphaned.clone()]).await?;
        instance
            .local_blob_engine()?
            .pin_blob("not-a-registration", "", &foreign)
            .await?;
        remove_row_only(&instance, &account, removed_db.root_id()).await;
        let owner = owner_of(&account);
        assert_eq!(
            instance
                .registration_wanted(&owner, removed_db.root_id())
                .await?,
            BTreeSet::from([orphaned.clone()]),
            "the interrupted untrack left its records"
        );

        instance.reconcile_wanted_blobs().await?;

        assert!(
            instance
                .wanted_blobs_local(&owner, removed_db.root_id())
                .await?
                .is_empty()
        );
        assert_eq!(kept_db.wanted_blobs().await?, vec![kept.clone()]);
        let report = instance
            .gc_blobs(GcOptions {
                min_age_ms: 0,
                ..Default::default()
            })
            .await?;
        assert_eq!(report.evicted_count, 1);
        assert!(instance.get_blob_local(&orphaned).await?.is_none());
        assert!(instance.get_blob_local(&kept).await?.is_some());
        assert!(instance.get_blob_local(&foreign).await?.is_some());
        Ok(())
    }

    /// A release decided from an earlier "registration gone" observation
    /// re-checks under the blob lock: a registration tracked again and given a
    /// new set in between keeps it. Setting a set for an untracked
    /// registration records nothing.
    #[tokio::test]
    async fn blob_stale_release_keeps_a_retracked_registration() -> Result<()> {
        let (instance, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let database = account.create_database(Doc::new(), &key).await?;
        let root = database.root_id().clone();
        let owner = owner_of(&account);
        let blob = instance.put_blob(b"wanted again".to_vec()).await?;

        // Untrack's first step runs, then the Database is tracked again and
        // given a new set before the untrack's release step.
        remove_row_only(&instance, &account, &root).await;
        assert!(
            database.set_wanted_blobs([blob.clone()]).await.is_err(),
            "an untracked registration records nothing"
        );
        assert!(instance.wanted_blobs_local(&owner, &root).await?.is_empty());
        account
            .track_database(
                root.clone(),
                &key,
                crate::account::types::SyncSettings::disabled(),
            )
            .await?;
        database.set_wanted_blobs([blob.clone()]).await?;

        instance.release_if_unregistered(&owner, &root).await?;
        assert_eq!(database.wanted_blobs().await?, vec![blob.clone()]);
        instance.reconcile_wanted_blobs().await?;
        assert_eq!(database.wanted_blobs().await?, vec![blob.clone()]);

        // A full untrack releases it.
        account.untrack_database(&root).await?;
        assert!(instance.wanted_blobs_local(&owner, &root).await?.is_empty());
        Ok(())
    }

    /// The owner is the login record and its Account root: a key naming the
    /// same login with a different Account root owns nothing and is released.
    #[tokio::test]
    async fn blob_owner_requires_matching_account_root() -> Result<()> {
        let (instance, mut account) = instance_with_account().await?;
        let key = account.get_default_key()?;
        let database = account.create_database(Doc::new(), &key).await?;
        let blob = instance.put_blob(b"owner check".to_vec()).await?;
        let impostor = BlobOwner::new(account.login_id(), database.root_id())?;
        assert!(
            instance
                .set_wanted_blobs_local(&impostor, database.root_id(), BTreeSet::from([blob]))
                .await
                .is_err()
        );
        assert!(BlobOwner::parse(&owner_of(&account).key()) == Some(owner_of(&account)));
        assert!(BlobOwner::parse("not-a-registration").is_none());
        let too_many: Vec<ID> = (0..=MAX_WANTED_BLOBS)
            .map(|i| ID::from_bytes(i.to_le_bytes()))
            .collect();
        assert!(matches!(
            database.set_wanted_blobs(too_many).await,
            Err(Error::Account(e)) if matches!(*e, AccountError::TooManyWantedBlobs { .. })
        ));
        assert!(
            database
                .set_wanted_blobs([ID::from_dagcbor_bytes(b"not raw")])
                .await
                .is_err()
        );
        Ok(())
    }
}
