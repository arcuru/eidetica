//! Service server: accepts Unix socket connections and dispatches `BackendImpl` operations.
//!
//! The server wraps an `Instance` (not just a backend) so it can handle write
//! notifications through the Instance's callback system.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;
use tokio::sync::watch;

use crate::Instance;
use crate::service::error::ServiceError;
use crate::service::protocol::{
    HandshakeAck, PROTOCOL_VERSION, ServiceRequest, ServiceResponse, read_frame, write_frame,
};

/// Eidetica service server that listens on a Unix domain socket.
///
/// The server wraps a full `Instance` so it can dispatch both storage operations
/// (via the backend) and write callbacks (via `Instance::put_entry()`'s notification path).
pub struct ServiceServer {
    instance: Instance,
    socket_path: PathBuf,
}

impl ServiceServer {
    /// Create a new service server.
    ///
    /// # Arguments
    /// * `instance` - The Instance to serve. The server holds a strong reference.
    /// * `socket_path` - Path for the Unix domain socket.
    pub fn new(instance: Instance, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            instance,
            socket_path: socket_path.into(),
        }
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Run the server until the shutdown signal is received.
    ///
    /// Removes any stale socket file, creates the parent directory, binds the
    /// listener, and loops accepting connections. Each connection is handled in
    /// a spawned task. On shutdown, the socket file is cleaned up.
    ///
    /// # Arguments
    /// * `shutdown` - A watch receiver; the server stops when the sender is dropped.
    pub async fn run(&self, mut shutdown: watch::Receiver<()>) -> crate::Result<()> {
        // Remove stale socket if it exists
        if self.socket_path.exists() {
            tokio::fs::remove_file(&self.socket_path).await?;
        }

        // Create parent directory with owner-only permissions (0700)
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
            tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // Restrict socket to owner-only access (0600)
        tokio::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))
            .await?;

        tracing::info!("Service server listening on {}", self.socket_path.display());

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let instance = self.instance.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, instance).await {
                                    tracing::debug!("Connection handler error: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Failed to accept connection: {e}");
                        }
                    }
                }
                _ = shutdown.changed() => {
                    tracing::info!("Service server shutting down");
                    break;
                }
            }
        }

        // Clean up socket file
        let _ = tokio::fs::remove_file(&self.socket_path).await;
        Ok(())
    }
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    instance: Instance,
) -> crate::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);

    // 1. Read and validate handshake
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

    // 2. Request/response loop
    loop {
        let request: ServiceRequest = match read_frame(&mut reader).await? {
            Some(req) => req,
            None => break, // Clean EOF
        };

        let response = dispatch(&instance, request).await;
        write_frame(&mut writer, &response).await?;
    }

    Ok(())
}

/// Dispatch a service request to the appropriate Instance/Backend method.
async fn dispatch(instance: &Instance, request: ServiceRequest) -> ServiceResponse {
    match dispatch_inner(instance, request).await {
        Ok(resp) => resp,
        Err(e) => ServiceResponse::Error(ServiceError::from(&e)),
    }
}

/// Inner dispatch that returns Result for ergonomic error handling.
async fn dispatch_inner(
    instance: &Instance,
    request: ServiceRequest,
) -> crate::Result<ServiceResponse> {
    let backend = instance.backend();

    match request {
        // === Entry operations ===
        ServiceRequest::Get { id } => {
            let entry = backend.get(&id).await?;
            Ok(ServiceResponse::Entry(entry))
        }
        ServiceRequest::Put {
            verification_status,
            entry,
        } => {
            backend.put(verification_status, entry).await?;
            Ok(ServiceResponse::Ok)
        }

        // === Verification ===
        ServiceRequest::GetVerificationStatus { id } => {
            let status = backend.get_verification_status(&id).await?;
            Ok(ServiceResponse::VerificationStatus(status))
        }
        ServiceRequest::UpdateVerificationStatus {
            id,
            verification_status,
        } => {
            backend
                .update_verification_status(&id, verification_status)
                .await?;
            Ok(ServiceResponse::Ok)
        }
        ServiceRequest::GetEntriesByVerificationStatus { status } => {
            let ids = backend.get_entries_by_verification_status(status).await?;
            Ok(ServiceResponse::Ids(ids))
        }

        // === Tips ===
        ServiceRequest::GetTips { tree } => {
            let tips = backend.get_tips(&tree).await?;
            Ok(ServiceResponse::Ids(tips))
        }
        ServiceRequest::GetStoreTips { tree, store } => {
            let tips = backend.get_store_tips(&tree, &store).await?;
            Ok(ServiceResponse::Ids(tips))
        }
        ServiceRequest::GetStoreTipsUpToEntries {
            tree,
            store,
            main_entries,
        } => {
            let tips = backend
                .get_store_tips_up_to_entries(&tree, &store, &main_entries)
                .await?;
            Ok(ServiceResponse::Ids(tips))
        }

        // === Tree/Store traversal ===
        ServiceRequest::AllRoots => {
            let roots = backend.all_roots().await?;
            Ok(ServiceResponse::Ids(roots))
        }
        ServiceRequest::FindMergeBase {
            tree,
            store,
            entry_ids,
        } => {
            let base = backend.find_merge_base(&tree, &store, &entry_ids).await?;
            Ok(ServiceResponse::Id(base))
        }
        ServiceRequest::CollectRootToTarget {
            tree,
            store,
            target_entry,
        } => {
            let path = backend
                .collect_root_to_target(&tree, &store, &target_entry)
                .await?;
            Ok(ServiceResponse::Ids(path))
        }
        ServiceRequest::GetTree { tree } => {
            let entries = backend.get_tree(&tree).await?;
            Ok(ServiceResponse::Entries(entries))
        }
        ServiceRequest::GetStore { tree, store } => {
            let entries = backend.get_store(&tree, &store).await?;
            Ok(ServiceResponse::Entries(entries))
        }
        ServiceRequest::GetTreeFromTips { tree, tips } => {
            let entries = backend.get_tree_from_tips(&tree, &tips).await?;
            Ok(ServiceResponse::Entries(entries))
        }
        ServiceRequest::GetStoreFromTips { tree, store, tips } => {
            let entries = backend.get_store_from_tips(&tree, &store, &tips).await?;
            Ok(ServiceResponse::Entries(entries))
        }

        // === CRDT cache ===
        ServiceRequest::GetCachedCrdtState { entry_id, store } => {
            let state = backend.get_cached_crdt_state(&entry_id, &store).await?;
            Ok(ServiceResponse::CachedCrdtState(state))
        }
        ServiceRequest::CacheCrdtState {
            entry_id,
            store,
            state,
        } => {
            backend.cache_crdt_state(&entry_id, &store, state).await?;
            Ok(ServiceResponse::Ok)
        }
        ServiceRequest::ClearCrdtCache => {
            backend.clear_crdt_cache().await?;
            Ok(ServiceResponse::Ok)
        }

        // === Path operations ===
        ServiceRequest::GetSortedStoreParents {
            tree_id,
            entry_id,
            store,
        } => {
            let parents = backend
                .get_sorted_store_parents(&tree_id, &entry_id, &store)
                .await?;
            Ok(ServiceResponse::Ids(parents))
        }
        ServiceRequest::GetPathFromTo {
            tree_id,
            store,
            from_id,
            to_ids,
        } => {
            let path = backend
                .get_path_from_to(&tree_id, &store, &from_id, &to_ids)
                .await?;
            Ok(ServiceResponse::Ids(path))
        }

        // === Instance metadata ===
        ServiceRequest::GetInstanceMetadata => {
            let metadata = backend.get_instance_metadata().await?;
            Ok(ServiceResponse::InstanceMetadata(metadata))
        }
        ServiceRequest::SetInstanceMetadata { metadata } => {
            backend.set_instance_metadata(&metadata).await?;
            Ok(ServiceResponse::Ok)
        }

        // === Write coordination ===
        ServiceRequest::NotifyEntryWritten {
            tree_id,
            entry_id,
            source,
        } => {
            // The entry is already stored by a preceding Put RPC.
            // Dispatch the Instance's write callbacks for the given tree/source.
            let entry = backend.get(&entry_id).await?;
            instance
                .dispatch_write_callbacks(&tree_id, &entry, source)
                .await?;
            Ok(ServiceResponse::Ok)
        }

        // === User management ===
        ServiceRequest::CreateUser { username, password } => {
            let uuid = instance.create_user(&username, password.as_deref()).await?;
            Ok(ServiceResponse::UserCreated(uuid))
        }
        ServiceRequest::ListUsers => {
            let users = instance.list_users().await?;
            Ok(ServiceResponse::Users(users))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::database::InMemory;
    use crate::service::protocol::{Handshake, write_frame};

    /// Helper: start a server on a temp socket, return path + shutdown sender.
    async fn start_test_server() -> (PathBuf, watch::Sender<()>, Instance) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.keep().join("test.sock");
        let instance = Instance::open(Box::new(InMemory::new())).await.unwrap();
        let (tx, rx) = watch::channel(());
        let server = ServiceServer::new(instance.clone(), socket_path.clone());
        tokio::spawn(async move {
            let _ = server.run(rx).await;
        });
        // Give the server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (socket_path, tx, instance)
    }

    #[tokio::test]
    async fn test_server_starts_and_shuts_down() {
        let (socket_path, tx, _instance) = start_test_server().await;
        assert!(socket_path.exists());
        drop(tx);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Socket should be cleaned up
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_wrong_protocol_version() {
        let (socket_path, _tx, _instance) = start_test_server().await;

        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        // Send wrong version
        let handshake = Handshake {
            protocol_version: 999,
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

    #[tokio::test]
    async fn test_get_nonexistent_entry() {
        let (socket_path, _tx, _instance) = start_test_server().await;

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

        // Request nonexistent entry
        write_frame(
            &mut writer,
            &ServiceRequest::Get {
                id: crate::entry::ID::from_bytes("nonexistent"),
            },
        )
        .await
        .unwrap();

        let resp: Option<ServiceResponse> = read_frame(&mut reader).await.unwrap();
        match resp.unwrap() {
            ServiceResponse::Error(e) => {
                assert_eq!(e.kind, "EntryNotFound");
            }
            other => panic!("Expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_instance_metadata() {
        let (socket_path, _tx, _instance) = start_test_server().await;

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

        let resp: Option<ServiceResponse> = read_frame(&mut reader).await.unwrap();
        match resp.unwrap() {
            ServiceResponse::InstanceMetadata(Some(_meta)) => {
                // Server was initialized so metadata should exist
            }
            other => panic!("Expected InstanceMetadata(Some), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stale_socket_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        // Create a stale socket file
        tokio::fs::write(&socket_path, "stale").await.unwrap();
        assert!(socket_path.exists());

        let instance = Instance::open(Box::new(InMemory::new())).await.unwrap();
        let (_tx, rx) = watch::channel(());
        let server = ServiceServer::new(instance, socket_path.clone());

        // Server should remove stale socket and bind successfully
        let handle = tokio::spawn(async move { server.run(rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(socket_path.exists());
        // Verify it's actually a socket now by connecting
        let _stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        handle.abort();
    }
}
