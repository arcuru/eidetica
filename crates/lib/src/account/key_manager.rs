//! Session-owned unlocked credentials and public installation-local key metadata.

use std::collections::HashMap;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{AccountError, AccountKey, KeyStorage, UnlockedCredentials};
use crate::{
    Result,
    auth::crypto::{PrivateKey, PublicKey},
};

/// Holds exactly the caller-unlocked keys for a session, never reference-derived
/// credentials. Private keys and the guarded wrapping key are dropped on clear.
pub struct AccountKeyManager {
    credentials: UnlockedCredentials,
    key_metadata: HashMap<PublicKey, AccountKey>,
}

impl AccountKeyManager {
    pub(crate) fn new(credentials: UnlockedCredentials, metadata: Vec<AccountKey>) -> Result<Self> {
        let mut manager = Self {
            credentials,
            key_metadata: HashMap::new(),
        };
        for key in metadata {
            manager.add_key(key)?;
        }
        // Missing metadata is corruption, not permission to hide a bundled key.
        if manager.key_metadata.len() != manager.credentials.key_ids().len() {
            return Err(AccountError::InvalidLocalState.into());
        }
        Ok(manager)
    }

    pub fn get_signing_key(&self, key_id: &PublicKey) -> Option<&PrivateKey> {
        self.credentials.get_signer(key_id)
    }

    /// Update public metadata only for an actual held signer.
    pub fn add_key(&mut self, metadata: AccountKey) -> Result<()> {
        if self.get_signing_key(&metadata.key_id).is_none() {
            return Err(AccountError::KeyNotFound {
                key_id: metadata.key_id.to_string(),
            }
            .into());
        }
        self.key_metadata.insert(metadata.key_id.clone(), metadata);
        Ok(())
    }

    pub(crate) fn import_key(&mut self, key: PrivateKey, metadata: AccountKey) -> Result<()> {
        self.credentials.add_key(key)?;
        self.add_key(metadata)
    }

    pub(crate) fn unlock_storage(&self, storage: &KeyStorage) -> Result<UnlockedCredentials> {
        self.credentials.unlock_storage(storage)
    }

    pub fn clear(&mut self) {
        self.zeroize();
    }

    /// Public key IDs sorted by creation time, then key ID for ties.
    pub fn list_key_ids(&self) -> Vec<PublicKey> {
        let mut keys: Vec<_> = self.key_metadata.values().collect();
        keys.sort_by_key(|key| (key.created_at, key.key_id.to_string()));
        keys.into_iter().map(|key| key.key_id.clone()).collect()
    }

    pub fn get_key_metadata(&self, key_id: &PublicKey) -> Option<&AccountKey> {
        self.key_metadata.get(key_id)
    }

    pub fn get_default_key_id(&self) -> Option<PublicKey> {
        self.list_key_ids()
            .into_iter()
            .find(|id| self.key_metadata[id].is_default)
            .or_else(|| self.list_key_ids().into_iter().next())
    }
}

impl Zeroize for AccountKeyManager {
    fn zeroize(&mut self) {
        self.credentials.clear();
        self.key_metadata.clear();
    }
}
impl Drop for AccountKeyManager {
    fn drop(&mut self) {
        self.zeroize();
    }
}
impl ZeroizeOnDrop for AccountKeyManager {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ID, account::CredentialBundle, auth::crypto::generate_keypair};

    fn metadata(key_id: PublicKey, created_at: i64, is_default: bool) -> AccountKey {
        AccountKey {
            key_id,
            created_at,
            is_default,
            display_name: None,
            last_used: None,
            database_sigkeys: HashMap::new(),
        }
    }

    #[test]
    fn key_manager_holds_exact_bundle_keys_and_metadata() {
        let (first, a) = generate_keypair();
        let (second, b) = generate_keypair();
        let account = ID::from_bytes("account");
        let bundle = CredentialBundle::seal(&account, &[first, second], "password").unwrap();
        let mut manager = AccountKeyManager::new(
            bundle.unlock("password").unwrap(),
            vec![metadata(a.clone(), 2, true), metadata(b.clone(), 1, false)],
        )
        .unwrap();
        assert_eq!(manager.list_key_ids(), [b.clone(), a.clone()]);
        assert_eq!(manager.get_default_key_id(), Some(a.clone()));
        let message = b"actual-key";
        b.verify(message, &manager.get_signing_key(&b).unwrap().sign(message))
            .unwrap();
        assert!(
            manager
                .add_key(metadata(generate_keypair().1, 3, false))
                .is_err()
        );
        let mut updated = manager.get_key_metadata(&b).unwrap().clone();
        updated.display_name = Some("new label".to_owned());
        manager.add_key(updated).unwrap();
        assert_eq!(
            manager
                .get_key_metadata(&b)
                .unwrap()
                .display_name
                .as_deref(),
            Some("new label")
        );
        let storage = manager.credentials.storage().unwrap();
        assert_eq!(
            manager.unlock_storage(&storage).unwrap().key_ids(),
            manager.credentials.key_ids()
        );
        manager.clear();
        assert!(manager.get_signing_key(&a).is_none());
        assert!(manager.list_key_ids().is_empty());
        assert!(manager.get_default_key_id().is_none());
    }

    #[test]
    fn key_manager_rejects_incomplete_or_foreign_metadata() {
        let (key, id) = generate_keypair();
        let bundle =
            CredentialBundle::seal(&ID::from_bytes("account"), &[key], "password").unwrap();
        assert!(AccountKeyManager::new(bundle.unlock("password").unwrap(), vec![]).is_err());
        assert!(
            AccountKeyManager::new(
                bundle.unlock("password").unwrap(),
                vec![metadata(generate_keypair().1, 0, true)]
            )
            .is_err()
        );
        let mut manager = AccountKeyManager::new(
            bundle.unlock("password").unwrap(),
            vec![metadata(id.clone(), 0, false)],
        )
        .unwrap();
        assert_eq!(manager.get_default_key_id(), Some(id));
        let rewrapped = bundle
            .unlock("password")
            .unwrap()
            .rewrap("new-password")
            .unwrap();
        assert!(
            manager
                .unlock_storage(&KeyStorage::Encrypted { bundle: rewrapped })
                .is_err()
        );
        manager.clear();
    }
}
