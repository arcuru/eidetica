//! Read-only navigation anchored to the signed-in Account's named references.

use std::fmt::Write;

use axum::{extract::RawQuery, http::header};
use eidetica::service::dashboard::browser::{
    self, Content, Link, Location, MAX_DEPTH, MAX_QUERY, Page, Preview, ROW_LIMIT, Selection,
};
use url::form_urlencoded;

use super::{
    AppState, Cookies, Html, ID, IntoResponse, Redirect, Response, SESSION_COOKIE, State,
    StatusCode,
};
use crate::templates::{COMMON_STYLES, html_escape as escape};

const ROUTE: &str = "/dashboard/accounts";
const UNAVAILABLE: &str = "Target inaccessible or unavailable.";
trait BrowserLocation: Sized {
    fn parse(query: &str) -> Option<Self>;
    fn url(
        &self,
        path: &[Link],
        namespace: Option<&str>,
        data: Option<&Link>,
        store: Option<&str>,
    ) -> String;
    fn back(&self) -> String;
}

impl BrowserLocation for Location {
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
        location.valid().then_some(location)
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
    let location = parse_location(query.as_deref());
    let page = match &location {
        Some(location) => browser::embedded(&state.instance, &user, location).await,
        None => None,
    };
    render(user.username(), location, page)
}

pub(crate) fn parse_location(query: Option<&str>) -> Option<Location> {
    Location::parse(query.unwrap_or_default())
}

pub(crate) fn render(username: &str, location: Option<Location>, page: Option<Page>) -> Response {
    let content = location
        .as_ref()
        .zip(page)
        .map(|(loc, page)| content(loc, page));
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
        escape(username),
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

fn content(location: &Location, page: Page) -> String {
    let mut html = match &location.identity {
        Some(identity) => format!(
            "<p><strong>Acting Identity:</strong> {} <code>{}</code><br>Member key: <code>{}</code></p>",
            escape(&identity.name),
            identity.root,
            identity.key,
        ),
        None => "<p><strong>Acting Identity:</strong> none selected</p>".to_owned(),
    };
    let _ = write!(
        html,
        "<p><strong>Viewed Account:</strong> {} <code>{}</code></p>",
        escape(&page.viewed_name),
        page.viewed_root
    );
    match page.content {
        Content::Preview(preview) => {
            html.push_str(&data_preview(preview, location));
        }
        Content::References {
            identities,
            accounts,
            data,
        } => {
            if let Some(identities) = identities {
                html.push_str(&identity_form(identities));
            }
            html.push_str("<h2>Account references</h2><p>A reference names a target. It does not grant access.</p>");
            html.push_str(&references(accounts, "accounts", location, false));
            if let Some(data) = data {
                html.push_str("<h2>Data references</h2>");
                match data {
                    Ok(data) => {
                        if let Some((selected, rows)) = data.selected {
                            html.push_str("<ul aria-label=\"Data reference namespaces\">");
                            for namespace in &data.namespaces {
                                let _ = write!(
                                    html,
                                    r#"<li><a href="{}">{}</a></li>"#,
                                    escape(&location.url(
                                        &location.path,
                                        Some(namespace),
                                        None,
                                        None
                                    )),
                                    escape(namespace)
                                );
                            }
                            let _ = write!(html, "</ul><h3>{}</h3>", escape(&selected));
                            html.push_str(&references(rows, &selected, location, true));
                        } else {
                            html.push_str("<p>No data references.</p>");
                        }
                    }
                    Err(()) => html.push_str("<p>Data references unavailable.</p>"),
                }
            }
        }
    }
    html
}

fn identity_form(identities: Result<Vec<Selection>, ()>) -> String {
    let Ok(mut identities) = identities else {
        return "<p>Acting Identity choices unavailable.</p>".to_owned();
    };
    identities.sort_by(|a, b| a.name.cmp(&b.name));
    if identities.is_empty() {
        return "<p>No locally active Identity is available for browsing.</p>".to_owned();
    }
    let mut html = format!(
        r#"<form method="GET" action="{ROUTE}">
        <label for="identity">Choose an owner Identity</label>
        <select id="identity" name="identity" required>
        <option value="" selected disabled>Select an Identity</option>"#
    );
    for selection in identities {
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

fn references(
    rows: Result<Vec<(String, Option<ID>)>, ()>,
    namespace: &str,
    location: &Location,
    data: bool,
) -> String {
    let Ok(mut references) = rows else {
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

fn data_preview(preview: Preview, location: &Location) -> String {
    let mut html = format!(
        "<h2>Data: {}</h2><p>Database <code>{}</code></p>",
        escape(&preview.name),
        preview.root
    );
    html.push_str("<p>Preview supports unencrypted Table stores only. Choose a store:</p><ul>");
    for store in &preview.stores {
        let _ = write!(
            html,
            r#"<li><a href="{}">{}</a></li>"#,
            escape(&location.url(
                &location.path,
                location.namespace.as_deref(),
                location.data.as_ref(),
                Some(store)
            )),
            escape(store)
        );
    }
    html.push_str("</ul>");
    let Some(store) = preview.selected else {
        html.push_str("<p>No application stores available for preview.</p>");
        return html;
    };
    let _ = write!(html, "<h3>{}</h3>", escape(&store));
    let Some(rows) = preview.rows else {
        html.push_str("<p>This store type does not have a preview.</p>");
        return html;
    };
    if rows.is_empty() {
        html.push_str("<p>This Table is empty.</p>");
    } else {
        html.push_str("<table><thead><tr><th scope=\"col\">Key</th><th scope=\"col\">Value</th></tr></thead><tbody>");
        for (key, value) in rows {
            let _ = write!(
                html,
                "<tr><td><code>{}</code></td><td><code>{}</code></td></tr>",
                escape(&key),
                escape(&value)
            );
        }
        html.push_str("</tbody></table>");
        if preview.more {
            let _ = write!(html, "<p>Showing the first {ROW_LIMIT} rows only.</p>");
        }
    }
    html
}
