//! Real browser HTTP requests and peer bootstrap exchanges, not mocked grants.
use super::*;
use eidetica::{
    AccountSession, Database, NewAccount,
    auth::{AuthKey, KeyStatus},
    crdt::Doc,
    store::DocStore,
    sync::{DatabaseTicket, RequestStatus, peer_types::Address},
};
use reqwest::{Client, Response as HttpResponse, redirect::Policy};
use tokio::task::JoinHandle;

struct Web {
    url: String,
    state: AppState,
    client: Client,
    task: JoinHandle<()>,
}

impl Drop for Web {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Web {
    async fn start(instance: &Instance) -> Self {
        let sync = instance.sync().unwrap();
        let state = AppState {
            instance: Arc::new(instance.clone()),
            sync_handler: Arc::new(SyncHandlerImpl::new(
                instance.clone(),
                sync.sync_tree_root_id().clone(),
            )),
            sessions: SessionStore::new(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = dashboard_router(state.clone(), true);
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            url,
            state,
            task,
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }

    async fn login(&self, username: &str) -> String {
        let response = self.post("/login", None, &[("username", username)]).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn post(&self, path: &str, cookie: Option<&str>, form: &[(&str, &str)]) -> HttpResponse {
        let mut request = self
            .client
            .post(format!("{}{path}", self.url))
            .header("Origin", &self.url);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.form(form).send().await.unwrap()
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> HttpResponse {
        let mut request = self.client.get(format!("{}{path}", self.url));
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.send().await.unwrap()
    }

    async fn track(&self, cookie: &str, ticket: &DatabaseTicket, permission: &str) -> HttpResponse {
        self.post(
            "/dashboard/track",
            Some(cookie),
            &[("ticket", &ticket.to_string()), ("permission", permission)],
        )
        .await
    }

    async fn decide(
        &self,
        cookie: Option<&str>,
        db: &ID,
        id: &str,
        decision: &str,
    ) -> HttpResponse {
        self.post(
            "/dashboard/requests/decide",
            cookie,
            &[
                ("database_id", &db.to_string()),
                ("request_id", id),
                ("decision", decision),
            ],
        )
        .await
    }

    async fn user(&self, cookie: &str) -> Arc<tokio::sync::RwLock<AccountSession>> {
        self.state
            .sessions
            .get_user(cookie.split_once('=').unwrap().1)
            .await
            .unwrap()
    }
}

async fn instance(name: &str) -> (Instance, AccountSession) {
    let (instance, user) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless(name))
            .await
            .unwrap();
    instance.enable_sync().await.unwrap();
    instance
        .sync()
        .unwrap()
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    (instance, user)
}

struct Sharing {
    instance: Instance,
    owner: AccountSession,
    key: PublicKey,
    db: Database,
    web: Web,
    ticket: DatabaseTicket,
    cookie: String,
}

impl Sharing {
    async fn start() -> Self {
        let (instance, mut owner) = instance("owner").await;
        // Exercise selection of a non-default, database-specific key.
        let key = owner.add_private_key(Some("database admin")).await.unwrap();
        let db = owner.create_database(Doc::new(), &key).await.unwrap();
        let tx = db.new_transaction().await.unwrap();
        tx.get_settings()
            .unwrap()
            .set_auth_key(
                &instance.id(),
                AuthKey::active(Some("sync device"), Permission::Admin(0)),
            )
            .await
            .unwrap();
        tx.get_store::<DocStore>("messages")
            .await
            .unwrap()
            .set_string("hello", "from owner")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        owner
            .track_database(db.root_id().clone(), &key, SyncSettings::enabled())
            .await
            .unwrap();
        let web = Web::start(&instance).await;
        let cookie = web.login("owner").await;
        let ticket = DatabaseTicket::with_addresses(
            db.root_id().clone(),
            vec![Address::http(web.url.trim_start_matches("http://"))],
        );
        Self {
            instance,
            owner,
            key,
            db,
            web,
            ticket,
            cookie,
        }
    }

    async fn pending(&self) -> String {
        let pending = self
            .instance
            .sync()
            .unwrap()
            .pending_bootstrap_requests()
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        pending[0].0.clone()
    }
}

#[tokio::test]
async fn access_http_approval_is_scoped_and_grants_only_requested_access() {
    let peer = Sharing::start().await;
    let (client_instance, _) = instance("requester").await;
    let client = Web::start(&client_instance).await;
    let requester = client.login("requester").await;
    let requester_lock = client.user(&requester).await;
    let requester_key = requester_lock.read().await.get_default_key().unwrap();
    let before = requester_lock.read().await.databases().await.unwrap();
    let pending = client.track(&requester, &peer.ticket, "write").await;
    assert_eq!(pending.status(), StatusCode::ACCEPTED);
    let body = pending.text().await.unwrap();
    assert!(body.contains("Access pending approval"));
    assert!(body.contains("Retry request"));
    let request_id = peer.pending().await;
    assert!(body.contains(&request_id));
    assert_eq!(
        requester_lock.read().await.databases().await.unwrap(),
        before
    );
    assert!(
        requester_lock
            .read()
            .await
            .key_mapping(&requester_key, peer.db.root_id())
            .unwrap()
            .is_none()
    );
    // Retry the pending HTTP submission: one stable request, no local writes.
    assert_eq!(
        client
            .track(&requester, &peer.ticket, "write")
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(peer.pending().await, request_id);

    let page = peer
        .web
        .get("/dashboard/requests", Some(&peer.cookie))
        .await;
    assert_eq!(page.status(), StatusCode::OK);
    let page = page.text().await.unwrap();
    for text in [
        &request_id,
        &peer.db.root_id().to_string(),
        &requester_key.to_string(),
        "Write(10)",
        "Pending approval",
        "not an independently verified person",
    ] {
        assert!(page.contains(text), "missing {text}: {page}");
    }
    assert!(
        peer.web
            .get("/dashboard", Some(&peer.cookie))
            .await
            .text()
            .await
            .unwrap()
            .contains("/dashboard/requests")
    );

    // Another instance administrator has no authority over this database.
    peer.owner
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::passwordless("outsider"))
        .await
        .unwrap();
    let outsider_user = peer.instance.login_account("outsider", None).await.unwrap();
    peer.owner
        .admin()
        .await
        .unwrap()
        .grant_instance_admin(&outsider_user.get_default_key().unwrap())
        .await
        .unwrap();
    let outsider = peer.web.login("outsider").await;
    let page = peer
        .web
        .get("/dashboard/requests", Some(&outsider))
        .await
        .text()
        .await
        .unwrap();
    for forbidden in [
        &request_id,
        &requester_key.to_string(),
        &peer.db.root_id().to_string(),
    ] {
        assert!(!page.contains(forbidden));
    }
    for decision in ["approve", "reject"] {
        assert_eq!(
            peer.web
                .decide(Some(&outsider), peer.db.root_id(), &request_id, decision)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            peer.web
                .decide(None, peer.db.root_id(), &request_id, decision)
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        for origin in [None, Some("http://invalid.example")] {
            let mut request = peer
                .web
                .client
                .post(format!("{}/dashboard/requests/decide", peer.web.url))
                .header("Cookie", &peer.cookie);
            if let Some(origin) = origin {
                request = request.header("Origin", origin);
            }
            let response = request
                .form(&[
                    ("database_id", peer.db.root_id().to_string()),
                    ("request_id", request_id.clone()),
                    ("decision", decision.to_string()),
                ])
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    assert_eq!(
        peer.web.get("/dashboard/requests", None).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        peer.web
            .get("/dashboard/requests", Some("eidetica_session=forged"))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    // A valid but unrelated database owned by this session cannot substitute for the target.
    let mut owner = peer.instance.login_account("owner", None).await.unwrap();
    let other = owner.create_database(Doc::new(), &peer.key).await.unwrap();
    let cookie = peer.web.login("owner").await;
    assert_eq!(
        peer.web
            .decide(Some(&cookie), other.root_id(), &request_id, "reject")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        peer.web
            .decide(Some(&cookie), peer.db.root_id(), "forged", "approve")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        peer.web
            .decide(Some(&cookie), peer.db.root_id(), &request_id, "automatic")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(peer.pending().await, request_id);

    let approved = peer
        .web
        .decide(Some(&cookie), peer.db.root_id(), &request_id, "approve")
        .await;
    assert_eq!(approved.status(), StatusCode::OK);
    assert!(
        approved
            .text()
            .await
            .unwrap()
            .contains("requester must explicitly retry")
    );
    assert!(matches!(
        peer.instance
            .sync()
            .unwrap()
            .get_bootstrap_request(&request_id)
            .await
            .unwrap()
            .unwrap()
            .1
            .status,
        RequestStatus::Approved { .. }
    ));
    for decision in ["approve", "reject"] {
        assert_eq!(
            peer.web
                .decide(Some(&cookie), peer.db.root_id(), &request_id, decision)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    // Native API acknowledgement retries stay idempotent.
    let tips = peer.db.snapshot().await.unwrap();
    peer.owner
        .approve_bootstrap_request(&peer.instance.sync().unwrap(), &request_id, &peer.key)
        .await
        .unwrap();
    assert_eq!(peer.db.snapshot().await.unwrap(), tips);
    assert_eq!(
        client
            .track(&requester, &peer.ticket, "write")
            .await
            .status(),
        StatusCode::OK
    );
    let user = requester_lock.read().await;
    let opened = user.open_database(peer.db.root_id()).await.unwrap();
    assert_eq!(
        opened.current_permission().await.unwrap(),
        Permission::Write(10)
    );
    assert_eq!(
        opened
            .get_store_viewer::<DocStore>("messages")
            .await
            .unwrap()
            .get_string("hello")
            .await
            .unwrap(),
        "from owner"
    );
    let tx = opened.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("messages")
        .await
        .unwrap()
        .set_string("reply", "from requester")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(!opened.current_permission().await.unwrap().can_admin());
}

#[tokio::test]
async fn access_http_rejection_is_terminal_and_does_not_record_access() {
    let peer = Sharing::start().await;
    let (client_instance, _) = instance("requester").await;
    let client = Web::start(&client_instance).await;
    let cookie = client.login("requester").await;
    let user = client.user(&cookie).await;
    let key = user.read().await.get_default_key().unwrap();
    let before = user.read().await.databases().await.unwrap();
    assert_eq!(
        client.track(&cookie, &peer.ticket, "read").await.status(),
        StatusCode::ACCEPTED
    );
    let id = peer.pending().await;
    let rejected = peer
        .web
        .decide(Some(&peer.cookie), peer.db.root_id(), &id, "reject")
        .await;
    assert_eq!(rejected.status(), StatusCode::OK);
    assert!(rejected.text().await.unwrap().contains("Request rejected"));
    for decision in ["approve", "reject"] {
        assert_eq!(
            peer.web
                .decide(Some(&peer.cookie), peer.db.root_id(), &id, decision)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    peer.owner
        .reject_bootstrap_request(&peer.instance.sync().unwrap(), &id, &peer.key)
        .await
        .unwrap();
    let response = client.track(&cookie, &peer.ticket, "read").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response.text().await.unwrap();
    assert!(body.contains("Access request rejected"));
    assert!(!body.contains("Retry request"));
    assert_eq!(user.read().await.databases().await.unwrap(), before);
    assert!(
        user.read()
            .await
            .key_mapping(&key, peer.db.root_id())
            .unwrap()
            .is_none()
    );
    assert!(
        !Database::can_access(&peer.instance, peer.db.root_id(), &key, &Permission::Read)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn access_http_non_admin_and_revoked_admin_cannot_inspect_or_decide() {
    let peer = Sharing::start().await;
    let (requester, mut user) = instance("requester").await;
    let key = user.get_default_key().unwrap();
    let error = user
        .request_database_access(
            &requester.sync().unwrap(),
            &peer.ticket,
            &key,
            Permission::Read,
            SyncSettings::disabled(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, eidetica::Error::Sync(e) if matches!(*e, SyncError::BootstrapPending { .. }))
    );
    let id = peer.pending().await;
    // Downgrading the account-held key removes GUI management rights even while
    // the instance device key remains Admin for peer operations.
    let tx = peer.db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(&peer.key, AuthKey::active(Some("reader"), Permission::Read))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let page = peer
        .web
        .get("/dashboard/requests", Some(&peer.cookie))
        .await
        .text()
        .await
        .unwrap();
    assert!(!page.contains(&id));
    assert!(!page.contains(&key.to_string()));
    for decision in ["approve", "reject"] {
        assert_eq!(
            peer.web
                .decide(Some(&peer.cookie), peer.db.root_id(), &id, decision)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(peer.pending().await, id);
    let device_db = peer
        .db
        .clone()
        .with_key(peer.instance.signing_key().unwrap().clone());
    let tx = device_db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &peer.key,
            AuthKey::new(Some("revoked"), Permission::Admin(0), KeyStatus::Revoked),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        !peer
            .web
            .get("/dashboard/requests", Some(&peer.cookie))
            .await
            .text()
            .await
            .unwrap()
            .contains(&id)
    );
    assert_eq!(
        peer.web
            .decide(Some(&peer.cookie), peer.db.root_id(), &id, "approve")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn access_http_network_failure_is_not_a_grant() {
    let (instance, _) = instance("requester").await;
    let web = Web::start(&instance).await;
    let cookie = web.login("requester").await;
    let user = web.user(&cookie).await;
    let before = user.read().await.databases().await.unwrap();
    let ticket = DatabaseTicket::with_addresses(
        ID::from_bytes("missing"),
        vec![Address::http("127.0.0.1:0")],
    );
    let response = web.track(&cookie, &ticket, "read").await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Access not confirmed")
    );
    assert_eq!(user.read().await.databases().await.unwrap(), before);
}

#[tokio::test]
async fn access_http_priority_escalation_is_refused_by_native_auth() {
    let peer = Sharing::start().await;
    let tx = peer.db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &peer.key,
            AuthKey::active(Some("limited admin"), Permission::Admin(5)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (client_instance, _) = instance("requester").await;
    let client = Web::start(&client_instance).await;
    let cookie = client.login("requester").await;
    let user = client.user(&cookie).await;
    let key = user.read().await.get_default_key().unwrap();
    assert_eq!(
        client.track(&cookie, &peer.ticket, "admin").await.status(),
        StatusCode::ACCEPTED
    );
    let id = peer.pending().await;
    assert!(
        peer.web
            .get("/dashboard/requests", Some(&peer.cookie))
            .await
            .text()
            .await
            .unwrap()
            .contains("Admin(0)")
    );
    let response = peer
        .web
        .decide(Some(&peer.cookie), peer.db.root_id(), &id, "approve")
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Decision not confirmed")
    );
    assert!(
        !Database::can_access(&peer.instance, peer.db.root_id(), &key, &Permission::Read)
            .await
            .unwrap()
    );
    assert_eq!(peer.pending().await, id);
    // Rejecting an ungrantable request still uses the existing Admin check.
    assert_eq!(
        peer.web
            .decide(Some(&peer.cookie), peer.db.root_id(), &id, "reject")
            .await
            .status(),
        StatusCode::OK
    );
}

/// Pauses one peer-bound bootstrap to deterministically exercise changes during
/// the GUI handler's network wait. All proof checks and grants are still real.
#[derive(Clone)]
struct Gate {
    handler: Arc<dyn SyncHandler>,
    pause_next: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

async fn gated_sync(
    State(gate): State<Gate>,
    ExtractJson(request): ExtractJson<SyncRequest>,
) -> axum::Json<SyncResponse> {
    if matches!(&request, SyncRequest::SyncTree(request) if request.requesting_key.is_some())
        && gate
            .pause_next
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        gate.entered.notify_one();
        tokio::time::timeout(Duration::from_secs(5), gate.resume.notified())
            .await
            .unwrap();
    }
    axum::Json(
        gate.handler
            .handle_request(&request, &RequestContext::default())
            .await,
    )
}

async fn gated_peer(peer: &Sharing) -> (Gate, DatabaseTicket, JoinHandle<()>) {
    let gate = Gate {
        handler: peer.web.state.sync_handler.clone(),
        pause_next: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        entered: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route("/api/v0", post(gated_sync))
        .with_state(gate.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        gate,
        DatabaseTicket::with_addresses(peer.db.root_id().clone(), vec![Address::http(address)]),
        task,
    )
}

#[tokio::test]
async fn access_http_network_wait_releases_lock_and_preserves_changed_preferences() {
    let peer = Sharing::start().await;
    let (client_instance, _) = instance("requester").await;
    let client = Web::start(&client_instance).await;
    let cookie = client.login("requester").await;
    let user = client.user(&cookie).await;
    let key = user.read().await.get_default_key().unwrap();
    let tx = peer.db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &key,
            AuthKey::active(Some("requester"), Permission::Write(10)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        client.track(&cookie, &peer.ticket, "write").await.status(),
        StatusCode::OK
    );
    let (gate, ticket, proxy_task) = gated_peer(&peer).await;
    let request = client.track(&cookie, &ticket, "write");
    let changed = SyncSettings::disabled().with_interval(123);
    let edit_during_network = async {
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let mut session = user
            .try_write()
            .expect("network wait held the per-session lock");
        session
            .track_database(peer.db.root_id().clone(), &key, changed.clone())
            .await
            .unwrap();
        gate.resume.notify_one();
    };
    let (response, ()) = tokio::join!(request, edit_during_network);
    proxy_task.abort();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        user.read()
            .await
            .database(peer.db.root_id())
            .await
            .unwrap()
            .sync_settings,
        changed
    );
}

#[tokio::test]
async fn access_http_local_bookkeeping_failure_does_not_claim_usable_access() {
    let peer = Sharing::start().await;
    let (client_instance, _) = instance("requester").await;
    let client = Web::start(&client_instance).await;
    let cookie = client.login("requester").await;
    let user = client.user(&cookie).await;
    let key = user.read().await.get_default_key().unwrap();
    let before = user.read().await.databases().await.unwrap();
    let tx = peer.db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &key,
            AuthKey::active(Some("requester"), Permission::Write(10)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Test-only fault injection: reduce Account write authority while the peer
    // is serving the join. Production code never opens the Account backing DB.
    let account = {
        let session = user.read().await;
        Database::open(&client_instance, session.account().id())
            .await
            .unwrap()
            .with_key(session.get_signing_key(&key).unwrap())
    };
    let (gate, ticket, proxy_task) = gated_peer(&peer).await;
    let request = client.track(&cookie, &ticket, "write");
    let fail_during_network = async {
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let tx = account.new_transaction().await.unwrap();
        tx.get_settings()
            .unwrap()
            .set_auth_key(&key, AuthKey::active(Some("read only"), Permission::Read))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        gate.resume.notify_one();
    };
    let (response, ()) = tokio::join!(request, fail_during_network);
    proxy_task.abort();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("Access transferred; local bookkeeping not confirmed"),
        "{body}"
    );
    assert!(body.contains("Retry request"));
    assert!(!body.contains("Access granted and recorded locally"));
    let session = user.read().await;
    assert_eq!(session.databases().await.unwrap(), before);
    assert!(
        session
            .key_mapping(&key, peer.db.root_id())
            .unwrap()
            .is_none()
    );
    assert!(
        Database::can_access(
            &peer.instance,
            peer.db.root_id(),
            &key,
            &Permission::Write(10)
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn access_http_forged_bootstrap_proof_cannot_enqueue_a_request() {
    use eidetica::{
        Clock, Snapshot, SystemClock,
        auth::crypto::PrivateKey,
        sync::protocol::{SyncRequestAuth, SyncTreeRequest},
    };
    let peer = Sharing::start().await;
    let claimed = PrivateKey::generate();
    let impostor = PrivateKey::generate();
    for auth in [
        None,
        Some(SyncRequestAuth::sign(
            &impostor,
            &peer.instance.id(),
            peer.db.root_id(),
            &Snapshot::EMPTY,
            SystemClock.now_millis(),
        )),
    ] {
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: peer.db.root_id().clone(),
            our_tips: Snapshot::EMPTY,
            peer_pubkey: None,
            requesting_key: Some(claimed.public_key()),
            requesting_key_name: Some("forged".to_string()),
            requested_permission: Some(Permission::Admin(0)),
            metadata: None,
            auth,
        });
        let response: SyncResponse = peer
            .web
            .client
            .post(format!("{}/api/v0", peer.web.url))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(matches!(response, SyncResponse::Error(_)), "{response:?}");
    }
    assert!(
        peer.instance
            .sync()
            .unwrap()
            .pending_bootstrap_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !Database::can_access(
            &peer.instance,
            peer.db.root_id(),
            &claimed.public_key(),
            &Permission::Read
        )
        .await
        .unwrap()
    );
}
