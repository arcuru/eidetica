//! Daemon command - runs the Eidetica local service (Unix socket).
//!
//! Two entry points:
//!
//! - [`run_init`] — `eidetica daemon init` — initialises a fresh instance with
//!   an explicit initial admin user. Fails if the backend is already
//!   initialised.
//! - [`run`] — `eidetica daemon` (no subcommand) — runs the daemon against an
//!   already-initialised backend. Fails with a pointer to `daemon init` if
//!   the backend is empty.

use std::{sync::Arc, time::Duration};

use axum::{
    extract::State,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::RwLock,
    task::JoinHandle,
};
use tracing_subscriber::EnvFilter;

use eidetica::Instance;
use eidetica::NewAccount;
use eidetica::instance::InstanceError;
use eidetica::service::ServiceServer;
use eidetica::service::default_socket_path;
use eidetica::sync::DatabaseTicket;
use eidetica::sync::transports::http::HttpTransport;
use eidetica::sync::transports::iroh::IrohTransport;

use crate::backend::{create_backend, open_instance};
use crate::cli::{BackendConfig, DaemonArgs, DaemonInitArgs};

/// Run the Eidetica daemon against an already-initialised backend.
///
/// Errors with a pointer to `eidetica daemon init` if the backend hasn't been
/// initialised yet (i.e. `Instance::open_backend` returns
/// [`InstanceError::NotInitialized`]).
pub async fn run(args: &DaemonArgs) -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    // Create backend
    let backend = create_backend(&args.backend_config).await?;

    // Initialize Instance — load only. Map NotInitialized to a friendly
    // pointer at `daemon init`.
    let instance = match open_instance(&args.backend_config, backend).await {
        Ok(instance) => instance,
        Err(e) => {
            if let eidetica::Error::Instance(boxed) = &e
                && matches!(boxed.as_ref(), InstanceError::NotInitialized)
            {
                if args.backend_config.device_key_dir.is_some() {
                    return Err("Trial database has no instance metadata. Run `eidetica daemon init` only for a fresh disposable pair. If initialisation was interrupted, preserve both stores for inspection.".into());
                }
                return Err(format!(
                    "Backend at {} is not initialised.\nRun `eidetica daemon init --username <NAME> [--password PASS | --passwordless]` first.",
                    crate::backend::backend_label(&args.backend_config)
                )
                .into());
            }
            return Err(Box::new(e));
        }
    };
    tracing::info!("Instance initialized (device ID: {})", instance.id());

    // Determine socket path
    let socket_path = args.socket.clone().unwrap_or_else(default_socket_path);
    let server = ServiceServer::bind(instance.clone(), &socket_path).await?;

    // Reserve the address before announcing readiness; no second backend is opened.
    // A bind failure drops the socket owner.
    let web_listener = if args.dashboard {
        Some(
            tokio::net::TcpListener::bind((args.dashboard_host.as_str(), args.dashboard_port))
                .await?,
        )
    } else {
        None
    };
    let shared_http_sync = web_listener.as_ref().is_some_and(|listener| {
        // Compare the configured port, including zero, rather than the allocated
        // ephemeral port. The bound IP also handles dashboard host resolution.
        args.http_sync_bind
            == Some(std::net::SocketAddr::new(
                listener.local_addr().expect("bound listener").ip(),
                args.dashboard_port,
            ))
    });

    instance.enable_sync().await?;
    let sync = instance.sync().ok_or("Sync not enabled on instance")?;
    let startup: eidetica::Result<String> = async {
        sync.register_transport("iroh", IrohTransport::builder())
            .await?;
        let mut http = HttpTransport::builder();
        if let Some(bind) = args.http_sync_bind
            && !shared_http_sync
        {
            http = http.bind(bind.to_string());
        }
        sync.register_transport("http", http).await?;
        // Start each required listener explicitly: accept_connections() allows
        // partial success, which would hide a failed HTTP bind behind Iroh.
        sync.accept_connections_on("iroh").await?;
        if args.http_sync_bind.is_some() && !shared_http_sync {
            sync.accept_connections_on("http").await?;
            println!(
                "HTTP sync listening on http://{}",
                sync.get_server_address_for("http").await?
            );
        }
        for ticket in &args.sync_tickets {
            sync.sync_with_ticket(&ticket.parse::<DatabaseTicket>()?)
                .await?;
        }
        sync.get_server_address_for("iroh").await
    }
    .await;
    let address = match startup {
        Ok(address) => address,
        Err(error) => {
            let _ = sync.stop_server().await;
            return Err(Box::new(error));
        }
    };
    tracing::info!(%address, "Daemon sync listener started");

    let dashboard_open = Arc::new(RwLock::new(true));
    let web = web_listener.map(|listener| {
        let addr = listener.local_addr().expect("bound listener");
        println!("Dashboard listening on http://{addr}");
        if shared_http_sync {
            println!("HTTP sync listening on http://{addr} (shared dashboard listener)");
        }
        let app = super::serve::web_router(instance.clone(), shared_http_sync).route_layer(
            middleware::from_fn_with_state(dashboard_open.clone(), dashboard_admission),
        );
        (listener, app)
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

    println!("Eidetica daemon listening on {}", socket_path.display());
    println!(
        "  Backend: {}",
        crate::backend::backend_label(&args.backend_config)
    );
    println!();
    println!("Connect with:");
    println!("  Instance::connect(\"unix://{}\")", socket_path.display());
    println!();
    println!("Press Ctrl+C to shutdown");

    let mut server =
        tokio::spawn(async move { server.run(shutdown_rx).await.map_err(|e| e.to_string()) });
    let (web_shutdown_tx, web_shutdown) = tokio::sync::watch::channel(());
    let mut web_server = tokio::spawn(async move {
        if let Some((listener, app)) = web {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let mut rx = web_shutdown;
                let _ = rx.changed().await;
            })
            .await
            .map_err(|e| e.to_string())
        } else {
            let mut rx = web_shutdown;
            let _ = rx.changed().await;
            Ok(())
        }
    });

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to set up SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to set up SIGINT handler");

    let stopped = tokio::select! {
        result = &mut server => Some(("service", result)),
        result = &mut web_server => Some(("dashboard", result)),
        _ = sigterm.recv() => None,
        _ = sigint.recv() => None,
    };
    drop(shutdown_tx);
    let drain = Duration::from_secs(10);
    // Stop browser writes and wait for admitted requests before flushing. The
    // shared /api/v0 route bypasses this gate and stays served during the flush.
    let dashboard_result = close_dashboard(&dashboard_open, drain).await;
    if !shared_http_sync {
        let _ = web_shutdown_tx.send(());
    }
    let (service_result, web_result) = tokio::join!(
        async {
            if stopped.as_ref().is_some_and(|(name, _)| *name == "service") {
                Ok(())
            } else {
                drain_listener(&mut server, "service", drain).await
            }
        },
        async {
            if shared_http_sync
                || stopped
                    .as_ref()
                    .is_some_and(|(name, _)| *name == "dashboard")
            {
                Ok(())
            } else {
                drain_listener(&mut web_server, "dashboard", drain).await
            }
        }
    );

    // ServiceServer owns socket and client teardown. Once it has stopped, no
    // new local writes can race the final flush. Keep the sync listener open
    // through that flush, then close its endpoint even if either operation
    // reports an error.
    let flush_timeout = Duration::from_secs(10);
    tracing::info!("Flushing daemon sync before closing sync listeners");
    let sync_flush = if dashboard_result.is_err() {
        Err(eidetica::sync::SyncError::Network(
            "Dashboard writes still active during shutdown".into(),
        )
        .into())
    } else {
        match tokio::time::timeout(flush_timeout, instance.flush_sync()).await {
            Ok(result) => result,
            Err(_) => Err(eidetica::sync::SyncError::Network(
                "Timed out flushing daemon sync during shutdown".into(),
            )
            .into()),
        }
    };
    drop(web_shutdown_tx);
    let shared_web_result = if shared_http_sync
        && !stopped
            .as_ref()
            .is_some_and(|(name, _)| *name == "dashboard")
    {
        drain_listener(&mut web_server, "dashboard", drain).await
    } else {
        Ok(())
    };
    let sync_stop = sync.stop_server().await;
    service_result?;
    web_result?;
    shared_web_result?;
    dashboard_result?;
    if let Some((name, result)) = stopped {
        result
            .map_err(|e| format!("{name} listener task failed: {e}"))?
            .map_err(|e| format!("{name} listener failed: {e}"))?;
        return Err(format!("{name} listener stopped unexpectedly").into());
    }
    sync_flush?;
    sync_stop?;

    println!("Daemon shut down");
    Ok(())
}

async fn close_dashboard(open: &RwLock<bool>, drain: Duration) -> Result<(), String> {
    tokio::time::timeout(drain, async {
        *open.write().await = false;
    })
    .await
    .map_err(|_| "dashboard requests did not drain before sync flush".to_string())
}

/// Hold a read guard until a browser handler finishes, so shutdown can quiesce
/// local writes without stopping HTTP peer sync on the same listener.
async fn dashboard_admission(
    State(open): State<Arc<RwLock<bool>>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/api/v0" {
        return next.run(request).await;
    }
    let open = open.read().await;
    if !*open {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    next.run(request).await
}

async fn drain_listener(
    task: &mut JoinHandle<Result<(), String>>,
    name: &str,
    timeout: Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, &mut *task).await {
        Ok(result) => result.map_err(|e| format!("{name} listener task failed: {e}"))?,
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(format!("{name} shutdown timed out"))
        }
    }
}

/// Initialise a fresh daemon instance with an initial admin user.
///
/// Builds the [`NewAccount`] from `--username` + one of `--password` /
/// `--passwordless` / an interactive double-prompt, then calls
/// [`Instance::create_backend`]. Exits after initialisation; the operator runs
/// `eidetica daemon` separately to actually serve the socket.
pub async fn run_init(
    args: &DaemonInitArgs,
    backend_args: &BackendConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    // Resolve the password choice. `--passwordless` and explicit `--password`
    // are mutually exclusive (enforced by clap); otherwise prompt twice.
    let new_account = if args.passwordless {
        NewAccount::passwordless(&args.username)
    } else if let Some(pw) = &args.password {
        NewAccount::with_password(&args.username, pw)
    } else {
        let pw = rpassword::prompt_password(format!(
            "Password for new admin user '{}': ",
            args.username
        ))?;
        let confirm = rpassword::prompt_password("Re-enter password: ")?;
        if pw != confirm {
            return Err("Passwords did not match.".into());
        }
        NewAccount::with_password(&args.username, pw)
    };

    let backend = create_backend(backend_args).await?;
    let initialized = match backend_args.device_key_dir.as_deref() {
        Some(directory) => {
            Instance::create_backend_with_device_key_dir(backend, new_account, directory).await
        }
        None => Instance::create_backend(backend, new_account).await,
    };
    let (instance, _user) = match initialized {
        Ok(pair) => pair,
        Err(e) => {
            if let eidetica::Error::Instance(boxed) = &e
                && matches!(boxed.as_ref(), InstanceError::InstanceAlreadyExists)
            {
                return Err(format!(
                    "Backend at {} is already initialised.\nUse `eidetica daemon` (no subcommand) to run the existing instance.",
                    crate::backend::backend_label(backend_args)
                )
                .into());
            }
            return Err(Box::new(e));
        }
    };

    println!(
        "Eidetica instance initialised on {}",
        crate::backend::backend_label(backend_args)
    );
    println!("  Device ID:    {}", instance.id());
    println!("  Initial user: {}", args.username);
    println!();
    if backend_args.device_key_dir.is_some() {
        println!("Start `eidetica daemon` with the same --device-key-dir.");
    } else {
        println!("Start the daemon with: `eidetica daemon`");
    }

    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .try_init();
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;
    use crate::cli::Backend;

    fn daemon_args(data_dir: &std::path::Path, socket: &std::path::Path) -> DaemonArgs {
        DaemonArgs {
            command: None,
            sync_tickets: Vec::new(),
            http_sync_bind: None,
            socket: Some(socket.to_path_buf()),
            dashboard: false,
            dashboard_host: "127.0.0.1".into(),
            dashboard_port: 5942,
            backend_config: BackendConfig {
                cache_dir: None,
                cache_backend: None,
                backend: Backend::Inmemory,
                data_dir: Some(data_dir.to_path_buf()),
                postgres_url: None,
                device_key_dir: None,
                cache_bytes: None,
                cache_namespaces: 1024,
            },
        }
    }

    #[tokio::test]
    async fn daemon_bind_error_returns_within_a_bound() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let socket_parent = dir.path().join("not-a-directory");
        let socket_path = socket_parent.join("daemon.sock");
        tokio::fs::write(&socket_parent, b"not a directory")
            .await
            .unwrap();
        let backend = create_backend(&BackendConfig {
            cache_dir: None,
            cache_backend: None,
            backend: Backend::Inmemory,
            data_dir: Some(data_dir.clone()),
            postgres_url: None,
            device_key_dir: None,
            cache_bytes: None,
            cache_namespaces: 1024,
        })
        .await
        .unwrap();
        Instance::create_backend(backend, NewAccount::passwordless("admin"))
            .await
            .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run(&daemon_args(&data_dir, &socket_path)),
        )
        .await
        .expect("daemon startup failure must not hang");

        assert!(result.is_err());
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn dashboard_bind_failure_releases_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("service.sock");
        let backend = create_backend(&BackendConfig {
            cache_dir: None,
            cache_backend: None,
            backend: Backend::Inmemory,
            data_dir: Some(dir.path().into()),
            postgres_url: None,
            device_key_dir: None,
            cache_bytes: None,
            cache_namespaces: 1024,
        })
        .await
        .unwrap();
        Instance::create_backend(backend, NewAccount::passwordless("admin"))
            .await
            .unwrap();
        let occupied = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut args = daemon_args(dir.path(), &socket);
        args.dashboard = true;
        args.dashboard_port = occupied.local_addr().unwrap().port();
        let result = tokio::time::timeout(Duration::from_secs(2), run(&args))
            .await
            .unwrap();
        assert!(result.is_err());
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn dashboard_drain_timeout_keeps_admission_closed() {
        let (instance, _) = Instance::create_backend(
            Box::new(eidetica::backend::database::InMemory::new()),
            NewAccount::passwordless("admin"),
        )
        .await
        .unwrap();
        instance.enable_sync().await.unwrap();
        let open = Arc::new(RwLock::new(true));
        let admitted = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let app = {
            let admitted = admitted.clone();
            let release = release.clone();
            super::super::serve::web_router(instance, true)
                .route(
                    "/write",
                    axum::routing::post(move || {
                        let admitted = admitted.clone();
                        let release = release.clone();
                        async move {
                            admitted.notify_one();
                            release.acquire().await.unwrap().forget();
                            StatusCode::OK
                        }
                    }),
                )
                .route_layer(middleware::from_fn_with_state(
                    open.clone(),
                    dashboard_admission,
                ))
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            let http = reqwest::Client::new();
            let browser = {
                let http = http.clone();
                let url = url.clone();
                tokio::spawn(async move { http.post(format!("{url}/write")).send().await.unwrap() })
            };
            admitted.notified().await;
            assert!(
                close_dashboard(&open, Duration::from_millis(50))
                    .await
                    .is_err(),
                "dashboard admission must cover the handler's lifetime"
            );
            assert_eq!(
                http.post(format!("{url}/login"))
                    .header("Origin", &url)
                    .form(&[("username", "admin")])
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "a drain timeout must not reopen dashboard admission"
            );
            let response = http
                .post(format!("{url}/api/v0"))
                .json(&eidetica::sync::protocol::SyncRequest::SendEntries(
                    Vec::new(),
                ))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(matches!(
                response
                    .json::<eidetica::sync::protocol::SyncResponse>()
                    .await
                    .unwrap(),
                eidetica::sync::protocol::SyncResponse::Ack
            ));
            release.add_permits(1);
            assert_eq!(browser.await.unwrap().status(), StatusCode::OK);
            assert_eq!(
                http.post(format!("{url}/login"))
                    .header("Origin", &url)
                    .form(&[("username", "admin")])
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
            assert_eq!(
                http.post(format!("{url}/api/v0"))
                    .json(&eidetica::sync::protocol::SyncRequest::SendEntries(
                        Vec::new()
                    ))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
            shutdown.send(()).unwrap();
            task.await.unwrap();
        })
        .await
        .expect("shared listener drain must remain bounded");
    }

    #[tokio::test]
    async fn http_sync_bind_failure_releases_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("service.sock");
        let backend = create_backend(&BackendConfig {
            backend: Backend::Inmemory,
            data_dir: Some(dir.path().into()),
            postgres_url: None,
            cache_dir: None,
            cache_backend: None,
            device_key_dir: None,
            cache_bytes: None,
            cache_namespaces: 1024,
        })
        .await
        .unwrap();
        Instance::create_backend(backend, NewAccount::passwordless("admin"))
            .await
            .unwrap();
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut args = daemon_args(dir.path(), &socket);
        args.http_sync_bind = Some(occupied.local_addr().unwrap());
        let result = tokio::time::timeout(Duration::from_secs(5), run(&args))
            .await
            .unwrap();
        assert!(
            result.is_err(),
            "HTTP bind failure cannot be hidden by Iroh startup"
        );
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn concurrent_daemon_is_rejected_without_disturbing_owner() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let socket_parent = dir.path().join("runtime");
        tokio::fs::create_dir(&socket_parent).await.unwrap();
        tokio::fs::set_permissions(&socket_parent, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let socket_path = socket_parent.join("daemon.sock");
        let backend = create_backend(&BackendConfig {
            cache_dir: None,
            cache_backend: None,
            backend: Backend::Inmemory,
            data_dir: Some(data_dir.clone()),
            postgres_url: None,
            device_key_dir: None,
            cache_bytes: None,
            cache_namespaces: 1024,
        })
        .await
        .unwrap();
        let (instance, _admin) =
            Instance::create_backend(backend, NewAccount::passwordless("admin"))
                .await
                .unwrap();
        let owner = ServiceServer::bind(instance.clone(), &socket_path)
            .await
            .unwrap();
        let expected_id = instance.id();
        let (shutdown, rx) = tokio::sync::watch::channel(());
        let owner_task = tokio::spawn(owner.run(rx));

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run(&daemon_args(&data_dir, &socket_path)),
        )
        .await
        .expect("concurrent daemon startup must be bounded");

        assert!(result.is_err());
        let client = tokio::time::timeout(
            Duration::from_secs(1),
            Instance::connect(format!("unix://{}", socket_path.display())),
        )
        .await
        .expect("the original daemon must answer within a bound")
        .expect("the original daemon must still serve protocol requests");
        assert_eq!(client.id(), expected_id);
        drop(client);
        drop(shutdown);
        owner_task.await.unwrap().unwrap();
        assert!(!socket_path.exists());
    }
}
