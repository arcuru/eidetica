//! Restricted, same-UID Unix client for the web dashboard.
//!
//! Unlike `Instance::connect`, this never constructs a backend or exposes raw
//! database operations. Each login owns its own connection and credential bundle.

use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    Clock, ID, Result, Snapshot, SystemClock,
    account::{AccountError, KeyStorage, TrackedDatabase, UnlockedCredentials},
    auth::{Permission, crypto::PublicKey},
    service::{
        RemoteConnection,
        protocol::{ServiceRequest, ServiceResponse, TicketBootstrapOutcome},
    },
    sync::{DatabaseTicket, protocol::SyncRequestAuth},
};

pub mod browser;

// Deliberately different from the general service's required version. No
// negotiation, probing with credentials, or fallback to the general profile.
pub(super) const PROTOCOL_VERSION: u32 = 0x45494447;
const DEADLINE: Duration = Duration::from_secs(45);

#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Ping,
    Credentials,
    Ready {
        bootstrap: Option<Box<crate::Entry>>,
    },
    Databases,
    Database(ID),
    Browse(Box<browser::Location>),
    Preferences {
        root: ID,
        enabled: bool,
        on_commit: bool,
        interval: Option<u64>,
    },
    PrepareJoin {
        ticket: DatabaseTicket,
        permission: Permission,
    },
    Join {
        token: String,
        proof: SyncRequestAuth,
    },
    AccessRequests,
    Decide {
        root: ID,
        id: String,
        reject: bool,
    },
    SubmitGrant {
        token: String,
        signed: Box<crate::Entry>,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Reply {
    Ok,
    Credentials {
        bundle: Box<KeyStorage>,
        keys: Vec<KeyInfo>,
        bootstrap: Option<Box<crate::Entry>>,
    },
    Databases(Vec<DatabaseSummary>),
    Database(Option<DatabaseSummary>),
    Browser(Option<Box<browser::Page>>),
    JoinRoute {
        token: String,
        peer: PublicKey,
        tips: Snapshot,
        key: PublicKey,
    },
    Joined(JoinOutcome),
    AccessRequests(Vec<QueueRow>),
    Grant {
        token: String,
        key: PublicKey,
        draft: Box<crate::Entry>,
    },
}

/// Public key metadata presented by the dashboard, never key bytes or mappings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    pub id: PublicKey,
    pub name: Option<String>,
    pub default: bool,
}

/// One tracked row and the outcome of reading it with the login's mapped key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseSummary {
    pub tracked: TrackedDatabase,
    pub name: Result<String, String>,
    pub entries: Result<(usize, Vec<ID>), String>,
}

/// The fields used by the access-request page; no peer address or free-form metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueRow {
    pub id: String,
    pub root: ID,
    pub key: PublicKey,
    pub name: String,
    pub permission: Permission,
    pub timestamp: String,
}

impl From<(String, crate::sync::BootstrapRequest)> for QueueRow {
    fn from((id, row): (String, crate::sync::BootstrapRequest)) -> Self {
        Self {
            id,
            root: row.tree_id,
            key: row.requesting_pubkey,
            name: row.requesting_key_name,
            permission: row.requested_permission,
            timestamp: row.timestamp,
        }
    }
}

/// Distinguish a successful transfer from a confirmed tracking update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JoinOutcome {
    Complete,
    Pending(String),
    Rejected(String),
    BookkeepingUnconfirmed,
}

/// A dashboard transport, still unauthenticated until `login` succeeds.
pub struct Connection(RemoteConnection);

impl Connection {
    /// Verify the dashboard profile before sending any login information.
    pub async fn connect(socket: &Path) -> Result<Self> {
        Ok(Self(
            tokio::time::timeout(
                Duration::from_secs(5),
                RemoteConnection::connect_version(socket, PROTOCOL_VERSION),
            )
            .await
            .map_err(|_| timed_out())??,
        ))
    }

    /// Endpoint liveness only; grants no login or target access.
    pub async fn ping(&self) -> Result<()> {
        self.ok(Request::Ping).await
    }

    /// Consume a fresh connection. A failed attempt leaves no reusable handle.
    pub async fn login(self, username: &str, password: &str) -> Result<Login> {
        // The password is used only by the local unlock in trusted_login.
        let (login_id, info, anchor) = self.0.trusted_login(username, password).await?;
        let Reply::Credentials {
            bundle,
            keys,
            bootstrap,
        } = self.request(Request::Credentials).await?
        else {
            return Err(unexpected());
        };
        let unlocked = anchor.unlock_storage(&bundle)?;
        unlocked.signer(&info.credentials.root_key_id)?;
        let unlocked_ids: std::collections::HashSet<_> = unlocked.key_ids().into_iter().collect();
        if keys.iter().any(|key| !unlocked_ids.contains(&key.id)) {
            return Err(AccountError::InvalidLocalState.into());
        }
        for key in &keys {
            self.0
                .register_session_key(&unlocked.signer(&key.id)?)
                .await?;
        }
        let bootstrap = bootstrap
            .map(|draft| -> Result<_> {
                if draft.root().as_ref() != Some(&info.account_id) {
                    return Err(unexpected());
                }
                Ok(Box::new(
                    draft.sign(&unlocked.signer(&info.credentials.root_key_id)?)?,
                ))
            })
            .transpose()?;
        self.ok(Request::Ready { bootstrap }).await?;
        Ok(Login {
            connection: self,
            username: info.username,
            login_id,
            root: info.account_id,
            keys,
            unlocked,
            action: tokio::sync::Mutex::new(()),
        })
    }

    async fn request(&self, request: Request) -> Result<Reply> {
        let response = tokio::time::timeout(
            DEADLINE,
            self.0
                .request_ok(ServiceRequest::Dashboard(Box::new(request))),
        )
        .await;
        match response {
            Ok(Ok(ServiceResponse::Dashboard(reply))) => Ok(*reply),
            Ok(Ok(_)) => Err(unexpected()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // A timed-out write may already have been dispatched. Retire
                // this login; neither reconnect nor replay on its behalf.
                self.0.retire();
                Err(timed_out())
            }
        }
    }

    async fn ok(&self, request: Request) -> Result<()> {
        match self.request(request).await? {
            Reply::Ok => Ok(()),
            _ => Err(unexpected()),
        }
    }
}

/// One proof-bound browser login. No method changes its authenticated actor.
pub struct Login {
    connection: Connection,
    pub username: String,
    pub login_id: String,
    pub root: ID,
    pub keys: Vec<KeyInfo>,
    unlocked: UnlockedCredentials,
    // Serialize prepare/sign/submit, not independent page reads or retirement.
    action: tokio::sync::Mutex<()>,
}

impl Login {
    pub fn retire(&self) {
        self.connection.0.retire();
    }
    pub fn is_closed(&self) -> bool {
        self.connection.0.is_closed()
    }
    /// Resolve when the reader detects EOF or the login is explicitly retired.
    pub async fn disconnected(&self) {
        self.connection.0.disconnected().await;
    }

    pub async fn databases(&self) -> Result<Vec<DatabaseSummary>> {
        match self.connection.request(Request::Databases).await? {
            Reply::Databases(rows) => Ok(rows),
            _ => Err(unexpected()),
        }
    }

    pub async fn database(&self, root: ID) -> Result<Option<DatabaseSummary>> {
        match self.connection.request(Request::Database(root)).await? {
            Reply::Database(row) => Ok(row),
            _ => Err(unexpected()),
        }
    }

    pub async fn browse(&self, location: browser::Location) -> Result<Option<Box<browser::Page>>> {
        match self
            .connection
            .request(Request::Browse(Box::new(location)))
            .await?
        {
            Reply::Browser(page) => Ok(page),
            _ => Err(unexpected()),
        }
    }

    pub async fn preferences(
        &self,
        root: ID,
        enabled: bool,
        on_commit: bool,
        interval: Option<u64>,
    ) -> Result<()> {
        self.connection
            .ok(Request::Preferences {
                root,
                enabled,
                on_commit,
                interval,
            })
            .await
    }

    pub async fn join(
        &self,
        ticket: DatabaseTicket,
        permission: Permission,
    ) -> Result<JoinOutcome> {
        let _guard = self.action.lock().await;
        let root = ticket.database_id().clone();
        let Reply::JoinRoute {
            token,
            peer,
            tips,
            key,
        } = self
            .connection
            .request(Request::PrepareJoin { ticket, permission })
            .await?
        else {
            return Err(unexpected());
        };
        let proof = SyncRequestAuth::sign_access(
            &self.unlocked.signer(&key)?,
            &peer,
            &root,
            &tips,
            SystemClock.now_millis(),
            None,
        );
        match self
            .connection
            .request(Request::Join { token, proof })
            .await?
        {
            Reply::Joined(outcome) => Ok(outcome),
            _ => Err(unexpected()),
        }
    }

    pub async fn access_requests(&self) -> Result<Vec<QueueRow>> {
        match self.connection.request(Request::AccessRequests).await? {
            Reply::AccessRequests(requests) => Ok(requests),
            _ => Err(unexpected()),
        }
    }

    pub async fn decide(&self, root: ID, id: String, reject: bool) -> Result<()> {
        let _guard = self.action.lock().await;
        match self
            .connection
            .request(Request::Decide {
                root: root.clone(),
                id,
                reject,
            })
            .await?
        {
            Reply::Ok => Ok(()),
            Reply::Grant { token, key, draft }
                if !reject && draft.root().as_ref() == Some(&root) =>
            {
                let signed = Box::new(draft.sign(&self.unlocked.signer(&key)?)?);
                self.connection
                    .ok(Request::SubmitGrant { token, signed })
                    .await
            }
            _ => Err(unexpected()),
        }
    }
}

impl From<TicketBootstrapOutcome> for JoinOutcome {
    fn from(outcome: TicketBootstrapOutcome) -> Self {
        match outcome {
            TicketBootstrapOutcome::Complete => Self::Complete,
            TicketBootstrapOutcome::Pending { request_id, .. } => Self::Pending(request_id),
            TicketBootstrapOutcome::Rejected { request_id, .. } => Self::Rejected(request_id),
        }
    }
}

fn unexpected() -> crate::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "unexpected dashboard response",
    )
    .into()
}

fn timed_out() -> crate::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "dashboard daemon did not respond in time; outcome not confirmed",
    )
    .into()
}

#[cfg(test)]
mod tests;
