//! Wire-only representations of values whose ordinary Serde form does not
//! survive or stream through the binary codec.
//!
//! Each adapter leaves human-readable output exactly as it was and changes no
//! stored encoding.

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};

use crate::{
    account::{AccountCredentials, AccountInfo, AccountStatus, KeyStorage},
    auth::crypto::{PrivateKey, PublicKey},
    entry::ID,
};

/// Serialize JSON so `null` is a value of its own. The binary serializer writes
/// a unit as an empty array, which would turn `null` into `[]`.
pub(crate) fn json_value<S: Serializer>(
    value: &serde_json::Value,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde_json::Value;

    struct Nested<'a>(&'a Value);
    impl Serialize for Nested<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            json_value(self.0, serializer)
        }
    }

    match value {
        Value::Null => serializer.serialize_none(),
        Value::Bool(value) => serializer.serialize_bool(*value),
        Value::Number(value) => value.serialize(serializer),
        Value::String(value) => serializer.serialize_str(value),
        Value::Array(values) => {
            let mut sequence = serializer.serialize_seq(Some(values.len()))?;
            for value in values {
                sequence.serialize_element(&Nested(value))?;
            }
            sequence.end()
        }
        Value::Object(values) => {
            let mut map = serializer.serialize_map(Some(values.len()))?;
            for (key, value) in values {
                map.serialize_entry(key, &Nested(value))?;
            }
            map.end()
        }
    }
}

// `KeyStorage` is internally tagged in storage, which makes Serde buffer its
// whole content while decoding. On the binary wire it is externally tagged and
// its bytes are byte strings, so a ciphertext streams into place.
#[derive(Serialize)]
enum KeyRef<'a> {
    Encrypted {
        algorithm: &'a str,
        #[serde(with = "serde_bytes")]
        ciphertext: &'a [u8],
        #[serde(with = "serde_bytes")]
        nonce: &'a [u8],
    },
    Unencrypted {
        key: &'a PrivateKey,
    },
}

#[derive(Deserialize)]
enum KeyOwned {
    Encrypted {
        algorithm: String,
        #[serde(with = "serde_bytes")]
        ciphertext: Vec<u8>,
        #[serde(with = "serde_bytes")]
        nonce: Vec<u8>,
    },
    Unencrypted {
        key: PrivateKey,
    },
}

#[derive(Serialize)]
struct CredentialsRef<'a> {
    root_key_id: &'a PublicKey,
    root_key: KeyRef<'a>,
    password_salt: &'a Option<String>,
}

#[derive(Deserialize)]
struct CredentialsOwned {
    root_key_id: PublicKey,
    root_key: KeyOwned,
    password_salt: Option<String>,
}

#[derive(Serialize)]
struct AccountRef<'a> {
    username: &'a str,
    user_database_id: &'a ID,
    credentials: CredentialsRef<'a>,
    created_at: i64,
    status: &'a AccountStatus,
}

#[derive(Deserialize)]
struct AccountOwned {
    username: String,
    user_database_id: ID,
    credentials: CredentialsOwned,
    created_at: i64,
    status: AccountStatus,
}

/// Serde `with` adapter for [`AccountInfo`] in protocol messages.
pub(crate) mod account_info {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(
        account: &AccountInfo,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return account.serialize(serializer);
        }
        let credentials = &account.credentials;
        let root_key = match &credentials.root_key {
            KeyStorage::Encrypted {
                algorithm,
                ciphertext,
                nonce,
            } => KeyRef::Encrypted {
                algorithm,
                ciphertext,
                nonce,
            },
            KeyStorage::Unencrypted { key } => KeyRef::Unencrypted { key },
        };
        AccountRef {
            username: &account.username,
            user_database_id: &account.account_id,
            credentials: CredentialsRef {
                root_key_id: &credentials.root_key_id,
                root_key,
                password_salt: &credentials.password_salt,
            },
            created_at: account.created_at,
            status: &account.status,
        }
        .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<AccountInfo, D::Error> {
        if deserializer.is_human_readable() {
            return AccountInfo::deserialize(deserializer);
        }
        let account = AccountOwned::deserialize(deserializer)?;
        let credentials = account.credentials;
        let root_key = match credentials.root_key {
            KeyOwned::Encrypted {
                algorithm,
                ciphertext,
                nonce,
            } => KeyStorage::Encrypted {
                algorithm,
                ciphertext,
                nonce,
            },
            KeyOwned::Unencrypted { key } => KeyStorage::Unencrypted { key },
        };
        Ok(AccountInfo {
            username: account.username,
            account_id: account.user_database_id,
            credentials: AccountCredentials {
                root_key_id: credentials.root_key_id,
                root_key,
                password_salt: credentials.password_salt,
            },
            created_at: account.created_at,
            status: account.status,
        })
    }
}
