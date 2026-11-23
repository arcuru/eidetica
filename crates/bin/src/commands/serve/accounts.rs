//! Read-only navigation anchored to the signed-in Account's named references.

use std::{fmt::Write, io::Cursor};

use axum::{extract::RawQuery, http::header};
use eidetica::{
    Database, Result, Snapshot,
    account::{Account, IdentityStatus},
    store::{Registered, Table},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::form_urlencoded;

use super::{
    AccountSession, AppState, Cookies, Html, ID, IntoResponse, PublicKey, Redirect, Response,
    SESSION_COOKIE, State, StatusCode,
};
use crate::templates::{COMMON_STYLES, html_escape as escape};

const ROUTE: &str = "/dashboard/accounts";
const UNAVAILABLE: &str = "Target inaccessible or unavailable.";
// A URL can be replayed or edited. Bound the number of authorized hops it can request.
const MAX_DEPTH: usize = 12;
const MAX_QUERY: usize = 16 * 1024;
const ROW_LIMIT: usize = 10;
const VALUE_BYTES: usize = 2048;

#[derive(Serialize, Deserialize)]
struct Selection {
    name: String,
    root: ID,
    key: PublicKey,
}

#[derive(Clone, Serialize, Deserialize)]
struct Link {
    name: String,
    root: ID,
}

#[derive(Default)]
struct Location {
    identity: Option<Selection>,
    path: Vec<Link>,
    namespace: Option<String>,
    data: Option<Link>,
    store: Option<String>,
}

impl Location {
    fn parse(query: &str) -> Option<Self> {
        if query.len() > MAX_QUERY {
            return None;
        }
        let mut location = Self::default();
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "identity" if location.identity.is_none() => {
                    location.identity = Some(serde_json::from_str(&value).ok()?);
                }
                "path" if location.path.is_empty() => {
                    location.path = serde_json::from_str(&value).ok()?;
                    if location.path.is_empty() || location.path.len() > MAX_DEPTH {
                        return None;
                    }
                }
                "namespace" if location.namespace.is_none() => {
                    location.namespace = Some(value.into_owned())
                }
                "data" if location.data.is_none() => {
                    location.data = Some(serde_json::from_str(&value).ok()?)
                }
                "store" if location.store.is_none() => location.store = Some(value.into_owned()),
                _ => return None,
            }
        }
        if (!location.path.is_empty() && location.identity.is_none())
            || (location.path.is_empty() && location.namespace.is_some())
            || (location.data.is_some()
                && !location.namespace.as_deref().is_some_and(data_namespace))
            || (location.store.is_some() && location.data.is_none())
        {
            return None;
        }
        Some(location)
    }

    fn url(
        &self,
        path: &[Link],
        namespace: Option<&str>,
        data: Option<&Link>,
        store: Option<&str>,
    ) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        if let Some(identity) = &self.identity {
            query.append_pair(
                "identity",
                &serde_json::to_string(identity).expect("serializable Identity selection"),
            );
        }
        if !path.is_empty() {
            query.append_pair(
                "path",
                &serde_json::to_string(path).expect("serializable Account path"),
            );
        }
        if let Some(namespace) = namespace {
            query.append_pair("namespace", namespace);
        }
        if let Some(data) = data {
            query.append_pair(
                "data",
                &serde_json::to_string(data).expect("serializable data link"),
            );
        }
        if let Some(store) = store {
            query.append_pair("store", store);
        }
        let query = query.finish();
        if query.is_empty() {
            ROUTE.to_owned()
        } else {
            format!("{ROUTE}?{query}")
        }
    }

    fn back(&self) -> String {
        if self.data.is_some() {
            self.url(&self.path, self.namespace.as_deref(), None, None)
        } else {
            self.url(
                &self.path[..self.path.len().saturating_sub(1)],
                None,
                None,
                None,
            )
        }
    }
}

fn data_namespace(namespace: &str) -> bool {
    !matches!(namespace, "accounts" | "identities")
}

pub(super) async fn browse(
    State(state): State<AppState>,
    cookies: Cookies,
    RawQuery(query): RawQuery,
) -> Response {
    let Some(cookie) = cookies.get(SESSION_COOKIE) else {
        return Redirect::to("/login").into_response();
    };
    let Some(user) = state.sessions.get_user(cookie.value()).await else {
        return Redirect::to("/login").into_response();
    };
    let user = user.read().await;
    let location = Location::parse(query.as_deref().unwrap_or_default());
    let content = match &location {
        Some(location) => content(&state, &user, location).await,
        None => None,
    };
    let (status, content) = match content {
        Some(content) => (StatusCode::OK, content),
        None => (
            StatusCode::NOT_FOUND,
            format!("<p class=\"error\">{UNAVAILABLE}</p>"),
        ),
    };
    let back = location
        .filter(|l| !l.path.is_empty())
        .map_or_else(String::new, |l| {
            format!(r#" · <a href="{}">Back</a>"#, escape(&l.back()))
        });
    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
        <title>Eidetica - Linked Accounts</title><style>{COMMON_STYLES}</style></head>
        <body><main class="container"><h1>Linked Accounts</h1>
        <p><a href="/dashboard">Dashboard</a> · <a href="{ROUTE}">Account list</a>{back}</p>
        <p><strong>Signed in as:</strong> {}</p>{content}</main></body></html>"#,
        escape(user.username()),
    );
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(html),
    )
        .into_response()
}

async fn content(state: &AppState, user: &AccountSession, location: &Location) -> Option<String> {
    let identity = match &location.identity {
        Some(selection) => {
            // Pure lookup; neither install a key nor update the session's selected member.
            let identity = user.get_identity(&selection.name).await.ok()??;
            if identity.root_id() != &selection.root || identity.key_id() != &selection.key {
                return None;
            }
            Some(identity)
        }
        None => None,
    };
    let mut html = match &identity {
        Some(identity) => format!(
            "<p><strong>Acting Identity:</strong> {} <code>{}</code><br>Member key: <code>{}</code></p>",
            escape(identity.name()),
            identity.root_id(),
            identity.key_id(),
        ),
        None => "<p><strong>Acting Identity:</strong> none selected</p>".to_owned(),
    };

    let mut opened: Option<Account> = None;
    let mut seen = vec![user.account().id()];
    for link in &location.path {
        let parent = opened.as_ref().unwrap_or_else(|| user.account());
        // Resolve only the selected edge. Re-check its expected root before opening it.
        if seen.contains(&&link.root)
            || !matches_reference(parent, "accounts", link).await
            || !browsable_root(state, &link.root).await
        {
            return None;
        }
        opened = Some(identity.as_ref()?.open_account(&link.root).await.ok()?);
        seen.push(&link.root);
    }
    let account = opened.as_ref().unwrap_or_else(|| user.account());
    let viewed = if opened.is_some() {
        account.get_name().await.ok()?
    } else {
        "Your Account".to_owned()
    };
    let _ = write!(
        html,
        "<p><strong>Viewed Account:</strong> {} <code>{}</code></p>",
        escape(&viewed),
        account.id()
    );

    if let Some(link) = &location.data {
        let identity = identity.as_ref()?;
        let namespace = location.namespace.as_deref()?;
        if seen.contains(&&link.root)
            || !matches_reference(account, namespace, link).await
            || !browsable_root(state, &link.root).await
            || identity.root_id() == &link.root
        {
            return None;
        }
        let database = identity.open_database(&link.root).await.ok()?;
        html.push_str(
            &data_preview(&database, location, namespace, link)
                .await
                .ok()??,
        );
        return Some(html);
    }

    if location.identity.is_none() {
        html.push_str(&identity_form(user).await);
    }
    html.push_str(
        "<h2>Account references</h2><p>A reference names a target. It does not grant access.</p>",
    );
    html.push_str(&references(account, "accounts", location, false).await);

    if opened.is_some() {
        html.push_str("<h2>Data references</h2>");
        match account.reference_namespaces().await {
            Ok(namespaces) => {
                let mut namespaces: Vec<_> = namespaces
                    .into_iter()
                    .filter(|name| data_namespace(name))
                    .collect();
                namespaces.sort();
                if namespaces.is_empty() {
                    html.push_str("<p>No data references.</p>");
                } else {
                    let selected = location.namespace.as_deref().unwrap_or(&namespaces[0]);
                    if !namespaces.iter().any(|ns| ns == selected) {
                        return None;
                    }
                    html.push_str("<ul aria-label=\"Data reference namespaces\">");
                    for namespace in &namespaces {
                        let _ = write!(
                            html,
                            r#"<li><a href="{}">{}</a></li>"#,
                            escape(&location.url(&location.path, Some(namespace), None, None)),
                            escape(namespace)
                        );
                    }
                    let _ = write!(html, "</ul><h3>{}</h3>", escape(selected));
                    html.push_str(&references(account, selected, location, true).await);
                }
            }
            Err(_) => html.push_str("<p>Data references unavailable.</p>"),
        }
    }
    Some(html)
}

async fn matches_reference(account: &Account, namespace: &str, link: &Link) -> bool {
    matches!(account.reference(namespace, &link.name).await, Ok(Some(root)) if root == link.root)
}

async fn browsable_root(state: &AppState, root: &ID) -> bool {
    // Instance administration does not make its internal stores part of this browser.
    matches!(state.instance.backend().get_instance_metadata().await, Ok(Some(meta))
        if &meta.users_db != root && &meta.databases_db != root && meta.sync_db.as_ref() != Some(root))
}

async fn identity_form(user: &AccountSession) -> String {
    let Ok(mut identities) = user.identities().await else {
        return "<p>Acting Identity choices unavailable.</p>".to_owned();
    };
    identities.retain(|(_, tracked)| tracked.status == IdentityStatus::Active);
    identities.sort_by(|(a, _), (b, _)| a.cmp(b));
    if identities.is_empty() {
        return "<p>No locally active Identity is available for browsing.</p>".to_owned();
    }
    let mut html = format!(
        r#"<form method="GET" action="{ROUTE}">
        <label for="identity">Choose an owner Identity</label>
        <select id="identity" name="identity" required>
        <option value="" selected disabled>Select an Identity</option>"#
    );
    for (name, tracked) in identities {
        let selection = Selection {
            name,
            root: tracked.root_id,
            key: tracked.key_id,
        };
        let value = serde_json::to_string(&selection).expect("serializable Identity selection");
        let _ = write!(
            html,
            r#"<option value="{}">{} ({})</option>"#,
            escape(&value),
            escape(&selection.name),
            selection.root
        );
    }
    html.push_str("</select> <button type=\"submit\">Use Identity</button></form>");
    html
}

async fn references(account: &Account, namespace: &str, location: &Location, data: bool) -> String {
    let Ok(mut references) = account.references(namespace).await else {
        return "<p>References unavailable.</p>".to_owned();
    };
    if references.is_empty() {
        return "<p>No references.</p>".to_owned();
    }
    references.sort_by(|(a, _), (b, _)| a.cmp(b));
    let mut html = String::from("<ul>");
    for (name, root) in references {
        let label = escape(&name);
        let Some(root) = root else {
            let _ = write!(html, "<li>{label} — malformed reference</li>");
            continue;
        };
        let link = Link { name, root };
        let url = if location.identity.is_none() || (!data && location.path.len() >= MAX_DEPTH) {
            None
        } else if data {
            Some(location.url(&location.path, Some(namespace), Some(&link), None))
        } else {
            let mut path = location.path.clone();
            path.push(link.clone());
            Some(location.url(&path, None, None, None))
        };
        match url {
            Some(url) if url.len() <= MAX_QUERY => {
                let _ = write!(
                    html,
                    r#"<li><a href="{}">{label}</a> <code>{}</code></li>"#,
                    escape(&url),
                    link.root
                );
            }
            _ => {
                let _ = write!(html, "<li>{label} <code>{}</code></li>", link.root);
            }
        }
    }
    if !data && location.path.len() == MAX_DEPTH {
        html.push_str(
            "</ul><p>Account navigation limit reached; go back to browse another branch.</p>",
        );
    } else {
        html.push_str("</ul>");
    }
    html
}

async fn data_preview(
    database: &Database,
    location: &Location,
    namespace: &str,
    link: &Link,
) -> Result<Option<String>> {
    let tx = database.new_transaction().await?;
    database.current_permission().await?;
    let settings = tx.get_settings()?.get_all().await?;
    // Legacy personal Accounts stored credentials in ordinary Tables. Check the
    // root's original kind too: editing current settings cannot opt these in.
    let genesis = Snapshot::from(vec![database.root_id().clone()]);
    let original = database
        .new_transaction_at(&genesis)
        .await?
        .get_settings()?
        .get_all()
        .await?;
    if [&settings, &original].into_iter().any(|settings| {
        matches!(
            settings.get_as::<&str>("type"),
            Some("system" | "user" | "sync_settings")
        )
    }) {
        return Ok(None);
    }
    let name = settings.get_as::<&str>("name").unwrap_or(&link.name);
    let mut html = format!(
        "<h2>Data: {}</h2><p>Database <code>{}</code></p>",
        escape(name),
        database.root_id()
    );
    let index = tx.get_index().await?;
    let mut stores: Vec<_> = index
        .list()
        .await?
        .into_iter()
        .filter(|name| !name.starts_with('_') && !name.starts_with("refs:"))
        .collect();
    stores.sort();
    let selected = match location.store.as_ref() {
        Some(store) if stores.contains(store) => Some(store),
        Some(_) => return Ok(None),
        None => stores.first(),
    };
    html.push_str("<p>Preview supports unencrypted Table stores only. Choose a store:</p><ul>");
    for store in &stores {
        let _ = write!(
            html,
            r#"<li><a href="{}">{}</a></li>"#,
            escape(&location.url(&location.path, Some(namespace), Some(link), Some(store))),
            escape(store)
        );
    }
    html.push_str("</ul>");
    let Some(store) = selected else {
        html.push_str("<p>No application stores available for preview.</p>");
        return Ok(Some(html));
    };
    let _ = write!(html, "<h3>{}</h3>", escape(store));
    if !Table::<Value>::supports_type_id(&index.get_entry(store).await?.type_id) {
        html.push_str("<p>This store type does not have a preview.</p>");
        return Ok(Some(html));
    }
    let page = tx
        .get_store::<Table<Value>>(store)
        .await?
        .scan_page(None, ROW_LIMIT)
        .await?;
    if page.rows.is_empty() {
        html.push_str("<p>This Table is empty.</p>");
    } else {
        html.push_str("<table><thead><tr><th scope=\"col\">Key</th><th scope=\"col\">Value</th></tr></thead><tbody>");
        for (key, value) in page.rows {
            let mut buf = [0; VALUE_BYTES];
            let mut output = Cursor::new(buf.as_mut_slice());
            let rendered = if serde_json::to_writer(&mut output, &value).is_ok() {
                let length = output.position() as usize;
                str::from_utf8(&buf[..length]).expect("JSON is UTF-8")
            } else {
                "[Value too large to preview]"
            };
            let key = if key.len() <= VALUE_BYTES {
                &key
            } else {
                "[Key too large to preview]"
            };
            let _ = write!(
                html,
                "<tr><td><code>{}</code></td><td><code>{}</code></td></tr>",
                escape(key),
                escape(rendered)
            );
        }
        html.push_str("</tbody></table>");
        if page.next.is_some() {
            let _ = write!(html, "<p>Showing the first {ROW_LIMIT} rows only.</p>");
        }
    }
    Ok(Some(html))
}
