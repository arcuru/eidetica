//! Explicit installation-scoped credentials, independent of a daemon login.
//!
//! A bundle contains one Account locator and exactly the supplied private keys.
//! References and Identity membership are deliberately not inputs. Unlocking
//! obtains signers, not authority: ordinary database Auth still applies.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{AccountError, crypto};
use crate::{
    ID, Result,
    auth::crypto::{PrivateKey, PublicKey},
};

// Version 1 pins the existing account Argon2id 19 MiB / t=2 / p=1,
// base64-text-as-salt convention and AES-256-GCM. It is not a PasswordStore
// format, and does not adopt or migrate PasswordStore's independently versioned
// envelopes. The domain/version is authenticated alongside the entire payload.
const VERSION: u8 = 1;
const AAD: &[u8] = b"eidetica:local-credential-bundle:v1";

/// An encrypted local bundle, safe to persist outside the replicated DAG.
///
/// This is a local storage format, not a portable Account schema or a recovery
/// export. The ordinary login directory is not yet backed by this type.
/// Encrypted records may be served to filesystem-trusted socket clients;
/// their passwords and decrypted keys need not be sent to a daemon.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialBundle {
    version: u8,
    salt: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Unlocked keys and their Account locator, owned by the caller.
///
/// No Debug/Serialize/Clone implementation is provided. Returned signer clones
/// and handles have independent lifetimes; dropping this owner is not revocation
/// or a promise to erase parser, allocator or cryptographic-library copies.
pub struct UnlockedCredentials {
    account_id: ID,
    keys: Vec<PrivateKey>,
    // Retain only the wrapping key, not the password, for explicit key updates
    // and password rewrap. Zeroized on drop, including early error returns.
    wrapping_key: Zeroizing<Vec<u8>>,
    salt: String,
}

#[derive(Serialize)]
struct BorrowedPayload<'a> {
    account_id: &'a ID,
    keys: &'a [PrivateKey],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadData {
    account_id: ID,
    keys: Vec<PrivateKey>,
}

fn invalid_bundle() -> crate::Error {
    AccountError::DecryptionFailed {
        reason: "Invalid local credential bundle".to_owned(),
    }
    .into()
}

fn validate_keys(keys: &[PrivateKey]) -> Result<()> {
    if keys.is_empty() {
        return Err(AccountError::NoKeysAvailable.into());
    }
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        if !seen.insert(key.public_key()) {
            return Err(AccountError::KeyAlreadyExists {
                key_id: key.public_key().to_string(),
            }
            .into());
        }
    }
    Ok(())
}

impl CredentialBundle {
    /// Encrypt exactly these installation keys and this Account locator with
    /// one password. Keys must be nonempty and unique. No reference lookup,
    /// membership enrollment, or database write occurs.
    ///
    /// ```
    /// use eidetica::{ID, account::{CredentialBundle, AccountSigner}, auth::crypto::generate_keypair};
    /// let account_id = ID::from_bytes("example-account-locator");
    /// let (private, public) = generate_keypair();
    /// let encrypted = CredentialBundle::seal(&account_id, &[private], "local password")?;
    /// let bytes = serde_json::to_vec(&encrypted)?; // Persist only outside the Entry DAG.
    /// let stored: CredentialBundle = serde_json::from_slice(&bytes)?;
    /// let unlocked = stored.unlock_for(&account_id, "local password")?;
    /// let acting = AccountSigner::new(account_id.clone(), unlocked.signer(&public)?);
    /// assert_eq!(acting.account_id(), &account_id);
    /// assert_eq!(acting.key_id(), public);
    /// drop(unlocked); // The acting handle owns an independent signer clone.
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn seal(account_id: &ID, keys: &[PrivateKey], password: &str) -> Result<Self> {
        validate_keys(keys)?;
        let salt = crypto::generate_salt();
        let wrapping_key = Zeroizing::new(crypto::derive_encryption_key(password, &salt)?);
        Self::encrypt(account_id, keys, &salt, &wrapping_key)
    }

    fn encrypt(account_id: &ID, keys: &[PrivateKey], salt: &str, key: &[u8]) -> Result<Self> {
        // Guard the buffer before serialization; the reservation avoids moving
        // completed plaintext into abandoned reallocations. As with PasswordStore,
        // serde's internal per-field scratch is not a complete-memory-wipe claim.
        let payload = BorrowedPayload { account_id, keys };
        let mut counter = ByteCount(0);
        serde_json::to_writer(&mut counter, &payload).map_err(|_| invalid_bundle())?;
        let mut plaintext = Zeroizing::new(Vec::with_capacity(counter.0));
        serde_json::to_writer(&mut *plaintext, &payload).map_err(|_| invalid_bundle())?;
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| invalid_bundle())?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plaintext,
                    aad: AAD,
                },
            )
            .map_err(|_| invalid_bundle())?;
        Ok(Self {
            version: VERSION,
            salt: salt.to_owned(),
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    /// Unlock on the client. The Account locator and every private key are
    /// authenticated by the same envelope. Unknown formats, tampering and wrong
    /// passwords fail closed without echoing decrypted bytes in diagnostics.
    pub fn unlock(&self, password: &str) -> Result<UnlockedCredentials> {
        if self.version != VERSION || self.nonce.len() != crypto::NONCE_LENGTH {
            return Err(invalid_bundle());
        }
        let wrapping_key = Zeroizing::new(crypto::derive_encryption_key(password, &self.salt)?);
        let cipher = Aes256Gcm::new_from_slice(&wrapping_key).map_err(|_| invalid_bundle())?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&self.nonce),
                    Payload {
                        msg: &self.ciphertext,
                        aad: AAD,
                    },
                )
                .map_err(|_| invalid_bundle())?,
        );
        let payload: PayloadData =
            serde_json::from_slice(&plaintext).map_err(|_| invalid_bundle())?;
        validate_keys(&payload.keys)?;
        Ok(UnlockedCredentials {
            account_id: payload.account_id,
            keys: payload.keys,
            wrapping_key,
            salt: self.salt.clone(),
        })
    }

    /// Unlock only if the encrypted locator matches the expected Account.
    /// A directory hint or copied encrypted blob cannot rebind its locator.
    pub fn unlock_for(&self, account_id: &ID, password: &str) -> Result<UnlockedCredentials> {
        let credentials = self.unlock(password)?;
        if credentials.account_id() != account_id {
            return Err(invalid_bundle());
        }
        Ok(credentials)
    }
}

impl UnlockedCredentials {
    /// The authenticated Account locator. It grants no access by itself.
    pub fn account_id(&self) -> &ID {
        &self.account_id
    }

    /// Public IDs of exactly the explicitly supplied keys, in supplied order.
    pub fn key_ids(&self) -> Vec<PublicKey> {
        self.keys.iter().map(PrivateKey::public_key).collect()
    }

    /// Obtain an independently owned signer for an explicit key selection.
    /// A missing key is an error, never fallback to a personal/default key.
    pub fn signer(&self, key_id: &PublicKey) -> Result<PrivateKey> {
        self.keys
            .iter()
            .find(|key| &key.public_key() == key_id)
            .cloned()
            .ok_or_else(|| {
                AccountError::KeyNotFound {
                    key_id: key_id.to_string(),
                }
                .into()
            })
    }

    /// Add an explicitly supplied key locally. Neither references nor Auth
    /// changes call this method automatically. Persist with [`Self::seal`].
    pub fn add_key(&mut self, key: PrivateKey) -> Result<()> {
        let key_id = key.public_key();
        if self.keys.iter().any(|key| key.public_key() == key_id) {
            return Err(AccountError::KeyAlreadyExists {
                key_id: key_id.to_string(),
            }
            .into());
        }
        self.keys.push(key);
        Ok(())
    }

    /// Encrypt the current explicit bundle using the unlocked wrapping key and
    /// a fresh nonce. Does not persist or change the password.
    pub fn seal(&self) -> Result<CredentialBundle> {
        CredentialBundle::encrypt(&self.account_id, &self.keys, &self.salt, &self.wrapping_key)
    }

    /// Rewrap with a new password and salt. Does not revoke or rotate signing
    /// keys, invalidate existing handles or render old copies unrecoverable.
    /// The caller must explicitly persist the result before discarding it.
    pub fn rewrap(&self, password: &str) -> Result<CredentialBundle> {
        CredentialBundle::seal(&self.account_id, &self.keys, password)
    }
}

struct ByteCount(usize);
impl std::io::Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
