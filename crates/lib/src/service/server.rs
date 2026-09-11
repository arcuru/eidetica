//! Service server: accepts Unix socket connections and dispatches `BackendImpl` operations.
//!
//! The server wraps an `Instance` (not just a backend) so it can handle write
//! notifications through the Instance's callback system.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UnixListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::Instance;
use crate::auth::crypto::{PublicKey, generate_challenge, verify_challenge_response};
use crate::auth::errors::AuthError;
use crate::auth::types::{Permission, SigKey};
use crate::auth::validation::permissions::resolve_identity_permission;
use crate::backend::{
    CacheScope, StagingToken, StoreStateLifecycle, StoreStateRequest, VerificationStatus,
};
use crate::database::Database;
use crate::entry::ID;
use crate::instance::{CallbackId, WriteSource};
use crate::service::error::ServiceError;
use crate::service::protocol::{
    AuthenticatedDbRequest, DatabaseOp, HandshakeAck, MergeState, Notification, PROTOCOL_VERSION,
    ServerFrame, ServiceRequest, ServiceResponse, read_frame, write_frame,
};
use crate::user::system_databases::lookup_user_record;

/// Connection identifier. Monotonic per server-run; reused only after
/// `AtomicU64` wraps (effectively never on a real daemon). Purely
/// diagnostic now — registry-based routing went away in the per-db
/// callback refactor.
type ConnectionId = u64;

/// How long an idle session token survives before the next dispatch reclaims
/// its backend resources.
const TOKEN_IDLE_TTL: Duration = Duration::from_secs(5 * 60);

/// One server-issued Store-state capability, bound to the session that minted
/// it. `published` distinguishes a readable view from a staging namespace that
/// still owes an abort if the session goes away.
struct SessionStaging {
    backend: StagingToken,
    user_uuid: String,
    database: ID,
    published: bool,
    chunks: HashMap<u64, Vec<u8>>,
    last_used: Instant,
}

/// Per-connection context carried through the dispatch chain. Holds:
///
/// - `tx`: the writer-channel sender — both `ServerFrame::Response`s from
///   the dispatcher and `ServerFrame::Notification`s from subscribed
///   per-db callbacks ride through this same channel, so the order
///   observed by the client is the order frames hit `frame_tx`.
/// - `instance`: needed in `Drop` to call `remove_write_callback` for
///   the connection's registered subscriptions.
/// - `subscribed`: `root_id -> CallbackId` for every `SubscribeWrites`
///   this connection has done. Cleaned up on disconnect (via the guard's
///   `Drop`) and on explicit `UnsubscribeWrites`.
///
/// There is no per-connection registry on the server. Each subscription
/// is just a per-db callback registered against the daemon's Instance
/// (`Instance::register_write_callback`); the daemon's existing
/// `fire_write_callbacks` dispatch path handles fan-out by walking the
/// per-tree callback list, no separate fan-out mechanism required.
struct ConnectionContext {
    conn_id: ConnectionId,
    tx: mpsc::UnboundedSender<ServerFrame>,
    instance: Instance,
    subscribed: std::sync::Mutex<HashMap<ID, CallbackId>>,
    staging: std::sync::Mutex<HashMap<String, SessionStaging>>,
    token_idle_ttl: Duration,
}

impl ConnectionContext {
    fn subscribed_lock(&self) -> std::sync::MutexGuard<'_, HashMap<ID, CallbackId>> {
        self.subscribed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop session tokens that have gone idle, returning the backend staging
    /// tokens whose namespaces the caller must abort.
    fn cleanup_expired(&self) -> Vec<StagingToken> {
        let now = Instant::now();
        let mut expired = Vec::new();
        self.staging.lock().unwrap().retain(|_, value| {
            let keep = now.duration_since(value.last_used) <= self.token_idle_ttl;
            if !keep && !value.published {
                expired.push(value.backend.clone());
            }
            keep
        });
        expired
    }
}

/// RAII cleanup: on connection drop (clean EOF, error, or panic), call
/// `Instance::remove_write_callback` for every subscription this
/// connection registered. Holds an `Arc<ConnectionContext>` rather than
/// borrowing so the cleanup path is identical for every exit case
/// without sprinkling `remove_*` calls through the connection handler.
struct ConnectionGuard {
    ctx: Arc<ConnectionContext>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let subs = self.ctx.subscribed_lock();
        if !subs.is_empty() {
            tracing::debug!(
                conn_id = self.ctx.conn_id,
                "Unregistering {} subscriptions on disconnect",
                subs.len()
            );
        }
        for (tree_id, id) in subs.iter() {
            self.ctx.instance.remove_write_callback(tree_id, *id);
        }
        drop(subs);
        let staging = std::mem::take(&mut *self.ctx.staging.lock().unwrap())
            .into_values()
            .filter(|value| !value.published)
            .map(|value| value.backend)
            .collect::<Vec<_>>();
        if !staging.is_empty() {
            let backend = self.ctx.instance.backend().clone();
            tokio::spawn(async move {
                for token in staging {
                    let _ = backend.abort_store_state(token).await;
                }
            });
        }
    }
}

/// Per-connection authentication state.
///
/// A connection moves `PreAuth → AwaitingProof → Authenticated` on a successful
/// `TrustedLoginUser` / `TrustedLoginProve` exchange. A failed proof drops the
/// connection back to `PreAuth` so the client can retry without reconnecting.
/// Any other request while in `AwaitingProof` also resets the state so a
/// half-finished login can't be exploited mid-flight.
///
/// "Trusted" refers to the assumption that whoever can reach this socket is
/// already authorised by filesystem permissions (mode 0600 under
/// `$XDG_RUNTIME_DIR`); see the protocol module docs and the Service
/// Architecture brain doc § Trusted login threat model.
#[derive(Debug, Clone)]
enum ConnectionState {
    /// No login attempt yet, or last attempt failed/abandoned.
    PreAuth,
    /// `TrustedLoginUser` succeeded; waiting for the client's `TrustedLoginProve`.
    AwaitingProof {
        username: String,
        user_uuid: String,
        challenge: Vec<u8>,
        expected_pubkey: PublicKey,
    },
    /// Login completed. `login_pubkey` is the verified root pubkey for the
    /// user, established at `TrustedLoginProve` time. `session_keyset` is the
    /// set of pubkeys the client has further proven possession of via
    /// `SessionKeyChallenge`/`SessionKeyRegister`; it always contains
    /// `login_pubkey` and may include additional per-DB keys the user owns.
    /// The dispatch path for `Authenticated`/`AuthenticatedDb` requests
    /// validates the identity hint against this set and gates against the
    /// resulting *acting* pubkey, so a single connection can drive ops on
    /// databases authored by any key the user has proven they hold.
    /// `user_uuid` is the per-user scope key for the unified CRDT-state
    /// cache (see [`crate::backend::CacheScope::User`]).
    /// `pending_key_challenges` holds outstanding registration challenges,
    /// keyed by the pubkey the challenge was issued for. Each challenge is
    /// single-use: the matching `SessionKeyRegister` consumes it whether
    /// verification succeeds or fails.
    #[allow(dead_code)] // username surfaces in audit/logging follow-ups
    Authenticated {
        username: String,
        user_uuid: String,
        login_pubkey: PublicKey,
        session_keyset: HashSet<PublicKey>,
        pending_key_challenges: HashMap<PublicKey, Vec<u8>>,
    },
}

/// Eidetica service server that listens on a Unix domain socket.
///
/// The server wraps a full `Instance` so it can dispatch both storage operations
/// (via the backend) and write callbacks (via `Instance::put_entry()`'s notification path).
///
/// CRDT-state caching lives in the underlying `BackendImpl` (scope-keyed
/// via [`crate::backend::CacheScope`]); wire handlers route through
/// `instance.backend()` directly rather than keeping a separate
/// service-layer cache.
pub struct ServiceServer {
    instance: Instance,
    socket_path: PathBuf,
    listener: UnixListener,
    socket_identity: SocketIdentity,
    token_idle_ttl: Duration,
    _lock_file: File,
    parent: ParentGuard,
}

#[derive(Clone, Copy)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn matches(self, metadata: &std::fs::Metadata) -> bool {
        self.device == metadata.dev() && self.inode == metadata.ino()
    }
}

impl ServiceServer {
    /// Bind the service socket and return a server ready to accept clients.
    ///
    /// # Arguments
    /// * `instance` - The Instance to serve. The server holds a strong reference.
    /// * `socket_path` - Path for the Unix domain socket.
    pub async fn bind(instance: Instance, socket_path: impl Into<PathBuf>) -> crate::Result<Self> {
        Self::bind_inner(instance, socket_path.into(), TOKEN_IDLE_TTL, &mut |_| {}).await
    }

    /// Bind with a shorter session-token lifetime for expiry tests.
    #[cfg(feature = "testing")]
    pub async fn bind_with_token_idle_ttl_for_test(
        instance: Instance,
        socket_path: impl Into<PathBuf>,
        ttl: Duration,
    ) -> crate::Result<Self> {
        Self::bind_inner(instance, socket_path.into(), ttl, &mut |_| {}).await
    }

    #[cfg(test)]
    async fn bind_with_test_hook(
        instance: Instance,
        socket_path: impl Into<PathBuf>,
        mut hook: impl FnMut(BindTestStage),
    ) -> crate::Result<Self> {
        Self::bind_inner(instance, socket_path.into(), TOKEN_IDLE_TTL, &mut hook).await
    }

    async fn bind_inner(
        instance: Instance,
        socket_path: PathBuf,
        token_idle_ttl: Duration,
        hook: &mut impl FnMut(BindTestStage),
    ) -> crate::Result<Self> {
        let socket_path = resolve_socket_path(socket_path)?;
        let parent = prepare_parent(&socket_path).await?;
        hook(BindTestStage::AfterParentSetup);
        parent.ensure_current()?;
        let lock_file = acquire_endpoint_lock(&socket_path, hook)?;
        parent.ensure_current()?;
        recover_stale_socket(&socket_path, &parent, hook).await?;

        parent.ensure_current()?;
        let listener = UnixListener::bind(&socket_path)?;
        let socket_identity = prepare_bound_socket(&listener, &socket_path, &parent, hook)?;

        Ok(Self {
            instance,
            socket_path,
            listener,
            socket_identity,
            token_idle_ttl,
            _lock_file: lock_file,
            parent,
        })
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Accept client connections until the shutdown signal is received.
    ///
    /// # Arguments
    /// * `shutdown` - A watch receiver; the server stops when the sender is dropped.
    pub async fn run(self, mut shutdown: watch::Receiver<()>) -> crate::Result<()> {
        tracing::info!("Service server listening on {}", self.socket_path.display());

        // Diagnostic per-connection counter — purely for logging. No
        // registry-based routing needed; each connection's subscriptions
        // live as per-db callbacks on the Instance, dispatched by the
        // existing `fire_write_callbacks` path.
        let next_conn_id = Arc::new(AtomicU64::new(1));

        // Track active per-connection tasks so shutdown actually
        // disconnects them. Without this the shutdown signal only stops
        // the accept loop; live handlers keep running until the client
        // closes its end, which means "shutdown" is half-done — clients
        // hang on response reads from a daemon they were told had stopped.
        let mut handlers = JoinSet::new();

        let result = loop {
            tokio::select! {
                accept_result = self.listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let instance = self.instance.clone();
                            let token_idle_ttl = self.token_idle_ttl;
                            let conn_id = next_conn_id.fetch_add(1, Ordering::Relaxed);
                            handlers.spawn(async move {
                                if let Err(e) = handle_connection(stream, instance, conn_id, token_idle_ttl).await {
                                    tracing::debug!(conn_id, "Connection handler error: {e}");
                                }
                            });
                        }
                        Err(e) => break Err(e.into()),
                    }
                }
                // Reap finished handlers as they complete. Without this arm
                // the JoinSet only drains at shutdown, so every connection the
                // daemon has ever served keeps its task entry alive for the
                // daemon's lifetime.
                _ = handlers.join_next(), if !handlers.is_empty() => {}
                _ = shutdown.changed() => {
                    tracing::info!("Service server shutting down");
                    break Ok(());
                }
            }
        };

        // Abort live handlers. Each aborted task drops its `UnixStream`
        // halves, which the client observes as a clean EOF on its read
        // side — the reader task exits, sets `closed`, and any in-flight
        // or subsequent `request()` surfaces `ConnectionAborted`.
        handlers.abort_all();
        // Drain to completion so the tempdir-owned socket file isn't
        // pulled out from under any handler that hasn't yet observed
        // the abort.
        while handlers.join_next().await.is_some() {}

        result
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindTestStage {
    AfterParentSetup,
    AfterLock,
    AfterStaleProbe,
    BeforeSocketChmod,
}

const PARENT_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;
const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

struct ParentGuard {
    path: PathBuf,
    identity: SocketIdentity,
    _directory: File,
}

impl ParentGuard {
    fn ensure_current(&self) -> crate::Result<()> {
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if !metadata.file_type().is_dir()
            || !self.identity.matches(&metadata)
            || metadata.uid() != effective_uid()
            || metadata.permissions().mode() & 0o777 != PARENT_MODE
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "service endpoint parent {} changed or is not owner-only",
                    self.path.display()
                ),
            )
            .into());
        }
        Ok(())
    }
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn resolve_socket_path(path: PathBuf) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let file_name = absolute.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("service endpoint {} has no file name", absolute.display()),
        )
    })?;
    let parent = absolute.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("service endpoint {} has no parent", absolute.display()),
        )
    })?;

    match std::fs::canonicalize(parent) {
        Ok(parent) => Ok(parent.join(file_name)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let invalid_parent = || {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("service endpoint parent {} is invalid", parent.display()),
                )
            };
            let parent_name = parent.file_name().ok_or_else(invalid_parent)?;
            let ancestor = parent.parent().ok_or_else(invalid_parent)?;
            Ok(std::fs::canonicalize(ancestor)?
                .join(parent_name)
                .join(file_name))
        }
        Err(error) => Err(error),
    }
}

async fn prepare_parent(socket_path: &Path) -> crate::Result<ParentGuard> {
    let parent = socket_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "service endpoint {} has no parent directory",
                    socket_path.display()
                ),
            )
        })?;

    match std::fs::symlink_metadata(parent) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let ancestor = parent.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "service endpoint parent {} has no existing ancestor",
                        parent.display()
                    ),
                )
            })?;
            validate_existing_ancestors(ancestor)?;
            let mut builder = tokio::fs::DirBuilder::new();
            builder.mode(PARENT_MODE);
            match builder.create(parent).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    validate_existing_ancestors(parent)?;

    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o777 != PARENT_MODE
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing unsafe service endpoint parent {} (must be owned by the effective user with mode 0700)",
                parent.display()
            ),
        )
        .into());
    }

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    let guard = ParentGuard {
        path: parent.to_path_buf(),
        identity: SocketIdentity::from_metadata(&directory.metadata()?),
        _directory: directory,
    };
    guard.ensure_current()?;
    Ok(guard)
}

fn validate_existing_ancestors(parent: &Path) -> crate::Result<()> {
    for ancestor in parent.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!(
                    "service endpoint ancestor {} is not a real directory",
                    ancestor.display()
                ),
            )
            .into());
        }
        let mode = metadata.permissions().mode();
        if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "service endpoint ancestor {} is writable by another user",
                    ancestor.display()
                ),
            )
            .into());
        }
    }
    Ok(())
}

fn lock_path(socket_path: &Path) -> PathBuf {
    let mut path = socket_path.as_os_str().to_owned();
    path.push(".lock");
    PathBuf::from(path)
}

fn acquire_endpoint_lock(
    socket_path: &Path,
    hook: &mut impl FnMut(BindTestStage),
) -> crate::Result<File> {
    let path = lock_path(socket_path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(SOCKET_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.uid() != effective_uid() || metadata.nlink() != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing unsafe service endpoint lockfile {}",
                path.display()
            ),
        )
        .into());
    }
    if metadata.permissions().mode() & 0o777 != SOCKET_MODE {
        file.set_permissions(std::fs::Permissions::from_mode(SOCKET_MODE))?;
    }
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "service endpoint {} is already owned",
                socket_path.display()
            ),
        ),
        TryLockError::Error(error) => error,
    })?;
    hook(BindTestStage::AfterLock);
    let locked = SocketIdentity::from_metadata(&file.metadata()?);
    let current = std::fs::symlink_metadata(&path)?;
    if !current.file_type().is_file() || !locked.matches(&current) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "service endpoint lockfile {} changed while acquiring it",
                path.display()
            ),
        )
        .into());
    }
    Ok(file)
}

async fn recover_stale_socket(
    socket_path: &Path,
    parent: &ParentGuard,
    hook: &mut impl FnMut(BindTestStage),
) -> crate::Result<()> {
    let metadata = match tokio::fs::symlink_metadata(socket_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "refusing to replace non-socket service endpoint {}",
                socket_path.display()
            ),
        )
        .into());
    }

    let probe = tokio::time::timeout(
        STALE_PROBE_TIMEOUT,
        tokio::net::UnixStream::connect(socket_path),
    )
    .await;
    match probe {
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "timed out probing service endpoint {}",
                socket_path.display()
            ),
        )
        .into()),
        Ok(Ok(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("service endpoint {} is live", socket_path.display()),
        )
        .into()),
        Ok(Err(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            hook(BindTestStage::AfterStaleProbe);
            parent.ensure_current()?;
            let current = tokio::fs::symlink_metadata(socket_path).await?;
            if !current.file_type().is_socket()
                || !SocketIdentity::from_metadata(&metadata).matches(&current)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "service endpoint {} changed while recovering it",
                        socket_path.display()
                    ),
                )
                .into());
            }
            tokio::fs::remove_file(socket_path).await?;
            Ok(())
        }
        Ok(Err(error)) => Err(error.into()),
    }
}

fn prepare_bound_socket(
    listener: &UnixListener,
    socket_path: &Path,
    parent: &ParentGuard,
    hook: &mut impl FnMut(BindTestStage),
) -> crate::Result<SocketIdentity> {
    let metadata = std::fs::symlink_metadata(socket_path)?;
    if !metadata.file_type().is_socket() || metadata.uid() != effective_uid() {
        return Err(std::io::Error::other(format!(
            "bound service endpoint is not an owned socket: {}",
            socket_path.display()
        ))
        .into());
    }
    let identity = SocketIdentity::from_metadata(&metadata);
    hook(BindTestStage::BeforeSocketChmod);
    let result = (|| -> crate::Result<()> {
        parent.ensure_current()?;
        let descriptor = fstat_fd(listener.as_fd())?;
        if descriptor.st_mode & libc::S_IFMT != libc::S_IFSOCK
            || descriptor.st_uid != effective_uid()
        {
            return Err(std::io::Error::other(format!(
                "bound service endpoint has incorrect type or owner: {}",
                socket_path.display()
            ))
            .into());
        }
        let current = std::fs::symlink_metadata(socket_path)?;
        if !current.file_type().is_socket()
            || current.uid() != effective_uid()
            || !identity.matches(&current)
        {
            return Err(std::io::Error::other(format!(
                "bound service endpoint changed before permissions were applied: {}",
                socket_path.display()
            ))
            .into());
        }
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
        parent.ensure_current()?;
        let current = std::fs::symlink_metadata(socket_path)?;
        if !current.file_type().is_socket()
            || current.uid() != effective_uid()
            || !identity.matches(&current)
            || current.permissions().mode() & 0o777 != SOCKET_MODE
        {
            return Err(std::io::Error::other(format!(
                "bound service endpoint changed or has incorrect permissions: {}",
                socket_path.display()
            ))
            .into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        remove_socket_if_owned(socket_path, Some(identity), parent);
        return Err(error);
    }
    Ok(identity)
}

fn fstat_fd(fd: std::os::fd::BorrowedFd<'_>) -> std::io::Result<libc::stat> {
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut status) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(status)
}

fn remove_socket_if_owned(
    socket_path: &Path,
    identity: Option<SocketIdentity>,
    parent: &ParentGuard,
) {
    if parent.ensure_current().is_err() {
        return;
    }
    let Ok(metadata) = std::fs::symlink_metadata(socket_path) else {
        return;
    };
    if metadata.file_type().is_socket()
        && identity.is_none_or(|expected| expected.matches(&metadata))
    {
        let _ = std::fs::remove_file(socket_path);
    }
}

impl Drop for ServiceServer {
    fn drop(&mut self) {
        remove_socket_if_owned(&self.socket_path, Some(self.socket_identity), &self.parent);
    }
}

/// Handle a single client connection.
///
/// I/O is split across two tasks:
///
/// - **Writer task** (spawned below) owns the `WriteHalf` and drains an
///   `mpsc::UnboundedReceiver<ServerFrame>` into `write_frame` in
///   submission order. Responses (dispatched from the request loop) and
///   server-pushed notifications (from subscribed per-db callbacks both
///   capture clones of the same `frame_tx`, so a single connection-local
///   order is preserved.
///
/// - **This task** owns the `ReadHalf`, runs the auth/request loop, and
///   sends `ServerFrame::Response(...)` into the channel. It also holds
///   a [`ConnectionGuard`] whose Drop unregisters every subscription this
///   connection made on every exit path (clean EOF, error, or panic).
///
/// Handshake still runs inline on the read side first; the writer task is
/// not started until the handshake succeeds, so a version-mismatch close
/// uses the simpler inline writer path.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    instance: Instance,
    conn_id: ConnectionId,
    token_idle_ttl: Duration,
) -> crate::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);

    // 1. Read and validate handshake (inline writer, no channel yet).
    let handshake: crate::service::protocol::Handshake = match read_frame(&mut reader).await? {
        Some(h) => h,
        None => return Ok(()), // Client disconnected before handshake
    };

    if handshake.protocol_version != PROTOCOL_VERSION {
        // Send error ack and close
        let ack = HandshakeAck {
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut writer, &ack).await?;
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Protocol version mismatch: client={}, server={}",
                handshake.protocol_version, PROTOCOL_VERSION
            ),
        )));
    }

    // Send handshake ack
    let ack = HandshakeAck {
        protocol_version: PROTOCOL_VERSION,
    };
    write_frame(&mut writer, &ack).await?;

    // 2. Spin up the per-connection writer task.
    //
    // TODO(backpressure): this channel is `unbounded`, so a stalled client
    // reader buffers frames in daemon memory without limit. Acceptable
    // under the v1 trust posture (single trusted local client over a Unix
    // socket — an unresponsive reader is treated as a client-side bug, not
    // an attacker). Before serving multiple or untrusted clients this
    // needs a bound + drop policy. Complication: this single channel
    // carries both `Response` and `Notification` frames. Naive bounding
    // is wrong — dropping a `Response` breaks request/response
    // correlation. Two viable shapes for the follow-up:
    //
    //   a) **Split**: separate `response_tx` (bounded, block-send) from
    //      `notification_tx` (bounded, drop-oldest); writer `select!`s
    //      with response priority. Cleanest semantics but touches every
    //      `frame_tx.clone()` site and the cleanup-ordering comment at
    //      the end of this function needs migration.
    //
    //   b) **Variant-aware drop**: bound the single channel; on full,
    //      `try_send` returns `Full(frame)`; reject if Notification,
    //      block if Response. Less plumbing, policy duplicated per send
    //      site.
    //
    // Under the cursor-only `Notification::DatabaseWrite` shape (no entry
    // bodies, just `previous_tips`/`post_tips`), drops are *recoverable*:
    // a subscriber that misses event N still sees event N+1's `post_tips`
    // and calls `ids_added(cursor, post_tips)` to catch up. That makes
    // drop-oldest the right policy and considerably softens the cost of
    // dropping at all — but the work still needs doing for prod.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<ServerFrame>();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            if let Err(e) = write_frame(&mut writer, &frame).await {
                tracing::debug!(conn_id, "Connection writer error: {e}");
                break;
            }
        }
    });

    let ctx = Arc::new(ConnectionContext {
        conn_id,
        tx: frame_tx.clone(),
        instance: instance.clone(),
        subscribed: std::sync::Mutex::new(HashMap::new()),
        staging: std::sync::Mutex::new(HashMap::new()),
        token_idle_ttl,
    });
    let guard = ConnectionGuard { ctx: ctx.clone() };

    // 3. Request/response loop with per-connection auth state. Responses
    //    travel through the writer channel as `ServerFrame::Response`.
    let mut state = ConnectionState::PreAuth;
    let loop_result: crate::Result<()> = async {
        loop {
            let request: ServiceRequest = match read_frame(&mut reader).await? {
                Some(req) => req,
                None => break, // Clean EOF
            };

            let response = dispatch(&instance, &mut state, &ctx, request).await;
            if frame_tx
                .send(ServerFrame::Response(Box::new(response)))
                .is_err()
            {
                // Writer task has exited; nothing more we can send.
                break;
            }
        }
        Ok(())
    }
    .await;

    // Cleanup ordering matters — every clone of `frame_tx` must be
    // dropped before `writer_task.await` can complete (the task is
    // waiting on `frame_rx.recv()`, which only returns `None` when no
    // senders remain):
    //
    // 1. Drop the local `frame_tx` (this fn's own clone).
    // 2. Drop `guard`, which runs `ConnectionGuard::drop` and calls
    //    `Instance::remove_write_callback(tree_id, id)` for every
    //    subscription this connection registered. Each removed callback
    //    Arc drops its captured `tx` clone. (No-op when nothing was
    //    subscribed — e.g. handshake-then-malformed-frame paths.)
    // 3. Drop `ctx` — releases `ctx.tx`, the last sender clone held
    //    on the request-loop side.
    // 4. The writer task's `recv()` returns `None`; the task exits and
    //    `writer_task.await` finishes.
    //
    // Note: a callback dispatch in flight when step 2 runs holds its
    // own Arc to the closure (from `fire_write_callbacks`'s snapshot).
    // It can still send a final notification through its captured `tx`
    // before that Arc is released; the writer task will drain that
    // frame before exiting in step 4.
    drop(frame_tx);
    drop(guard);
    drop(ctx);
    let _ = writer_task.await;

    // Session teardown: subscription cleanup ran via the guard's Drop.
    // The unified CRDT-state cache lives in `BackendImpl` and is bounded
    // by its own eviction policy (byte-bounded LRU on the in-memory
    // backend; disk-bounded on SQL). Per-user cache slots survive
    // disconnect intentionally so a reconnecting client recovers
    // materialized state from tier 2 without recomputing from entries.

    loop_result
}

/// Dispatch a service request to the appropriate Instance/Backend method.
async fn dispatch(
    instance: &Instance,
    state: &mut ConnectionState,
    ctx: &ConnectionContext,
    request: ServiceRequest,
) -> ServiceResponse {
    match dispatch_inner(instance, state, ctx, request).await {
        Ok(resp) => resp,
        Err(e) => ServiceResponse::Error(ServiceError::from(&e)),
    }
}

/// Inner dispatch that returns Result for ergonomic error handling.
async fn dispatch_inner(
    instance: &Instance,
    state: &mut ConnectionState,
    ctx: &ConnectionContext,
    request: ServiceRequest,
) -> crate::Result<ServiceResponse> {
    for token in ctx.cleanup_expired() {
        let _ = instance.backend().abort_store_state(token).await;
    }
    match request {
        // === Pre-auth: login handshake ===
        ServiceRequest::TrustedLoginUser { username } => {
            handle_trusted_login_user(instance, state, username).await
        }
        ServiceRequest::TrustedLoginProve { signature } => {
            handle_trusted_login_prove(state, &signature)
        }

        // === Pre-auth: server identity ===
        ServiceRequest::GetInstanceMetadata => {
            let metadata = instance.backend().get_instance_metadata().await?;
            Ok(ServiceResponse::InstanceMetadata(metadata))
        }

        // === Post-auth: extend the session keyset ===
        ServiceRequest::SessionKeyChallenge { pubkey } => {
            handle_session_key_challenge(state, pubkey)
        }
        ServiceRequest::SessionKeyRegister { pubkey, signature } => {
            handle_session_key_register(state, pubkey, &signature)
        }

        // === Authenticated storage operations ===
        //
        // Gate 1: the connection must have completed `TrustedLogin*`. Gate 2:
        // the per-tree permission gate. Every `DatabaseOp` carries its target
        // `root_id` explicitly, so the gate is *unconditional* — there is no
        // tree-less op to fall through it — with two exceptions handled below
        // (`SubmitSignedEntry`, verification-gated; `SetInstanceMetadata`,
        // gated against the server-known `_databases`, not the request root).
        ServiceRequest::AuthenticatedDb(inner) => {
            let (login_pubkey, keyset_snapshot, session_user_uuid) = match state {
                ConnectionState::Authenticated {
                    login_pubkey,
                    session_keyset,
                    user_uuid,
                    ..
                } => (
                    login_pubkey.clone(),
                    session_keyset.clone(),
                    user_uuid.clone(),
                ),
                _ => {
                    return Err(crate::Error::Auth(Box::new(
                        AuthError::InvalidAuthConfiguration {
                            reason: format!(
                                "database operation requires an authenticated connection; complete TrustedLogin* first (op: {:?})",
                                inner.op
                            ),
                        },
                    )));
                }
            };

            let AuthenticatedDbRequest {
                root_id,
                identity,
                op,
            } = *inner;

            // Submit is verification-gated, not session-gated.
            //
            // Reads are session-gated (confidentiality boundary); submits
            // are verification-gated (integrity boundary). `SubmitSignedEntry`
            // requires only an *authenticated* connection (gate 1, the
            // `ConnectionState::Authenticated` match above, still applies).
            // Which tree the entry belongs to, and whether its signer may
            // write that tree, is decided by the server's own verification
            // pass in the handler (store `Unverified`, then
            // `Database::open(...).verify()`) against the tree's *real*
            // pinned auth lineage — not by who holds the socket. An attacker
            // without a key the tree's auth grants cannot produce a
            // `Verified` entry, and unverified junk is excluded from every
            // default read by the frontier cut, so the per-tree session gate
            // adds no correctness or isolation property here; it only blocks
            // a legitimate transporter (e.g. an admin session carrying a
            // user-signed genesis). See the verification-gated-submit design
            // doc for the full threat analysis.
            let is_submit = matches!(op, DatabaseOp::SubmitSignedEntry { .. });

            // `SetInstanceMetadata` rewrites the daemon's pointers to its own
            // system DBs. It is gated against `_databases` (a server-known
            // tree), not the request's `root_id`, so an instance admin — by
            // construction a user with Admin on `_databases` via the
            // first-user bootstrap — is required. Fail closed (require_existing
            // = true): `_databases` always exists on an initialized daemon and
            // this is never a creation flow, so the create-flow passthrough
            // must not apply (it would let any authenticated user rewrite
            // system-DB pointers if `_databases` were ever unreadable).
            let is_set_metadata = matches!(op, DatabaseOp::SetInstanceMetadata { .. });

            // Submit accepts any identity hint (admin transports user-signed
            // entries); every other op resolves an acting pubkey from the
            // keyset and gates per-tree against it.
            let acting_pubkey = if is_submit {
                // Use the hint if it parses as a pubkey (for submit metadata),
                // otherwise fall back to login_pubkey — submit doesn't gate
                // on this value.
                identity
                    .hint()
                    .pubkey
                    .clone()
                    .unwrap_or_else(|| login_pubkey.clone())
            } else {
                resolve_acting_pubkey(&identity, &login_pubkey, &keyset_snapshot)?
            };

            // Per-tree permission gate. Unconditional for every op *except*
            // submit (verification in the handler is its boundary) and
            // set-metadata (gated against `_databases` below). Create-flow
            // passthrough (false) for the rest, so a not-yet-propagated tree is
            // waved through and database creation works.
            if is_set_metadata {
                gate_tree_permission(
                    instance,
                    &acting_pubkey,
                    &identity,
                    instance.databases_db_id(),
                    Permission::Admin(0),
                    true,
                )
                .await?;
            } else if !is_submit {
                gate_tree_permission(
                    instance,
                    &acting_pubkey,
                    &identity,
                    &root_id,
                    op.required_permission(),
                    false,
                )
                .await?;
            }

            dispatch_database_op(
                instance,
                ctx,
                &acting_pubkey,
                &identity,
                &session_user_uuid,
                root_id,
                op,
            )
            .await
        }
    }
}

/// Resolve the *acting* pubkey for a session-gated op.
///
/// The identity hint, when present, must be in the connection's session
/// keyset (proof of possession registered via `SessionKeyChallenge` /
/// `SessionKeyRegister`, or established at login time). Returning the hint
/// as the acting pubkey lets the per-tree gate check the actual key the
/// caller wants to act as, not the connection-wide login key.
///
/// An absent hint defaults to the login pubkey — matches the pre-keyset
/// behavior where every op acted as the login identity.
fn resolve_acting_pubkey(
    identity: &SigKey,
    login_pubkey: &PublicKey,
    session_keyset: &HashSet<PublicKey>,
) -> crate::Result<PublicKey> {
    match &identity.hint().pubkey {
        Some(claimed) if session_keyset.contains(claimed) => Ok(claimed.clone()),
        Some(claimed) => Err(crate::Error::Auth(Box::new(
            AuthError::SigningKeyMismatch {
                reason: format!(
                    "request identity claims pubkey '{claimed}' but it is not in the session keyset; \
                     register it first via SessionKeyChallenge/SessionKeyRegister"
                ),
            },
        ))),
        None => Ok(login_pubkey.clone()),
    }
}

/// Dispatch a Database-level op against the server's local `Database`.
///
/// Additive sibling of `dispatch_backend_op`. The caller has already verified
/// the session identity and run the unconditional per-tree permission gate on
/// `root_id`. Because the server runs the `Database` layer here, verify-on-read
/// and the Verified frontier are server-side **by construction**.
async fn dispatch_database_op(
    instance: &Instance,
    ctx: &ConnectionContext,
    acting_pubkey: &PublicKey,
    identity: &SigKey,
    user_uuid: &str,
    root_id: ID,
    op: DatabaseOp,
) -> crate::Result<ServiceResponse> {
    match op {
        DatabaseOp::ResolveStoreState { request } => {
            let request = session_store_state_request(request, user_uuid, &root_id)?;
            let view = match instance.backend().resolve_store_state(&request).await? {
                Some(view) => Some(view),
                // A client asks with its own scope; the daemon's own
                // materialized state is shared, so fall back to it rather than
                // rebuilding per user.
                None => {
                    let shared = StoreStateRequest {
                        scope: CacheScope::Shared,
                        ..request.clone()
                    };
                    instance.backend().resolve_store_state(&shared).await?
                }
            };
            let token = view.map(|view| {
                let token = random_token();
                ctx.staging.lock().unwrap().insert(
                    token.clone(),
                    SessionStaging {
                        backend: StagingToken {
                            namespace_id: view.namespace_id,
                            target: request.clone(),
                        },
                        user_uuid: user_uuid.to_string(),
                        database: request.database.clone(),
                        published: true,
                        chunks: HashMap::new(),
                        last_used: Instant::now(),
                    },
                );
                token
            });
            Ok(ServiceResponse::RecordView(token))
        }
        DatabaseOp::BeginStoreStateStaging { request } => {
            let request = session_store_state_request(request, user_uuid, &root_id)?;
            let backend = instance
                .backend()
                .begin_store_state_staging(request.clone())
                .await?;
            let token = random_token();
            ctx.staging.lock().unwrap().insert(
                token.clone(),
                SessionStaging {
                    backend,
                    user_uuid: user_uuid.to_string(),
                    database: request.database,
                    published: false,
                    chunks: HashMap::new(),
                    last_used: Instant::now(),
                },
            );
            Ok(ServiceResponse::Token(token))
        }
        DatabaseOp::StageStoreStateRecords {
            token,
            chunk_id,
            records,
        } => {
            let digest = serde_json::to_vec(&records)?;
            if digest.len() > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize {
                return Err(crate::backend::BackendError::RecordTooLarge {
                    encoded_bytes: digest.len(),
                }
                .into());
            }
            let records = records
                .into_iter()
                .collect::<crate::backend::RecordMutations>();
            let (backend, duplicate) =
                session_staging(ctx, user_uuid, &root_id, &token, chunk_id, &digest)?;
            if duplicate {
                return Ok(ServiceResponse::Ok);
            }
            if let Err(error) = instance
                .backend()
                .stage_store_state_records(&backend, records)
                .await
            {
                if let Some(value) = ctx.staging.lock().unwrap().get_mut(&token) {
                    value.chunks.remove(&chunk_id);
                }
                return Err(error);
            }
            Ok(ServiceResponse::Ok)
        }
        DatabaseOp::PublishStoreState { token } => {
            let staged = ctx
                .staging
                .lock()
                .unwrap()
                .remove(&token)
                .filter(|value| value.user_uuid == user_uuid && value.database == root_id)
                .ok_or(crate::backend::BackendError::InvalidStoreStateStagingToken)?;
            let backend_token = staged.backend;
            let view = match instance
                .backend()
                .publish_store_state(backend_token.clone())
                .await
            {
                Ok(view) => view,
                Err(error) => {
                    let _ = instance.backend().abort_store_state(backend_token).await;
                    return Err(error);
                }
            };
            let view_token = random_token();
            let database = root_id.clone();
            ctx.staging.lock().unwrap().insert(
                view_token.clone(),
                SessionStaging {
                    backend: StagingToken {
                        namespace_id: view.namespace_id,
                        target: StoreStateRequest {
                            database: database.clone(),
                            store: String::new(),
                            lifecycle: StoreStateLifecycle::Derived,
                            scope: CacheScope::User(user_uuid.to_string()),
                            projection: crate::backend::ProjectionDescriptor {
                                name: String::new(),
                                version: 0,
                            },
                            source_key: Vec::new(),
                        },
                    },
                    user_uuid: user_uuid.to_string(),
                    database,
                    published: true,
                    chunks: HashMap::new(),
                    last_used: Instant::now(),
                },
            );
            Ok(ServiceResponse::RecordView(Some(view_token)))
        }
        DatabaseOp::AbortStoreState { token } => {
            let staged = {
                let mut staging = ctx.staging.lock().unwrap();
                if staging
                    .get(&token)
                    .is_some_and(|value| value.user_uuid != user_uuid || value.database != root_id)
                {
                    return Err(crate::backend::BackendError::InvalidStoreStateStagingToken.into());
                }
                staging.remove(&token)
            };
            if let Some(staged) = staged {
                instance.backend().abort_store_state(staged.backend).await?;
            }
            Ok(ServiceResponse::Ok)
        }
        DatabaseOp::StoreStateRecordGet { view, key } => {
            let namespace_id = {
                let mut staged = ctx.staging.lock().unwrap();
                staged
                    .get_mut(&view)
                    .filter(|value| {
                        value.user_uuid == user_uuid && value.database == root_id && value.published
                    })
                    .map(|value| {
                        value.last_used = Instant::now();
                        value.backend.namespace_id.clone()
                    })
                    .ok_or(crate::backend::BackendError::InvalidStoreStateView)?
            };
            let record = instance
                .backend()
                .store_state_record_get(&crate::backend::RecordView { namespace_id }, &key)
                .await?;
            ensure_record_fits(record.as_ref())?;
            Ok(ServiceResponse::Record(record))
        }
        DatabaseOp::StoreStateRecordScan {
            view,
            range,
            after,
            max_records,
            max_encoded_bytes,
        } => {
            let backend = {
                let mut staged = ctx.staging.lock().unwrap();
                staged
                    .get_mut(&view)
                    .filter(|value| {
                        value.user_uuid == user_uuid && value.database == root_id && value.published
                    })
                    .map(|value| {
                        value.last_used = Instant::now();
                        value.backend.clone()
                    })
                    .ok_or(crate::backend::BackendError::InvalidStoreStateView)?
            };
            let page = bounded_page(
                instance
                    .backend()
                    .store_state_record_scan(
                        &crate::backend::RecordView {
                            namespace_id: backend.namespace_id,
                        },
                        &range,
                        after.as_deref(),
                        max_records as usize,
                    )
                    .await?,
                max_encoded_bytes,
            )?;
            Ok(ServiceResponse::RecordPage(page))
        }

        DatabaseOp::GetEntry { id } => {
            let entry = instance.backend().get(&id).await?;
            // Post-fetch owning-tree Read gate: a raw entry id carries no
            // inline tree, so the pre-dispatch gate (which keys on `root_id`)
            // could not cover the entry's real owning tree.
            gate_entry_read(instance, acting_pubkey, identity, &entry).await?;
            Ok(ServiceResponse::Entry(entry))
        }

        DatabaseOp::GetVerifiedTips => {
            // The server runs the Database layer, so `snapshot()` returns the
            // Verified frontier by construction — no client-side verify, no
            // remote-detection heuristic.
            let db = Database::open(instance, &root_id).await?;
            let snapshot = db.snapshot().await?;
            Ok(ServiceResponse::Ids(snapshot))
        }

        DatabaseOp::SubmitSignedEntry { entry } => {
            // The client signed this entry; the server does NOT trust its
            // claimed validity. Store it `Unverified`, then run our OWN
            // verification pass against the entry's pinned settings. A
            // poisoned entry never reaches `Verified` and is excluded from
            // every default read by the frontier cut — D1 is closed by
            // construction here, not by gate-hardening a raw `Put`.
            //
            // Only settled-state writes trigger an event: the
            // `put_entry(.., Unverified, ..)` is a no-fire path by
            // design (see `Instance::put_entry`), and `Database::verify`
            // fires its own batched `Verified` event for any entries
            // the pass settles. The handler just chains the two — no
            // extra fire bookkeeping here. Note this gates the trigger
            // only; the event's raw-frontier cursors can still bracket
            // an unsettled tip (see `Notification` rustdoc).
            instance
                .put_entry(
                    &root_id,
                    VerificationStatus::Unverified,
                    *entry,
                    WriteSource::Remote,
                )
                .await?;
            Database::open(instance, &root_id).await?.verify().await?;
            Ok(ServiceResponse::Ok)
        }

        DatabaseOp::BeginTransaction { stores, scope } => {
            // Single-sourced: both this handler and the Phase-3 remote seam
            // call `Database::transaction_context`, so `Transaction::commit`'s
            // build-sign path has one source of truth.
            let db = Database::open(instance, &root_id).await?;
            let ctx = db.transaction_context(&stores, scope).await?;
            Ok(ServiceResponse::TransactionContext(ctx))
        }

        DatabaseOp::GetStoreState { store } => {
            // Server-materialized merged state (unencrypted stores only).
            // Encrypted stores must use GetStoreEntries instead — the
            // ephemeral transaction here has no encryptor, and Doc
            // deserialization would fail on ciphertext.
            let db = Database::open(instance, &root_id).await?;
            let value = db.get_store_state(&store).await?;
            Ok(ServiceResponse::CrdtValue(value))
        }

        DatabaseOp::GetStoreEntries { store, tips, scope } => {
            // Universal primitive (encrypted + unencrypted): returns raw
            // Entry records with opaque data in canonical CRDT replay order.
            // For encrypted stores the client decrypts+merges locally.
            let db = Database::open(instance, &root_id).await?;
            let entries = db.get_store_entries(&store, &tips, scope).await?;
            Ok(ServiceResponse::Entries(entries))
        }

        DatabaseOp::GetStoreTipsUpToEntries { store, up_to } => {
            let db = Database::open(instance, &root_id).await?;
            let boundary = crate::Snapshot::from(up_to);
            let snapshot = db
                .ops()
                .store_snapshot_at(&root_id, &store, &boundary)
                .await?;
            Ok(ServiceResponse::Ids(snapshot))
        }

        DatabaseOp::ComputeMergeState { store, entry_ids } => {
            let db = Database::open(instance, &root_id).await?;
            let slice = db
                .ops()
                .compute_merge_state(&root_id, &store, &entry_ids)
                .await?;
            Ok(ServiceResponse::MergeState(MergeState {
                merge_base: slice.merge_base,
                path: slice.path,
            }))
        }

        DatabaseOp::SetInstanceMetadata { metadata } => {
            // Admin-on-`_databases` gate already ran in the dispatcher (against
            // the server-known system tree, not `root_id`).
            instance.backend().set_instance_metadata(&metadata).await?;
            Ok(ServiceResponse::Ok)
        }

        DatabaseOp::SubscribeWrites { tips } => {
            // Per-tree Read gate already ran in the dispatcher.
            //
            // Subscription is just a per-db callback registered against the
            // daemon's Instance. The callback's body pushes a notification
            // frame into this connection's writer channel; the daemon's
            // existing `fire_write_callbacks` dispatch handles fan-out by
            // walking the per-tree callback list. No separate registry or
            // global-publisher hook needed.
            //
            // Idempotent: a tree this connection has already subscribed to
            // is a no-op (we'd otherwise register a second callback that
            // pushes a duplicate frame on every write). Take the lock
            // briefly to early-out, then release before any await — the
            // std::Mutex isn't `Send` and can't cross an await point.
            //
            // Under the current dispatch shape the per-connection request
            // loop is single-threaded (one frame → one dispatch → one
            // response), so a second `SubscribeWrites` for the same tree
            // on the same connection can only arrive *after* the first
            // call has fully completed and recorded its subscription. The
            // post-await Entry-API guard below is therefore **defensive
            // against a future shape change** (per-connection parallel
            // dispatch, or another path that takes `ctx` and registers
            // callbacks concurrently) rather than fixing a present race.
            // Cheap to keep — one Entry-API call plus a single
            // `remove_write_callback` in the unreachable Occupied arm.
            {
                let subs = ctx.subscribed_lock();
                if subs.contains_key(&root_id) {
                    return Ok(ServiceResponse::Ok);
                }
            }
            let tx = ctx.tx.clone();
            // Capture the initial cursor and register the callback atomically
            // under the tree lock. The fire path (`put_entry` /
            // `Database::verify`) holds this same lock while advancing cursors,
            // so no write can land between the tips snapshot and the
            // registration: the empty-`tips` cursor is exactly the daemon's
            // frontier at the instant this subscription begins, with no
            // lost-update gap. Safe to hold across the `snapshot` await —
            // `Instance::snapshot` reads the raw backend (no tree lock) and,
            // unlike the fire path, we spawn/await no user callback under the
            // guard, so there is no reentrancy.
            let tree_lock = instance.tree_lock(&root_id);
            let tree_guard = tree_lock.lock().await;
            // Initial cursor: the client's supplied `tips` if non-empty;
            // otherwise the daemon's current tips at subscribe-time (the "I have
            // no initial state; give me events from now" posture documented on
            // the wire variant). The cursor advances per fire as usual.
            let initial_tips = if tips.is_empty() {
                // Propagate rather than defaulting: an error here would seed
                // the daemon-side cursor at `EMPTY`, which is the "replay
                // from the beginning" value rather than "start from now".
                instance.snapshot(&root_id).await?
            } else {
                tips
            };
            let id = instance.register_write_callback(
                root_id.clone(),
                initial_tips,
                move |event, db| {
                    // ORDERING INVARIANT — DO NOT add an `.await` before the
                    // `tx.send` below. Per-tree notifications are delivered in
                    // daemon-canonical order, and that guarantee rests entirely
                    // on this send running *synchronously*, in the closure's
                    // pre-future prefix. `Instance::spawn_write_callbacks`
                    // invokes this closure while the caller holds the tree's
                    // `tree_lock` (see `put_entry` / `Database::verify`), and it
                    // advances each subscription's cursor in the same critical
                    // section. Because the send happens here — before any await,
                    // still under that lock, in cursor-advance order — two
                    // connections writing the same tree concurrently cannot
                    // interleave their frames: writer A's send completes and A
                    // drops the lock before writer B can acquire it and send.
                    // The returned future is deliberately a no-op; only that
                    // (empty) tail is spawned and drained lock-free. Moving the
                    // send *into* the future — or awaiting anything before it —
                    // would push it out of the locked section onto the runtime's
                    // scheduler and let same-tree frames reorder (the bug fixed
                    // in "preserve write-notification order under concurrent
                    // writers"). `mpsc::UnboundedSender::send` is non-blocking
                    // and takes `&self`, which is exactly what makes a
                    // synchronous send here possible.
                    //
                    // Send failure (writer task gone) is silently dropped; the
                    // connection is about to be torn down and the guard's Drop
                    // will unregister us next.
                    //
                    // The closure only fires for settled-state writes today
                    // (Verified). Unverified writes go through `put_entry`
                    // without firing `fire_write_callbacks`, so no notification
                    // is ever *triggered* by them — though the cursors shipped
                    // here are raw frontiers and can bracket an unsettled tip.
                    let frame = ServerFrame::Notification(Notification::DatabaseWrite {
                        root_id: db.root_id().clone(),
                        previous_tips: event.previous_tips().clone(),
                        post_tips: event.post_tips().clone(),
                        source: event.source(),
                    });
                    let _ = tx.send(frame);
                    async move { Ok(()) }
                },
            );
            // Cursor captured and callback live; release the tree lock before
            // touching the std subscribed-lock below (never nest the two).
            drop(tree_guard);
            // Re-acquire the lock to record this connection's subscription.
            // Defensive Entry-API guard: in the current shape (single-
            // threaded per-connection dispatch) the Occupied arm is
            // unreachable — see the comment above the early-out check.
            // Kept so that a future shape change (parallel dispatch per
            // connection, or another concurrent path that takes `ctx`)
            // can't leave us with two callbacks pushing duplicate frames
            // on every write; the loser drops its just-registered
            // callback and the first registration stays.
            use std::collections::hash_map::Entry as MapEntry;
            let mut subs = ctx.subscribed_lock();
            match subs.entry(root_id.clone()) {
                MapEntry::Vacant(slot) => {
                    slot.insert(id);
                }
                MapEntry::Occupied(_) => {
                    drop(subs);
                    instance.remove_write_callback(&root_id, id);
                }
            }
            Ok(ServiceResponse::Ok)
        }

        DatabaseOp::UnsubscribeWrites => {
            // Per-tree Read gate already ran. Idempotent: unsubscribing a
            // tree this connection was not subscribed to is a no-op.
            let removed = ctx.subscribed_lock().remove(&root_id);
            if let Some(id) = removed {
                instance.remove_write_callback(&root_id, id);
            }
            Ok(ServiceResponse::Ok)
        }
    }
}

fn random_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Refuse a record the wire cannot carry, before it is framed.
fn ensure_record_fits(record: Option<&Vec<u8>>) -> crate::Result<()> {
    if let Some(record) = record {
        let encoded = serde_json::to_vec(record)?.len();
        if encoded > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize {
            return Err(crate::backend::BackendError::RecordTooLarge {
                encoded_bytes: encoded,
            }
            .into());
        }
    }
    Ok(())
}

/// Trim a page to the caller's encoded-byte budget, re-deriving the exclusive
/// continuation key from what actually survives. A single record that cannot
/// fit under the budget is an error rather than a silently empty page.
fn bounded_page(
    mut page: crate::backend::RecordPage,
    max_encoded_bytes: u32,
) -> crate::Result<crate::backend::RecordPage> {
    let limit = max_encoded_bytes.min(crate::service::protocol::MAX_RECORD_CHUNK_BYTES) as usize;
    let mut size = 2usize;
    let mut keep = 0usize;
    for record in &page.records {
        let record_size = serde_json::to_vec(record)?.len();
        if record_size > limit {
            return Err(crate::backend::BackendError::RecordTooLarge {
                encoded_bytes: record_size,
            }
            .into());
        }
        if size + record_size > limit {
            break;
        }
        size += record_size;
        keep += 1;
    }
    if keep < page.records.len() {
        page.records.truncate(keep);
        page.next = page.records.last().map(|(key, _)| key.clone());
    }
    Ok(page)
}

/// Bind a client-supplied Store-state request to the authenticated session.
///
/// The request travels with its own database and scope, so both are checked
/// against the gate the dispatcher already applied: the database must be the
/// authorized one, and a client may only address shared state or its own user
/// scope. Client-computed state is written under the user scope, so one
/// session cannot publish into another's namespace or into the daemon's.
fn session_store_state_request(
    mut request: StoreStateRequest,
    user_uuid: &str,
    database: &ID,
) -> crate::Result<StoreStateRequest> {
    if request.database != *database || request.lifecycle != StoreStateLifecycle::Derived {
        return Err(crate::backend::BackendError::InvalidStoreStateStagingToken.into());
    }
    match &request.scope {
        CacheScope::Shared => request.scope = CacheScope::User(user_uuid.to_string()),
        CacheScope::User(uuid) if uuid == user_uuid => {}
        CacheScope::User(_) => {
            return Err(crate::backend::BackendError::InvalidStoreStateStagingToken.into());
        }
    }
    Ok(request)
}

/// Resolve a session staging token, recording the chunk digest so a retried
/// upload is idempotent and a changed one is refused.
fn session_staging(
    ctx: &ConnectionContext,
    user_uuid: &str,
    database: &ID,
    token: &str,
    chunk_id: u64,
    digest: &[u8],
) -> crate::Result<(StagingToken, bool)> {
    let mut staging = ctx.staging.lock().unwrap();
    let value = staging
        .get_mut(token)
        .filter(|value| value.user_uuid == user_uuid && value.database == *database)
        .ok_or(crate::backend::BackendError::InvalidStoreStateStagingToken)?;
    if let Some(existing) = value.chunks.get(&chunk_id) {
        if existing != digest {
            return Err(crate::backend::BackendError::InvalidStoreStateStagingToken.into());
        }
        return Ok((value.backend.clone(), true));
    }
    value.chunks.insert(chunk_id, digest.to_vec());
    value.last_used = Instant::now();
    Ok((value.backend.clone(), false))
}

/// Handle `ServiceRequest::TrustedLoginUser`: look up the user's full record,
/// mint a challenge, and move the connection into `AwaitingProof`.
async fn handle_trusted_login_user(
    instance: &Instance,
    state: &mut ConnectionState,
    username: String,
) -> crate::Result<ServiceResponse> {
    // Look up the full `UserInfo`. The encrypted root key + salt ship to the
    // client so it can decrypt locally and sign the challenge in the same
    // round-trip; the daemon never sees the password or the plaintext key.
    // The non-credential fields (user_database_id, status) ride along so the
    // client can build the `User` session after proof without a second wire
    // read of `_users`. If the lookup fails (no such user, disabled,
    // duplicates), drop back to `PreAuth` and bubble the error.
    let users_db = instance.users_db().await?;
    let (user_uuid, user_info) = match lookup_user_record(&users_db, &username).await {
        Ok(v) => v,
        Err(e) => {
            *state = ConnectionState::PreAuth;
            return Err(e);
        }
    };

    let expected_pubkey = user_info.credentials.root_key_id.clone();
    let challenge = generate_challenge();
    *state = ConnectionState::AwaitingProof {
        username,
        user_uuid: user_uuid.clone(),
        challenge: challenge.clone(),
        expected_pubkey,
    };
    Ok(ServiceResponse::TrustedLoginChallenge {
        challenge,
        user_uuid,
        user_info,
    })
}

/// Handle `ServiceRequest::TrustedLoginProve`: verify the signature against the
/// stored challenge and either transition to `Authenticated` or drop back
/// to `PreAuth`.
fn handle_trusted_login_prove(
    state: &mut ConnectionState,
    signature: &[u8],
) -> crate::Result<ServiceResponse> {
    let (username, user_uuid, challenge, expected_pubkey) =
        match std::mem::replace(state, ConnectionState::PreAuth) {
            ConnectionState::AwaitingProof {
                username,
                user_uuid,
                challenge,
                expected_pubkey,
            } => (username, user_uuid, challenge, expected_pubkey),
            other => {
                // Restore the (unexpected) state we just took out so subsequent
                // requests see consistent state.
                *state = other;
                return Err(crate::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "TrustedLoginProve received outside of AwaitingProof state",
                )));
            }
        };

    match verify_challenge_response(&challenge, signature, &expected_pubkey) {
        Ok(()) => {
            let mut session_keyset = HashSet::new();
            session_keyset.insert(expected_pubkey.clone());
            *state = ConnectionState::Authenticated {
                username,
                user_uuid,
                login_pubkey: expected_pubkey,
                session_keyset,
                pending_key_challenges: HashMap::new(),
            };
            Ok(ServiceResponse::TrustedLoginOk)
        }
        Err(e) => {
            // Already reset to PreAuth via the mem::replace above.
            Err(crate::Error::Auth(Box::new(e)))
        }
    }
}

/// Handle `ServiceRequest::SessionKeyChallenge`: mint a single-use challenge
/// bound to `pubkey` and stash it in the connection's pending-challenges map.
///
/// Requires an authenticated connection. A repeat call for the same pubkey
/// overwrites the prior challenge — last-issued wins, so a stale challenge
/// can't be replayed.
fn handle_session_key_challenge(
    state: &mut ConnectionState,
    pubkey: PublicKey,
) -> crate::Result<ServiceResponse> {
    match state {
        ConnectionState::Authenticated {
            pending_key_challenges,
            ..
        } => {
            let challenge = generate_challenge();
            pending_key_challenges.insert(pubkey, challenge.clone());
            Ok(ServiceResponse::SessionKeyChallenge { challenge })
        }
        _ => Err(crate::Error::Auth(Box::new(
            AuthError::InvalidAuthConfiguration {
                reason: "SessionKeyChallenge requires an authenticated connection; \
                     complete TrustedLogin* first"
                    .to_string(),
            },
        ))),
    }
}

/// Handle `ServiceRequest::SessionKeyRegister`: verify the signature against
/// the matching pending challenge and, on success, add `pubkey` to the
/// connection's session keyset.
///
/// The challenge is consumed (removed) whether verification succeeds or fails,
/// so a bad signature can't be retried against the same challenge.
fn handle_session_key_register(
    state: &mut ConnectionState,
    pubkey: PublicKey,
    signature: &[u8],
) -> crate::Result<ServiceResponse> {
    match state {
        ConnectionState::Authenticated {
            session_keyset,
            pending_key_challenges,
            ..
        } => {
            let challenge = pending_key_challenges.remove(&pubkey).ok_or_else(|| {
                crate::Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: format!(
                        "no outstanding SessionKeyChallenge for pubkey '{pubkey}'; \
                         issue the challenge before registering"
                    ),
                }))
            })?;
            verify_challenge_response(&challenge, signature, &pubkey)
                .map_err(|e| crate::Error::Auth(Box::new(e)))?;
            session_keyset.insert(pubkey);
            Ok(ServiceResponse::Ok)
        }
        _ => Err(crate::Error::Auth(Box::new(
            AuthError::InvalidAuthConfiguration {
                reason: "SessionKeyRegister requires an authenticated connection; \
                     complete TrustedLogin* first"
                    .to_string(),
            },
        ))),
    }
}

/// Resolve `pubkey`'s permission against `tree_id`'s `auth_settings` and reject
/// the request if the resolved level doesn't cover `required`.
///
/// If the database doesn't exist on this daemon yet and `require_existing`
/// is false, the gate passes through so the dispatched op surfaces its own
/// response (NotFound, empty result, or — for write coordination — a
/// no-op). This is what keeps the legitimate "create a new database" flow
/// working: `Database::create` reads tips on the tree before its root entry
/// has propagated, so an outright denial here would break creation. The
/// cost is that callers can still distinguish "no such database" from
/// "exists but no access"; closing that existence-leak channel is filed as
/// a follow-up.
///
/// `require_existing = true` flips that to **fail closed**: an absent
/// target is denied rather than waved through. Used for the
/// `SetInstanceMetadata` admin gate (D8) — `_databases` always exists on an
/// initialized daemon and that op is never a creation flow, so the
/// create-flow passthrough there would only ever be a fail-open hole that
/// lets any authenticated user rewrite the daemon's system-DB pointers if
/// `_databases` were ever unreadable.
///
/// When the gate does fire, the denial error is the same shape regardless
/// of which sub-check failed (key not in auth_settings, mismatched hint,
/// insufficient permission level): no internal detail about *why* leaks
/// back over the wire.
///
/// System databases (`_users`, `_databases`, `_sync`, `_instance`) are gated
/// like any other tree: callers must hold the required permission in the
/// system DB's `auth_settings`. The instance-admin bootstrap (first user on
/// the device) writes the first user as `Admin(0)` on `_users` and
/// `_databases`, which is how legitimate administrative access is granted —
/// the previous hardcoded read exemption is gone. The daemon's device-keyed
/// local path still handles internal system-database maintenance writes that
/// originate inside the server.
async fn gate_tree_permission(
    instance: &Instance,
    pubkey: &PublicKey,
    identity: &SigKey,
    tree_id: &ID,
    required: Permission,
    require_existing: bool,
) -> crate::Result<()> {
    let denied = || {
        crate::Error::Auth(Box::new(AuthError::PermissionDenied {
            reason: format!("tree {tree_id}: pubkey {pubkey} not permitted for {required:?}"),
        }))
    };

    if !instance.has_database(tree_id).await {
        return if require_existing {
            Err(denied())
        } else {
            Ok(())
        };
    }

    let database = Database::open(instance, tree_id).await?;
    let settings_store = database.get_settings().await?;
    let auth_settings = settings_store.auth_snapshot().await?;

    let resolved =
        match resolve_identity_permission(pubkey, identity, &auth_settings, Some(instance)).await {
            Ok(p) => p,
            // Resolution failures (key not found, mismatch, etc.) collapse to the
            // same shape as an insufficient-permission denial, so the client
            // can't tell whether its identity was unknown or merely too low.
            Err(_) => return Err(denied()),
        };

    let allowed = match required {
        Permission::Read => true,
        Permission::Write(_) => resolved.can_write(),
        Permission::Admin(_) => resolved.can_admin(),
    };

    if !allowed {
        return Err(denied());
    }

    Ok(())
}

/// Per-tree read gate for ops keyed by a raw entry id, which therefore
/// carry no inline tree id and never hit the pre-dispatch `tree_id()` gate
/// (`Get`). The tree to authorise against is only knowable *after* the
/// fetch: it is the entry's claimed `tree.root`, or — for a tree-root
/// entry, whose `root()` is `None` — the entry's own id.
///
/// Model B (hard multi-tenant boundary): system DBs are unencrypted and
/// protected solely by this gate, so a raw cross-tree `Get` MUST resolve
/// and check the real owning tree before returning content. Delegates to
/// `gate_tree_permission`, so the `has_database`-absent passthrough and the
/// opaque denial shape are identical to the inline-tree-id path.
async fn gate_entry_read(
    instance: &Instance,
    pubkey: &PublicKey,
    identity: &SigKey,
    entry: &crate::entry::Entry,
) -> crate::Result<()> {
    let owning_tree = entry.root().unwrap_or_else(|| entry.id());
    // create-flow passthrough (false): a Get against a tree not yet
    // registered on this daemon must be waved through, same as the
    // inline-tree-id path.
    gate_tree_permission(
        instance,
        pubkey,
        identity,
        &owning_tree,
        Permission::Read,
        false,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::database::InMemory;
    use crate::service::protocol::{Handshake, write_frame};

    fn private_tempdir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(PARENT_MODE)).unwrap();
        dir
    }

    /// Helper: start a server on a temp socket, return path + shutdown sender.
    async fn start_test_server() -> (
        PathBuf,
        watch::Sender<()>,
        tokio::task::JoinHandle<crate::Result<()>>,
        Instance,
    ) {
        let dir = private_tempdir();
        let socket_path = dir.keep().join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();
        let (tx, rx) = watch::channel(());
        let server = ServiceServer::bind(instance.clone(), socket_path.clone())
            .await
            .unwrap();
        let task = tokio::spawn(server.run(rx));
        (socket_path, tx, task, instance)
    }

    #[tokio::test]
    async fn test_server_starts_and_shuts_down() {
        let (socket_path, tx, task, _instance) = start_test_server().await;
        assert!(socket_path.exists());
        drop(tx);
        task.await.unwrap().unwrap();
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_bind_creates_owner_only_parent_and_socket() {
        let dir = private_tempdir();
        let parent = dir.path().join("eidetica");
        let socket_path = parent.join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let server = ServiceServer::bind(instance, &socket_path).await.unwrap();

        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            PARENT_MODE
        );
        assert_eq!(
            std::fs::symlink_metadata(&socket_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            SOCKET_MODE
        );
        drop(server);
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    #[should_panic]
    async fn test_existing_shared_parent_mode_is_preserved() {
        let dir = private_tempdir();
        let parent = dir.path().join("shared");
        tokio::fs::create_dir(&parent).await.unwrap();
        tokio::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        let socket_path = parent.join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let server = ServiceServer::bind(instance, &socket_path).await.unwrap();

        let mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(
            std::fs::symlink_metadata(&socket_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            SOCKET_MODE
        );
        drop(server);
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    #[should_panic]
    async fn test_symlinked_nested_parent_is_created_privately() {
        let dir = private_tempdir();
        let target = dir.path().join("shared");
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        let link = dir.path().join("runtime");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let nested = link.join("one").join("two");
        let socket_path = nested.join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let server = ServiceServer::bind(instance, &socket_path).await.unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        for created in [target.join("one"), target.join("one").join("two")] {
            assert_eq!(
                std::fs::metadata(created).unwrap().permissions().mode() & 0o777,
                PARENT_MODE
            );
        }
        assert_eq!(server.socket_path(), target.join("one/two/test.sock"));
    }

    #[test]
    #[should_panic]
    fn test_restrictive_umask_does_not_weaken_created_parent_mode() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service::server::tests::restrictive_umask_bind_helper",
                "--nocapture",
            ])
            .env("EIDETICA_TEST_RESTRICTIVE_UMASK", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn restrictive_umask_bind_helper() {
        if std::env::var_os("EIDETICA_TEST_RESTRICTIVE_UMASK").is_none() {
            return;
        }

        unsafe { libc::umask(0o777) };
        let dir = private_tempdir();
        let parent = dir.path().join("one").join("two");
        let socket_path = parent.join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let server = ServiceServer::bind(instance, &socket_path).await.unwrap();
        for created in [dir.path().join("one"), parent] {
            assert_eq!(
                std::fs::metadata(created).unwrap().permissions().mode() & 0o777,
                PARENT_MODE
            );
        }
        assert_eq!(
            std::fs::symlink_metadata(&socket_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            SOCKET_MODE
        );
        drop(server);
    }

    #[tokio::test]
    async fn test_parent_replacement_is_refused() {
        let dir = private_tempdir();
        let parent = dir.path().join("endpoint");
        let replacement = dir.path().join("replacement");
        tokio::fs::create_dir(&parent).await.unwrap();
        tokio::fs::create_dir(&replacement).await.unwrap();
        tokio::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        tokio::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let socket_path = parent.join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let result = ServiceServer::bind_with_test_hook(instance, &socket_path, |stage| {
            if stage == BindTestStage::AfterParentSetup {
                std::fs::remove_dir(&parent).unwrap();
                std::fs::rename(&replacement, &parent).unwrap();
            }
        })
        .await;

        assert!(
            result.is_err(),
            "a replaced socket parent must invalidate bind"
        );
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_wrong_protocol_version() {
        let (socket_path, _tx, _task, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        // Send wrong version
        let handshake = Handshake {
            protocol_version: 1,
        };
        write_frame(&mut writer, &handshake).await.unwrap();

        // Read ack (server sends its version back)
        let ack: Option<HandshakeAck> = read_frame(&mut reader).await.unwrap();
        let ack = ack.unwrap();
        assert_eq!(ack.protocol_version, PROTOCOL_VERSION);

        // Connection should be closed by server after version mismatch
        // Next read should get EOF
        let result: crate::Result<Option<ServiceResponse>> = read_frame(&mut reader).await;
        assert!(result.unwrap().is_none());
    }

    /// Load-bearing invariant: `ConnectionState` must never hold plaintext
    /// signing material in any variant. The daemon participates in storage
    /// and challenge-response only; the rejected Branch A design held
    /// decrypted user keys server-side and that boundary was reinstated by
    /// design (see Service Architecture doc § Decision record).
    ///
    /// Structure of the test: construct each variant, destructure with named
    /// fields (so adding a field forces this test to be edited), and check
    /// the static type of each field is not `PrivateKey`. A future refactor
    /// that adds e.g. `decrypted_root_key: PrivateKey` to `Authenticated`
    /// would either fail the type check at runtime or fail the destructure
    /// match exhaustiveness at compile time.
    #[test]
    fn connection_state_never_holds_private_key() {
        use crate::auth::crypto::{PrivateKey, generate_keypair};
        use std::any::TypeId;

        fn assert_not_private_key<T: 'static>(_value: &T, label: &str) {
            assert_ne!(
                TypeId::of::<T>(),
                TypeId::of::<PrivateKey>(),
                "ConnectionState field `{label}` is PrivateKey — daemon must not hold plaintext keys"
            );
        }

        let (_signing, pubkey) = generate_keypair();
        let states = [
            ConnectionState::PreAuth,
            ConnectionState::AwaitingProof {
                username: "u".to_string(),
                user_uuid: "uu".to_string(),
                challenge: vec![1, 2, 3],
                expected_pubkey: pubkey.clone(),
            },
            ConnectionState::Authenticated {
                username: "u".to_string(),
                user_uuid: "uu".to_string(),
                login_pubkey: pubkey.clone(),
                session_keyset: {
                    let mut s = HashSet::new();
                    s.insert(pubkey);
                    s
                },
                pending_key_challenges: HashMap::new(),
            },
        ];

        for state in &states {
            match state {
                ConnectionState::PreAuth => {}
                ConnectionState::AwaitingProof {
                    username,
                    user_uuid,
                    challenge,
                    expected_pubkey,
                } => {
                    assert_not_private_key(username, "AwaitingProof::username");
                    assert_not_private_key(user_uuid, "AwaitingProof::user_uuid");
                    assert_not_private_key(challenge, "AwaitingProof::challenge");
                    assert_not_private_key(expected_pubkey, "AwaitingProof::expected_pubkey");
                }
                ConnectionState::Authenticated {
                    username,
                    user_uuid,
                    login_pubkey,
                    session_keyset,
                    pending_key_challenges,
                } => {
                    assert_not_private_key(username, "Authenticated::username");
                    assert_not_private_key(user_uuid, "Authenticated::user_uuid");
                    assert_not_private_key(login_pubkey, "Authenticated::login_pubkey");
                    for k in session_keyset {
                        assert_not_private_key(k, "Authenticated::session_keyset entry");
                    }
                    for (k, ch) in pending_key_challenges {
                        assert_not_private_key(k, "Authenticated::pending_key_challenges key");
                        assert_not_private_key(
                            ch,
                            "Authenticated::pending_key_challenges challenge",
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_authenticated_request_rejected_without_login() {
        // Companion to the integration test `test_unauthenticated_backend_op_rejected`
        // — exercises the same gate path against the raw protocol so a
        // regression here surfaces immediately, not just at the
        // `RemoteConnection` layer.
        let (socket_path, _tx, _task, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        write_frame(
            &mut writer,
            &Handshake {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        let _ack: Option<HandshakeAck> = read_frame(&mut reader).await.unwrap();

        // Send an AuthenticatedDb request without completing TrustedLogin.
        write_frame(
            &mut writer,
            &ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
                root_id: crate::entry::ID::default(),
                identity: crate::auth::types::SigKey::default(),
                op: DatabaseOp::GetEntry {
                    id: crate::entry::ID::from_bytes("nonexistent"),
                },
            })),
        )
        .await
        .unwrap();

        let frame: Option<ServerFrame> = read_frame(&mut reader).await.unwrap();
        let resp = match frame.unwrap() {
            ServerFrame::Response(r) => *r,
            other => panic!("Expected Response frame, got {other:?}"),
        };
        match resp {
            ServiceResponse::Error(e) => {
                assert_eq!(
                    e.module, "auth",
                    "expected an auth-module error from the gate; got {e:?}"
                );
            }
            other => panic!("Expected gate Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_instance_metadata() {
        let (socket_path, _tx, _task, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        // Handshake
        write_frame(
            &mut writer,
            &Handshake {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        let _ack: Option<HandshakeAck> = read_frame(&mut reader).await.unwrap();

        // Request metadata
        write_frame(&mut writer, &ServiceRequest::GetInstanceMetadata)
            .await
            .unwrap();

        let frame: Option<ServerFrame> = read_frame(&mut reader).await.unwrap();
        let resp = match frame.unwrap() {
            ServerFrame::Response(r) => *r,
            other => panic!("Expected Response frame, got {other:?}"),
        };
        match resp {
            ServiceResponse::InstanceMetadata(Some(_meta)) => {
                // Server was initialized so metadata should exist
            }
            other => panic!("Expected InstanceMetadata(Some), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stale_socket_cleanup() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");

        // Leave a stale socket path behind after its listener exits.
        drop(UnixListener::bind(&socket_path).unwrap());
        assert!(socket_path.exists());

        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();
        let (_tx, rx) = watch::channel(());
        let server = ServiceServer::bind(instance, socket_path.clone())
            .await
            .unwrap();
        let _stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let handle = tokio::spawn(server.run(rx));

        handle.abort();
        handle.await.unwrap_err();
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_second_bind_is_refused_and_first_endpoint_remains_usable() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let first = ServiceServer::bind(instance.clone(), &socket_path)
            .await
            .unwrap();
        let error = match ServiceServer::bind(instance, &socket_path).await {
            Ok(_) => panic!("second bind must be refused"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, crate::Error::Io(error) if error.kind() == std::io::ErrorKind::AddrInUse),
            "expected AddrInUse, got {error:?}"
        );
        tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("first endpoint must remain usable");
        drop(first);
    }

    #[tokio::test]
    async fn test_existing_regular_file_is_refused_and_preserved() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        tokio::fs::write(&socket_path, b"keep me").await.unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        assert!(
            ServiceServer::bind(instance, &socket_path).await.is_err(),
            "regular file must be refused"
        );
        assert_eq!(tokio::fs::read(&socket_path).await.unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn test_symlink_lockfile_is_refused_without_touching_target() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let lock_path = lock_path(&socket_path);
        let sentinel_path = dir.path().join("sentinel");
        tokio::fs::write(&sentinel_path, b"keep me").await.unwrap();
        std::fs::set_permissions(&sentinel_path, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&sentinel_path, &lock_path).unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        assert!(
            ServiceServer::bind(instance, &socket_path).await.is_err(),
            "symlink lockfile must be refused"
        );
        assert_eq!(tokio::fs::read(&sentinel_path).await.unwrap(), b"keep me");
        assert_eq!(
            std::fs::metadata(&sentinel_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(
            std::fs::symlink_metadata(&lock_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn test_hard_link_lockfile_is_refused_without_touching_target() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let lock_path = lock_path(&socket_path);
        let sentinel_path = dir.path().join("sentinel");
        tokio::fs::write(&sentinel_path, b"keep me").await.unwrap();
        std::fs::set_permissions(&sentinel_path, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::hard_link(&sentinel_path, &lock_path).unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        assert!(
            ServiceServer::bind(instance, &socket_path).await.is_err(),
            "hard-linked lockfile must be refused"
        );
        assert_eq!(tokio::fs::read(&sentinel_path).await.unwrap(), b"keep me");
        assert_eq!(
            std::fs::metadata(&sentinel_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    #[tokio::test]
    async fn test_replaced_lock_path_is_refused() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let lock_path = lock_path(&socket_path);
        let replacement = dir.path().join("replacement");
        tokio::fs::write(&replacement, b"replacement")
            .await
            .unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let result = ServiceServer::bind_with_test_hook(instance, &socket_path, |stage| {
            if stage == BindTestStage::AfterLock {
                std::fs::remove_file(&lock_path).unwrap();
                std::fs::rename(&replacement, &lock_path).unwrap();
            }
        })
        .await;

        assert!(result.is_err(), "a replaced lock path must invalidate bind");
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_stale_probe_preserves_replacement_socket() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        drop(UnixListener::bind(&socket_path).unwrap());
        let replacement_path = dir.path().join("replacement.sock");
        let replacement = UnixListener::bind(&replacement_path).unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let result = ServiceServer::bind_with_test_hook(instance, &socket_path, |stage| {
            if stage == BindTestStage::AfterStaleProbe {
                std::fs::remove_file(&socket_path).unwrap();
                std::fs::rename(&replacement_path, &socket_path).unwrap();
            }
        })
        .await;

        assert!(result.is_err(), "a replacement socket must invalidate bind");
        assert!(
            tokio::net::UnixStream::connect(&socket_path).await.is_ok(),
            "the replacement listener must remain reachable"
        );
        drop(replacement);
    }

    #[tokio::test]
    async fn test_socket_chmod_preserves_replacement_file() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let replacement_path = dir.path().join("replacement");
        tokio::fs::write(&replacement_path, b"replacement")
            .await
            .unwrap();
        std::fs::set_permissions(&replacement_path, std::fs::Permissions::from_mode(0o640))
            .unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let result = ServiceServer::bind_with_test_hook(instance, &socket_path, |stage| {
            if stage == BindTestStage::BeforeSocketChmod {
                std::fs::remove_file(&socket_path).unwrap();
                std::fs::rename(&replacement_path, &socket_path).unwrap();
            }
        })
        .await;

        assert!(result.is_err(), "a replacement path must invalidate bind");
        assert_eq!(std::fs::read(&socket_path).unwrap(), b"replacement");
        assert_eq!(
            std::fs::metadata(&socket_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640,
            "bind must not chmod a replacement path"
        );
    }

    #[tokio::test]
    async fn test_dropping_bound_server_cleans_socket() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let server = ServiceServer::bind(instance, &socket_path).await.unwrap();
        assert!(socket_path.exists());
        drop(server);
        assert!(!socket_path.exists());
        assert!(lock_path(&socket_path).exists(), "lockfile remains stable");
    }

    #[tokio::test]
    async fn test_drop_preserves_replacement_path() {
        let dir = private_tempdir();
        let socket_path = dir.path().join("test.sock");
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let server = ServiceServer::bind(instance, &socket_path).await.unwrap();
        tokio::fs::remove_file(&socket_path).await.unwrap();
        tokio::fs::write(&socket_path, b"replacement")
            .await
            .unwrap();
        drop(server);
        assert_eq!(tokio::fs::read(&socket_path).await.unwrap(), b"replacement");
    }

    #[tokio::test]
    async fn test_bind_failure_is_returned_before_server_is_ready() {
        let dir = private_tempdir();
        let parent = dir.path().join("not-a-directory");
        let socket_path = parent.join("test.sock");
        tokio::fs::write(&parent, "file").await.unwrap();
        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();

        let error = match ServiceServer::bind(instance, &socket_path).await {
            Ok(_) => panic!("startup bind failure must return from bind"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, crate::Error::Io(error) if error.kind() == std::io::ErrorKind::NotADirectory),
            "expected synchronous parent-path failure, got {error:?}"
        );
        assert!(!socket_path.exists());
    }

    /// D8 regression: `require_existing` flips the create-flow passthrough
    /// to fail-closed. An absent target is waved through with `false` (the
    /// `Database::create` path) but denied with `true` (the
    /// `SetInstanceMetadata` admin gate), so an unreadable `_databases`
    /// can't become a fail-open hole.
    #[tokio::test]
    async fn test_gate_require_existing_fails_closed_on_absent_db() {
        use crate::auth::crypto::generate_keypair;

        let (instance, _admin) = Instance::create_backend(
            Box::new(InMemory::new()),
            crate::NewUser::passwordless("admin"),
        )
        .await
        .unwrap();
        let (_sk, pubkey) = generate_keypair();
        let absent = ID::from_bytes("no-such-tree");

        gate_tree_permission(
            &instance,
            &pubkey,
            &SigKey::default(),
            &absent,
            Permission::Admin(0),
            false,
        )
        .await
        .expect("create-flow passthrough must wave an absent tree through");

        let err = gate_tree_permission(
            &instance,
            &pubkey,
            &SigKey::default(),
            &absent,
            Permission::Admin(0),
            true,
        )
        .await
        .expect_err("require_existing must deny an absent tree");
        assert!(
            matches!(&err, crate::Error::Auth(b) if matches!(**b, AuthError::PermissionDenied { .. })),
            "expected PermissionDenied, got: {err:?}",
        );
    }
}
