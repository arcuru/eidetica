//! Inputs for creating a user.
//!
//! [`NewUser`] is the request shape for "create a user with these settings",
//! used in two places:
//!
//! - [`Instance::create`](super::Instance::create) and
//!   [`Instance::open_or_create`](super::Instance::open_or_create) — the
//!   initial user who bootstraps a fresh instance, automatically granted
//!   Admin on the system databases by virtue of being the first user.
//! - [`InstanceAdmin::create_user`](crate::user::InstanceAdmin::create_user) —
//!   every subsequent user created by an existing admin.
//!
//! There is no separate "initial user" concept: the entity that creates the
//! instance is just the first `NewUser`, and from that point on additional
//! users are created through the admin path. This eliminates the prior
//! hardcoded `admin/admin` bootstrap.

/// Inputs needed to create a user account.
///
/// Use the [`passwordless`](Self::passwordless) constructor for embedded or
/// single-user applications where login latency matters and the surrounding
/// process is already trusted; use [`with_password`](Self::with_password) for
/// multi-user deployments where the root key must be encrypted at rest.
///
/// The struct is marked `#[non_exhaustive]` so we can add fields
/// (key algorithm, locale, initial roles, etc.) without a breaking API
/// change. Construct it through the provided helpers rather than struct
/// literal syntax.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewUser {
    /// Login identifier for the user. Must be unique within the instance.
    pub username: String,
    /// Optional password. `None` produces a passwordless user whose root key
    /// is stored unencrypted (fine for embedded apps where the host process
    /// is the trust boundary); `Some(_)` produces an Argon2id-derived
    /// encryption key under which the root signing key is AES-256-GCM
    /// encrypted at rest.
    pub password: Option<String>,
}

impl NewUser {
    /// Create a passwordless user. The root signing key will be stored
    /// unencrypted in the user's credentials.
    pub fn passwordless(name: impl Into<String>) -> Self {
        Self {
            username: name.into(),
            password: None,
        }
    }

    /// Create a password-protected user. The root signing key will be
    /// encrypted with an Argon2id-derived key at rest; the password is
    /// required for every login.
    pub fn with_password(name: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: name.into(),
            password: Some(password.into()),
        }
    }
}
