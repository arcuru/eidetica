//! Shared real-HTTP fixture for dashboard tests.
use super::*;
use eidetica::{AccountSession, NewAccount};
use reqwest::{Client, Response as HttpResponse, redirect::Policy};
use tokio::task::JoinHandle;

pub(super) struct Web {
    pub(super) url: String,
    pub(super) state: AppState,
    pub(super) client: Client,
    task: JoinHandle<()>,
}

impl Drop for Web {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Web {
    pub(super) async fn start(instance: &Instance) -> Self {
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

    pub(super) async fn login(&self, username: &str) -> String {
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

    pub(super) async fn post(
        &self,
        path: &str,
        cookie: Option<&str>,
        form: &[(&str, &str)],
    ) -> HttpResponse {
        let mut request = self
            .client
            .post(format!("{}{path}", self.url))
            .header("Origin", &self.url);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.form(form).send().await.unwrap()
    }

    pub(super) async fn get(&self, path: &str, cookie: Option<&str>) -> HttpResponse {
        let mut request = self.client.get(format!("{}{path}", self.url));
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.send().await.unwrap()
    }

    pub(super) async fn track(
        &self,
        cookie: &str,
        ticket: &DatabaseTicket,
        permission: &str,
    ) -> HttpResponse {
        self.post(
            "/dashboard/track",
            Some(cookie),
            &[("ticket", &ticket.to_string()), ("permission", permission)],
        )
        .await
    }

    pub(super) async fn decide(
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

    pub(super) async fn user(&self, cookie: &str) -> Arc<tokio::sync::RwLock<AccountSession>> {
        self.state
            .sessions
            .get_user(cookie.split_once('=').unwrap().1)
            .await
            .unwrap()
    }
}

pub(super) async fn instance(name: &str) -> (Instance, AccountSession) {
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
