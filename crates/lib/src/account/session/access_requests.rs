//! Foreground, service-neutral access-request decision bookkeeping.

use super::*;

impl AccountSession {
    /// List pending access/membership requests for one target using an owned Admin
    /// key. Works through the daemon or local Sync. This grants nothing and reads
    /// no other target's requests. No client-side Sync is needed when connected.
    pub async fn pending_access_requests(
        &self,
        root_id: &ID,
        key_id: &PublicKey,
    ) -> Result<Vec<(String, BootstrapRequest)>> {
        let _key = self.get_signing_key(key_id)?;
        let identity = SigKey::from_pubkey(key_id);
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = self.instance.remote_connection() {
            connection.register_session_key(&_key).await?;
            return connection.pending_access_requests(root_id, identity).await;
        }
        self.instance
            .sync()
            .ok_or(SyncError::SyncNotEnabled)?
            .pending_access_requests_for(root_id, key_id, &identity)
            .await
    }

    /// Record approval after an explicit ordinary target Auth grant.
    ///
    /// Inspect the pending request, use a bound Database/Identity handle to grant
    /// the exact approved permission, then call this. It refuses missing authority
    /// rather than replaying an old grant. A grant and decision are separate
    /// commits: on partial failure inspect/retry, never copy a device's private key.
    /// Repeating a completed decision acknowledges history, not live membership.
    pub async fn complete_access_request(
        &self,
        root_id: &ID,
        request_id: &str,
        key_id: &PublicKey,
    ) -> Result<()> {
        self.finish_access_request(root_id, request_id, key_id, false)
            .await
    }

    /// Reject a pending shared request as a current target Admin. The rejection is
    /// terminal for that exact root/key/permission/requirement and adds no grant.
    pub async fn reject_access_request(
        &self,
        root_id: &ID,
        request_id: &str,
        key_id: &PublicKey,
    ) -> Result<()> {
        self.finish_access_request(root_id, request_id, key_id, true)
            .await
    }

    async fn finish_access_request(
        &self,
        root_id: &ID,
        request_id: &str,
        key_id: &PublicKey,
        reject: bool,
    ) -> Result<()> {
        let _key = self.get_signing_key(key_id)?;
        let identity = SigKey::from_pubkey(key_id);
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = self.instance.remote_connection() {
            connection.register_session_key(&_key).await?;
            return connection
                .finish_access_request(root_id, request_id, identity, reject)
                .await;
        }
        self.instance
            .sync()
            .ok_or(SyncError::SyncNotEnabled)?
            .finish_access_request(root_id, request_id, key_id, &identity, reject)
            .await
    }
}
