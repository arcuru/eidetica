use super::{
    ConnectionState, ServiceRequest, ServiceResponse, TicketBootstrapRequest, gate_tree_permission,
    handle_session_key_challenge, handle_session_key_register, handle_ticket_bootstrap,
    handle_trusted_login_account, handle_trusted_login_prove,
};
use crate::{
    Database, ID, Instance, Result,
    account::{
        AccountInfo, AccountKey, SyncSettings, TrackedDatabase, local_logins,
        local_state::{self, LocalAccountState},
    },
    auth::{Permission, SigKey, crypto::PublicKey},
    service::{
        dashboard::{
            DatabaseSummary, JoinOutcome, KeyInfo, Reply, Request,
            browser::{self, application_root, authorized_database, denied},
        },
        protocol::TicketBootstrapOutcome,
    },
    sync::{SyncError, peer_types::Address},
};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

const UNAVAILABLE: &str = "Target inaccessible or unavailable";
const PREPARATION_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Session {
    challenged_root: Option<ID>,
    ready: bool,
    join: Option<PendingJoin>,
    grant: Option<PendingGrant>,
    device_read: Option<(crate::Snapshot, crate::transaction::PreparedEntry)>,
}

struct PendingJoin {
    token: String,
    expires: Instant,
    root: ID,
    address: Address,
    peer: PublicKey,
    key: PublicKey,
    permission: Permission,
    before: Option<TrackedDatabase>,
}

struct PendingGrant {
    token: String,
    expires: Instant,
    root: ID,
    grant: crate::sync::PreparedAccessGrant,
}

impl Session {
    pub(super) async fn dispatch(
        &mut self,
        instance: &Instance,
        state: &mut ConnectionState,
        request: ServiceRequest,
    ) -> Result<ServiceResponse> {
        match request {
            ServiceRequest::TrustedLoginAccount { username }
                if matches!(state, ConnectionState::PreAuth) =>
            {
                let response = handle_trusted_login_account(instance, state, username).await?;
                if let ServiceResponse::TrustedLoginChallenge { account_info, .. } = &response {
                    self.challenged_root = Some(account_info.account_id.clone());
                }
                Ok(response)
            }
            ServiceRequest::TrustedLoginProve { signature }
                if matches!(state, ConnectionState::AwaitingProof { .. }) =>
            {
                let reply = handle_trusted_login_prove(state, &signature)?;
                let login = self.active(instance, state).await?;
                login.account(instance).await?;
                Ok(reply)
            }
            ServiceRequest::Dashboard(request) => {
                let response = self.execute(instance, state, *request).await?;
                Ok(ServiceResponse::Dashboard(Box::new(response)))
            }
            ServiceRequest::SessionKeyChallenge { pubkey } => {
                let login = self.active(instance, state).await?;
                if !login.local.keys.contains_key(&pubkey.to_string()) {
                    return Err(denied());
                }
                handle_session_key_challenge(state, pubkey)
            }
            ServiceRequest::SessionKeyRegister { pubkey, signature } => {
                let login = self.active(instance, state).await?;
                if !login.local.keys.contains_key(&pubkey.to_string()) {
                    return Err(denied());
                }
                handle_session_key_register(state, pubkey, &signature)
            }
            // Never proxy arbitrary service operations, including signer-only
            // authentication, raw reads, signed Entry ingestion, or native CAS.
            _ => Ok(ServiceResponse::Unsupported),
        }
    }

    async fn active<'a>(
        &self,
        instance: &Instance,
        state: &'a ConnectionState,
    ) -> Result<Actor<'a>> {
        let ConnectionState::Authenticated {
            username,
            login_id,
            login_pubkey,
            session_keyset,
            ..
        } = state
        else {
            return Err(denied());
        };
        let challenged_root = self.challenged_root.as_ref().ok_or_else(denied)?;
        let (current_id, info) = local_logins::lookup(instance, username)
            .await
            .map_err(|_| denied())?;
        if &current_id != login_id
            || &info.account_id != challenged_root
            || &info.credentials.root_key_id != login_pubkey
        {
            return Err(denied());
        }
        let mut local = local_state::read(instance, login_id).await?;
        initialise(&mut local, &info);
        Ok(Actor {
            login_id,
            info,
            local,
            proven: session_keyset,
        })
    }

    async fn execute(
        &mut self,
        instance: &Instance,
        state: &ConnectionState,
        request: Request,
    ) -> Result<Reply> {
        if matches!(request, Request::Ping) {
            return Ok(Reply::Ok);
        }
        let actor = self.active(instance, state).await?;
        let (account, account_tx, account_snapshot) = actor.account(instance).await?;
        match request {
            Request::Credentials if !self.ready => {
                self.device_read =
                    match prepare_device_read(instance, &actor.info, &account_tx).await {
                        Ok(draft) => draft.map(|draft| (account_snapshot.clone(), draft)),
                        Err(e) => {
                            tracing::debug!("dashboard device Read preparation skipped: {e}");
                            None
                        }
                    };
                return Ok(Reply::Credentials {
                    bundle: Box::new(actor.local.credentials.clone().ok_or_else(denied)?),
                    keys: actor
                        .local
                        .keys
                        .values()
                        .map(|key| KeyInfo {
                            id: key.key_id.clone(),
                            name: key.display_name.clone(),
                            default: key.is_default,
                        })
                        .collect(),
                    bootstrap: self
                        .device_read
                        .as_ref()
                        .map(|(_, draft)| Box::new(draft.entry.clone())),
                });
            }
            Request::Ready { bootstrap } if !self.ready => {
                if actor
                    .local
                    .keys
                    .values()
                    .any(|key| !actor.proven.contains(&key.key_id))
                {
                    return Err(denied());
                }
                match (self.device_read.take(), bootstrap) {
                    (Some((snapshot, draft)), Some(signed)) => {
                        if let Err(e) =
                            finish_device_read(instance, &account, snapshot, draft, *signed).await
                        {
                            tracing::debug!("dashboard device Read bootstrap skipped: {e}");
                        }
                    }
                    (None, None) => {}
                    _ => return Err(denied()),
                }
                local_state::update(instance, actor.login_id, |local| {
                    initialise(local, &actor.info);
                    local.last_login = Some(instance.clock().now_secs());
                    Ok(())
                })
                .await?;
                self.ready = true;
                if let Err(e) =
                    local_state::refresh_sync(instance, actor.login_id, &actor.info.account_id)
                        .await
                {
                    tracing::warn!("Could not reconcile dashboard login's sync preferences: {e}");
                }
                return Ok(Reply::Ok);
            }
            _ if !self.ready => return Err(denied()),
            _ => {}
        }
        match request {
            Request::Databases => {
                let mut rows = Vec::new();
                for tracked in actor.local.databases.values() {
                    rows.push(actor.summary(instance, tracked, 0).await);
                }
                Ok(Reply::Databases(rows))
            }
            Request::Database(root) => {
                let summary = match actor.tracked(&root) {
                    Some(tracked) => {
                        let row = actor.summary(instance, tracked, 100).await;
                        // A tracked locator is harmless on the list. A detail
                        // must not disclose data for a now-unreadable target.
                        if matches!(&row.name, Err(e) if e == UNAVAILABLE) {
                            None
                        } else {
                            Some(row)
                        }
                    }
                    None => None,
                };
                Ok(Reply::Database(summary))
            }
            Request::Browse(location) => Ok(Reply::Browser(
                browser::proven(
                    instance,
                    &account,
                    account_tx,
                    &actor.local,
                    actor.proven,
                    &location,
                )
                .await
                .map(Box::new),
            )),
            Request::Preferences {
                root,
                enabled,
                on_commit,
                interval,
            } => {
                let tracked = actor.tracked(&root).ok_or_else(denied)?;
                let identity = actor.mapping(&tracked.key_id, &root).ok_or_else(denied)?;
                actor.tracked_application(instance, tracked).await?;
                if interval.is_some_and(|seconds| {
                    seconds == 0
                        || Instant::now()
                            .checked_add(Duration::from_secs(seconds))
                            .is_none()
                }) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid sync interval",
                    )
                    .into());
                }
                local_state::update(instance, actor.login_id, |local| {
                    if mapping(local, &tracked.key_id, &root).as_ref() != Some(&identity) {
                        return Err(denied());
                    }
                    let row = local
                        .databases
                        .get_mut(&root.to_string())
                        .filter(|row| row.key_id == tracked.key_id)
                        .ok_or_else(denied)?;
                    row.sync_settings.sync_enabled = enabled;
                    row.sync_settings.sync_on_commit = on_commit;
                    row.sync_settings.interval_seconds = interval;
                    Ok(())
                })
                .await?;
                local_state::refresh_sync(instance, actor.login_id, &actor.info.account_id).await?;
                Ok(Reply::Ok)
            }
            Request::PrepareJoin { ticket, permission } => {
                let root = ticket.database_id().clone();
                let key = actor.default_key().cloned().ok_or_else(denied)?;
                if !application_root(instance, &root)
                    || root == actor.info.account_id
                    || !matches!(
                        permission,
                        Permission::Read | Permission::Write(10) | Permission::Admin(0)
                    )
                {
                    return Err(denied());
                }
                // For a locally available target, refuse before doing network
                // work unless this key can already read it through the service gate.
                gate_tree_permission(
                    instance,
                    &key,
                    &SigKey::from_pubkey(&key),
                    &root,
                    Permission::Read,
                    false,
                )
                .await?;
                let sync = instance.sync().ok_or(SyncError::SyncNotEnabled)?;
                let (address, peer, tips) = sync.prepare_ticket_bootstrap(&ticket).await?;
                let token = uuid::Uuid::new_v4().to_string();
                let before = actor.tracked(&root).cloned();
                self.join = Some(PendingJoin {
                    token: token.clone(),
                    expires: Instant::now() + PREPARATION_TTL,
                    root,
                    address,
                    peer: peer.clone(),
                    key: key.clone(),
                    permission,
                    before,
                });
                Ok(Reply::JoinRoute {
                    token,
                    peer,
                    tips,
                    key,
                })
            }
            Request::Join { token, proof } => {
                let pending = self
                    .join
                    .take_if(|p| p.token == token)
                    .filter(|p| {
                        p.expires > Instant::now()
                            && p.key == proof.key
                            && !proof.requires_direct_key()
                            && Some(&p.key) == actor.default_key()
                    })
                    .ok_or_else(denied)?;
                let response = handle_ticket_bootstrap(
                    instance,
                    state,
                    TicketBootstrapRequest {
                        database_id: pending.root.clone(),
                        address: pending.address,
                        peer: pending.peer,
                        tips: crate::Snapshot::EMPTY,
                        requesting_key_name: pending.key.to_string(),
                        requested_permission: pending.permission,
                        metadata: None,
                        auth: proof,
                    },
                )
                .await?;
                let ServiceResponse::TicketBootstrapOutcome(outcome) = response else {
                    return Err(denied());
                };
                if matches!(outcome, TicketBootstrapOutcome::Complete) {
                    let record = async {
                        let fresh = self.active(instance, state).await?;
                        fresh
                            .record_join(
                                instance,
                                &pending.root,
                                &pending.key,
                                pending.permission,
                                pending.before,
                            )
                            .await
                    };
                    if record.await.is_err() {
                        return Ok(Reply::Joined(JoinOutcome::BookkeepingUnconfirmed));
                    }
                }
                Ok(Reply::Joined(outcome.into()))
            }
            Request::AccessRequests => {
                let sync = instance.sync().ok_or(SyncError::SyncNotEnabled)?;
                let roots: BTreeSet<_> = actor
                    .local
                    .keys
                    .values()
                    .flat_map(|key| key.database_sigkeys.keys())
                    .collect();
                let mut rows = Vec::new();
                for root in roots {
                    if let Some((key, identity)) = actor.admin(instance, root).await {
                        rows.extend(
                            sync.pending_access_requests_for(root, &key, &identity)
                                .await?
                                .into_iter()
                                .map(Into::into),
                        );
                    }
                }
                Ok(Reply::AccessRequests(rows))
            }
            Request::Decide { root, id, reject } => {
                let (key, identity) = actor.admin(instance, &root).await.ok_or_else(denied)?;
                let sync = instance.sync().ok_or(SyncError::SyncNotEnabled)?;
                if !reject
                    && let Some(grant) = sync
                        .prepare_access_grant(&root, &id, &key, &identity)
                        .await?
                {
                    let token = uuid::Uuid::new_v4().to_string();
                    let draft = Box::new(grant.draft.entry.clone());
                    self.grant = Some(PendingGrant {
                        token: token.clone(),
                        expires: Instant::now() + PREPARATION_TTL,
                        root,
                        grant,
                    });
                    return Ok(Reply::Grant { token, key, draft });
                }
                sync.finish_access_request(&root, &id, &key, &identity, reject)
                    .await?;
                Ok(Reply::Ok)
            }
            Request::SubmitGrant { token, signed } => {
                let pending = self
                    .grant
                    .take_if(|p| p.token == token)
                    .filter(|p| p.expires > Instant::now())
                    .ok_or_else(denied)?;
                let (key, identity) = actor
                    .admin(instance, &pending.root)
                    .await
                    .ok_or_else(denied)?;
                instance
                    .sync()
                    .ok_or(SyncError::SyncNotEnabled)?
                    .commit_access_grant(pending.grant, *signed, &key, &identity)
                    .await?;
                Ok(Reply::Ok)
            }
            _ => Err(denied()),
        }
    }
}

async fn prepare_device_read(
    instance: &Instance,
    info: &AccountInfo,
    tx: &crate::Transaction,
) -> Result<Option<crate::transaction::PreparedEntry>> {
    let settings = tx.get_settings()?;
    let device = instance.id();
    if settings.get_auth_key(&device).await.is_ok() {
        return Ok(None);
    }
    settings
        .set_auth_key(
            &device,
            crate::auth::AuthKey::active(Some("device"), Permission::Read),
        )
        .await?;
    tx.prepare_entry(&SigKey::from_pubkey(&info.credentials.root_key_id))
        .await
        .map(Some)
}

async fn finish_device_read(
    instance: &Instance,
    account: &Database,
    snapshot: crate::Snapshot,
    draft: crate::transaction::PreparedEntry,
    signed: crate::Entry,
) -> Result<()> {
    let root = account.root_id();
    let guard = instance.tree_lock(root).lock_owned().await;
    if crate::Snapshot::from(account.verified_frontier().await?) != snapshot
        || instance.backend().snapshot(root).await? != snapshot
    {
        return Err(denied());
    }
    draft.validate_signed(instance, &signed).await?;
    instance
        .put_entry_under_tree_lock(
            guard,
            root,
            crate::backend::VerificationStatus::Verified,
            signed,
            crate::instance::WriteSource::Local,
        )
        .await
}

fn initialise(local: &mut LocalAccountState, info: &AccountInfo) {
    local
        .credentials
        .get_or_insert_with(|| info.credentials.bundle.clone());
    let key = &info.credentials.root_key_id;
    local
        .keys
        .entry(key.to_string())
        .or_insert_with(|| AccountKey {
            key_id: key.clone(),
            display_name: Some("Root Key".into()),
            created_at: info.created_at,
            last_used: None,
            is_default: true,
            database_sigkeys: Default::default(),
        });
}

struct Actor<'a> {
    login_id: &'a str,
    info: AccountInfo,
    local: LocalAccountState,
    proven: &'a std::collections::HashSet<PublicKey>,
}

impl Actor<'_> {
    async fn account(
        &self,
        instance: &Instance,
    ) -> Result<(Database, crate::Transaction, crate::Snapshot)> {
        if !application_root(instance, &self.info.account_id) {
            return Err(denied());
        }
        let key = &self.info.credentials.root_key_id;
        authorized_database(
            instance,
            &self.info.account_id,
            key,
            &SigKey::from_pubkey(key),
        )
        .await
    }

    fn tracked(&self, root: &ID) -> Option<&TrackedDatabase> {
        self.local
            .databases
            .get(&root.to_string())
            .filter(|row| &row.database_id == root)
    }

    fn mapping(&self, key: &PublicKey, root: &ID) -> Option<SigKey> {
        self.proven
            .contains(key)
            .then(|| mapping(&self.local, key, root))
            .flatten()
    }

    fn default_key(&self) -> Option<&PublicKey> {
        let mut defaults = self.local.keys.values().filter(|k| k.is_default);
        let key = &defaults.next()?.key_id;
        (defaults.next().is_none() && self.proven.contains(key)).then_some(key)
    }

    async fn tracked_application(
        &self,
        instance: &Instance,
        row: &TrackedDatabase,
    ) -> Result<(crate::crdt::Doc, crate::Snapshot)> {
        let root = &row.database_id;
        if !application_root(instance, root) || root == &self.info.account_id {
            return Err(denied());
        }
        let identity = self.mapping(&row.key_id, root).ok_or_else(denied)?;
        let (db, tx, snapshot) =
            authorized_database(instance, root, &row.key_id, &identity).await?;
        let settings = browser::application_settings(&db, &tx)
            .await?
            .ok_or_else(denied)?;
        Ok((settings, snapshot))
    }

    async fn summary(
        &self,
        instance: &Instance,
        tracked: &TrackedDatabase,
        limit: usize,
    ) -> DatabaseSummary {
        let (name, entries) = match self.tracked_application(instance, tracked).await {
            Ok((settings, observed)) => {
                let result = async {
                    let backend = instance.require_local_engine()?;
                    let entries = backend
                        .verified_entry_summary(&tracked.database_id, limit)
                        .await?;
                    if backend.verified_snapshot(&tracked.database_id).await? != observed {
                        return Err(denied());
                    }
                    Ok(entries)
                };
                let entries = tokio::time::timeout(Duration::from_secs(5), result)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .ok_or_else(|| "Could not read verified entries".into());
                let name = match settings.get("name") {
                    None => Ok(String::new()),
                    Some(value) => value
                        .as_text()
                        .map(str::to_owned)
                        .ok_or_else(|| "Could not read database name".into()),
                };
                (name, entries)
            }
            Err(_) => (Err(UNAVAILABLE.into()), Err(UNAVAILABLE.into())),
        };
        DatabaseSummary {
            tracked: tracked.clone(),
            name,
            entries,
        }
    }

    async fn admin(&self, instance: &Instance, root: &ID) -> Option<(PublicKey, SigKey)> {
        if !application_root(instance, root)
            || root == &self.info.account_id
            || !instance.has_database(root).await
        {
            return None;
        }
        let db = Database::open(instance, root).await.ok()?;
        let auth = db.live_auth_settings().await.ok()?;
        let mut best = None;
        for key in self.local.keys.values() {
            if let Some(identity) = self.mapping(&key.key_id, root)
                && let Ok(permission) =
                    crate::auth::validation::permissions::resolve_identity_permission(
                        &key.key_id,
                        &identity,
                        &auth,
                        Some(instance),
                    )
                    .await
                && permission.can_admin()
                && best
                    .as_ref()
                    .is_none_or(|(current, _, _)| permission > *current)
            {
                best = Some((permission, key.key_id.clone(), identity));
            }
        }
        best.map(|(_, key, identity)| (key, identity))
    }

    async fn record_join(
        &self,
        instance: &Instance,
        root: &ID,
        key: &PublicKey,
        permission: Permission,
        before: Option<TrackedDatabase>,
    ) -> Result<()> {
        if Some(key) != self.default_key() {
            return Err(denied());
        }
        let (identity, _) = Database::find_sigkeys(instance, root, key)
            .await?
            .into_iter()
            .find(|(_, granted)| granted >= &permission)
            .ok_or_else(denied)?;
        let (db, tx, _) = authorized_database(instance, root, key, &identity).await?;
        if browser::application_settings(&db, &tx).await?.is_none() {
            return Err(denied());
        }
        local_state::update(instance, self.login_id, |local| {
            let meta = local
                .keys
                .get_mut(&key.to_string())
                .filter(|meta| meta.is_default)
                .ok_or_else(denied)?;
            let row = local.databases.get(&root.to_string());
            if row != before.as_ref() {
                // Do not overwrite another session's preference/key edit.
                return Err(denied());
            }
            meta.database_sigkeys
                .insert(root.clone(), Some(identity.clone()));
            local.databases.insert(
                root.to_string(),
                TrackedDatabase {
                    database_id: root.clone(),
                    key_id: key.clone(),
                    sync_settings: SyncSettings::enabled().with_interval(13),
                },
            );
            Ok(())
        })
        .await?;
        local_state::refresh_sync(instance, self.login_id, &self.info.account_id).await
    }
}

fn mapping(local: &LocalAccountState, key: &PublicKey, root: &ID) -> Option<SigKey> {
    local
        .keys
        .get(&key.to_string())?
        .database_sigkeys
        .get(root)?
        .clone()
        .or_else(|| Some(SigKey::from_pubkey(key)))
}
