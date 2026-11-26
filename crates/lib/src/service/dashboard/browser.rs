//! Anchored navigation through named references, shared by both dashboard modes.

use std::io::Cursor;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Database, ID, Instance, Result, Snapshot, Transaction,
    account::{Account, AccountSession, Identity, IdentityStatus},
    auth::{SigKey, crypto::PublicKey, types::DelegationStep},
    store::{Registered, Table},
};

pub const MAX_DEPTH: usize = 12;
pub const MAX_QUERY: usize = 16 * 1024;
pub const ROW_LIMIT: usize = 10;
const VALUE_BYTES: usize = 2048;

type References = Vec<(String, Option<ID>)>;

/// A public selection, not an assertion of authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selection {
    pub name: String,
    pub root: ID,
    pub key: PublicKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub name: String,
    pub root: ID,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Location {
    pub identity: Option<Selection>,
    pub path: Vec<Link>,
    pub namespace: Option<String>,
    pub data: Option<Link>,
    pub store: Option<String>,
}

impl Location {
    pub fn valid(&self) -> bool {
        self.path.len() <= MAX_DEPTH
            && (self.path.is_empty() || self.identity.is_some())
            && (!self.path.is_empty() || self.namespace.is_none())
            && (self.data.is_none() || self.namespace.as_deref().is_some_and(data_namespace))
            && (self.store.is_none() || self.data.is_some())
            && self
                .identity
                .iter()
                .map(|s| s.name.len())
                .chain(self.path.iter().map(|l| l.name.len()))
                .chain(self.namespace.iter().map(String::len))
                .chain(self.data.iter().map(|l| l.name.len()))
                .chain(self.store.iter().map(String::len))
                .sum::<usize>()
                <= MAX_QUERY
    }
}

pub fn data_namespace(namespace: &str) -> bool {
    !matches!(namespace, "accounts" | "identities")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub viewed_root: ID,
    pub viewed_name: String,
    pub content: Content,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    References {
        identities: Option<Result<Vec<Selection>, ()>>,
        accounts: Result<References, ()>,
        data: Option<Result<DataReferences, ()>>,
    },
    Preview(Preview),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataReferences {
    pub namespaces: Vec<String>,
    pub selected: Option<(String, Result<References, ()>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub name: String,
    pub root: ID,
    pub stores: Vec<String>,
    pub selected: Option<String>,
    pub rows: Option<Vec<(String, String)>>,
    pub more: bool,
}

enum Actor {
    Combined(Box<Identity>),
    Proven(PublicKey, Box<SigKey>),
}

impl Actor {
    async fn open(&self, instance: &Instance, root: &ID) -> Result<(Database, Transaction)> {
        match self {
            Self::Combined(identity) => {
                let db = identity.open_database(root).await?;
                let tx = Account::new(db.clone())
                    .reference_read_transaction()
                    .await?;
                Ok((db, tx))
            }
            Self::Proven(key, identity) => authorized_database(instance, root, key, identity)
                .await
                .map(|(db, tx, _)| (db, tx)),
        }
    }
}

pub(in crate::service) fn denied() -> crate::Error {
    crate::auth::AuthError::PermissionDenied {
        reason: "dashboard target inaccessible or unavailable".into(),
    }
    .into()
}

pub(in crate::service) async fn authorized_database(
    instance: &Instance,
    root: &ID,
    key: &PublicKey,
    identity: &SigKey,
) -> Result<(Database, Transaction, Snapshot)> {
    if !instance.has_database(root).await {
        return Err(denied());
    }
    let db = Database::open(instance, root).await?;
    let snapshot = db.snapshot().await?;
    if snapshot.is_empty() {
        return Err(denied());
    }
    let auth = db.auth_settings_at(&snapshot).await?;
    crate::auth::validation::permissions::resolve_identity_permission(
        key,
        identity,
        &auth,
        Some(instance),
    )
    .await?;
    let tx = db.new_transaction_at(&snapshot).await?;
    Ok((db, tx, snapshot))
}

pub(in crate::service) fn application_root(instance: &Instance, root: &ID) -> bool {
    instance.users_db_id() != root
        && instance.databases_db_id() != root
        && instance
            .sync()
            .is_none_or(|sync| sync.sync_tree_root_id() != root)
}

/// Read the same projection using the embedded AccountSession. This path
/// preserves the existing keyed Database opens and credential ownership.
pub async fn embedded(
    instance: &Instance,
    user: &AccountSession,
    location: &Location,
) -> Option<Page> {
    if !location.valid() {
        return None;
    }
    let identity = match &location.identity {
        Some(selection) => {
            let identity = user.get_identity(&selection.name).await.ok()??;
            if identity.root_id() != &selection.root || identity.key_id() != &selection.key {
                return None;
            }
            Some(Actor::Combined(Box::new(identity)))
        }
        None => None,
    };
    let identities = if location.identity.is_none() {
        Some(
            user.identities()
                .await
                .map(|choices| {
                    choices
                        .into_iter()
                        .filter(|(_, tracked)| tracked.status == IdentityStatus::Active)
                        .map(|(name, tracked)| Selection {
                            name,
                            root: tracked.root_id,
                            key: tracked.key_id,
                        })
                        .collect()
                })
                .map_err(|_| ()),
        )
    } else {
        None
    };
    let tx = user.account().reference_read_transaction().await.ok()?;
    page(
        instance,
        user.account().id(),
        tx,
        identity,
        identities,
        location,
    )
    .await
}

/// Read a projection using only the connection's proven keys and current
/// installation mappings. Signerless Database handles never leave the daemon.
pub(in crate::service) async fn proven(
    instance: &Instance,
    account: &Database,
    account_tx: Transaction,
    local: &crate::account::local_state::LocalAccountState,
    proven: &std::collections::HashSet<PublicKey>,
    location: &Location,
) -> Option<Page> {
    if !location.valid() {
        return None;
    }
    let identity = match &location.identity {
        Some(selection) => {
            let tracked = local.identities.get(&selection.name)?;
            if tracked.status != IdentityStatus::Active
                || tracked.root_id != selection.root
                || tracked.key_id != selection.key
                || !proven.contains(&selection.key)
                || !local.keys.contains_key(&selection.key.to_string())
                || !application_root(instance, &selection.root)
                || &selection.root == account.root_id()
                || !matches_reference(
                    &account_tx,
                    "identities",
                    &Link {
                        name: selection.name.clone(),
                        root: selection.root.clone(),
                    },
                )
                .await
            {
                return None;
            }
            let (_, tx, snapshot) = authorized_database(
                instance,
                &selection.root,
                &selection.key,
                &SigKey::from_pubkey(&selection.key),
            )
            .await
            .ok()?;
            if !tx
                .get_settings()
                .ok()?
                .auth_snapshot()
                .await
                .ok()?
                .get_key_by_pubkey(&selection.key)
                .ok()?
                .is_active()
            {
                return None;
            }
            let identity = SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: selection.root.clone(),
                    tips: snapshot.into_tips(),
                }],
                hint: crate::auth::KeyHint::from_pubkey(&selection.key),
            };
            Some(Actor::Proven(selection.key.clone(), Box::new(identity)))
        }
        None => None,
    };
    let identities = location.identity.is_none().then(|| {
        Ok(local
            .identities
            .iter()
            .filter(|(_, tracked)| {
                tracked.status == IdentityStatus::Active && proven.contains(&tracked.key_id)
            })
            .map(|(name, tracked)| Selection {
                name: name.clone(),
                root: tracked.root_id.clone(),
                key: tracked.key_id.clone(),
            })
            .collect())
    });
    page(
        instance,
        account.root_id(),
        account_tx,
        identity,
        identities,
        location,
    )
    .await
}

async fn matches_reference(tx: &Transaction, namespace: &str, link: &Link) -> bool {
    matches!(Account::reference_at(tx, namespace, &link.name).await, Ok(Some(root)) if root == link.root)
}

async fn page(
    instance: &Instance,
    root: &ID,
    mut tx: Transaction,
    identity: Option<Actor>,
    identities: Option<Result<Vec<Selection>, ()>>,
    location: &Location,
) -> Option<Page> {
    let mut seen = vec![root];
    let mut current = root;
    for link in &location.path {
        if seen.contains(&&link.root)
            || !application_root(instance, &link.root)
            || !matches_reference(&tx, "accounts", link).await
        {
            return None;
        }
        let (_, opened) = identity.as_ref()?.open(instance, &link.root).await.ok()?;
        current = &link.root;
        tx = opened;
        seen.push(&link.root);
    }
    let viewed_name = if location.path.is_empty() {
        "Your Account".into()
    } else {
        tx.get_settings().ok()?.get_name().await.ok()?
    };
    let content = if let Some(link) = &location.data {
        if seen.contains(&&link.root)
            || !application_root(instance, &link.root)
            || location
                .identity
                .as_ref()
                .is_some_and(|s| s.root == link.root)
            || !matches_reference(&tx, location.namespace.as_deref()?, link).await
        {
            return None;
        }
        let (db, tx) = identity.as_ref()?.open(instance, &link.root).await.ok()?;
        Content::Preview(preview(&db, &tx, location, link).await.ok()??)
    } else {
        let accounts = Account::references_at(&tx, "accounts")
            .await
            .map_err(|_| ());
        let data = if location.path.is_empty() {
            None
        } else {
            Some(match Account::reference_namespaces_at(&tx).await {
                Ok(mut namespaces) => {
                    namespaces.retain(|name| data_namespace(name));
                    namespaces.sort();
                    let selected = if namespaces.is_empty() {
                        None
                    } else {
                        let name = location.namespace.as_ref().unwrap_or(&namespaces[0]);
                        if !namespaces.contains(name) {
                            return None;
                        }
                        Some((
                            name.clone(),
                            Account::references_at(&tx, name).await.map_err(|_| ()),
                        ))
                    };
                    Ok(DataReferences {
                        namespaces,
                        selected,
                    })
                }
                Err(_) => Err(()),
            })
        };
        Content::References {
            identities,
            accounts,
            data,
        }
    };
    Some(Page {
        viewed_root: current.clone(),
        viewed_name,
        content,
    })
}

pub(in crate::service) async fn application_settings(
    db: &Database,
    tx: &Transaction,
) -> Result<Option<crate::crdt::Doc>> {
    let current = tx.get_settings()?.get_all().await?;
    // Legacy personal Accounts used ordinary Tables for credentials. A kind
    // edit cannot make a system or legacy credential root eligible for preview.
    let original = db
        .new_transaction_at(&Snapshot::from(vec![db.root_id().clone()]))
        .await?
        .get_settings()?
        .get_all()
        .await?;
    if [&current, &original].into_iter().any(|s| {
        matches!(
            s.get_as::<&str>("type"),
            Some("system" | "user" | "sync_settings")
        )
    }) {
        return Ok(None);
    }
    Ok(Some(current))
}

async fn preview(
    db: &Database,
    tx: &Transaction,
    location: &Location,
    link: &Link,
) -> Result<Option<Preview>> {
    let Some(settings) = application_settings(db, tx).await? else {
        return Ok(None);
    };
    let index = tx.get_index().await?;
    let mut stores = index.list().await?;
    stores.retain(|name| !name.starts_with('_') && !name.starts_with("refs:"));
    stores.sort();
    let selected = match &location.store {
        Some(store) if stores.contains(store) => Some(store.clone()),
        Some(_) => return Ok(None),
        None => stores.first().cloned(),
    };
    let mut rows = None;
    let mut more = false;
    if let Some(store) = &selected
        && Table::<Value>::supports_type_id(&index.get_entry(store).await?.type_id)
    {
        let page = tx
            .get_store::<Table<Value>>(store)
            .await?
            .scan_page(None, ROW_LIMIT)
            .await?;
        more = page.next.is_some();
        rows = Some(
            page.rows
                .into_iter()
                .map(|(key, value)| {
                    let mut buf = [0; VALUE_BYTES];
                    let mut output = Cursor::new(buf.as_mut_slice());
                    let value = if serde_json::to_writer(&mut output, &value).is_ok() {
                        let length = output.position() as usize;
                        str::from_utf8(&buf[..length])
                            .expect("JSON is UTF-8")
                            .to_owned()
                    } else {
                        "[Value too large to preview]".into()
                    };
                    (
                        if key.len() <= VALUE_BYTES {
                            key
                        } else {
                            "[Key too large to preview]".into()
                        },
                        value,
                    )
                })
                .collect(),
        );
    }
    Ok(Some(Preview {
        name: settings
            .get_as::<&str>("name")
            .unwrap_or(&link.name)
            .to_owned(),
        root: db.root_id().clone(),
        stores,
        selected,
        rows,
        more,
    }))
}
