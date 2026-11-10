//! Inputs for creating a user.
//!
//! [`NewAccount`] is the request shape for "create a user with these settings",
//! used in two places:
//!
//! - [`Instance::create_backend`](super::Instance::create_backend) and
//!   [`Instance::connect_or_create`](super::Instance::connect_or_create) — the
//!   initial user who bootstraps a fresh instance, automatically granted
//!   Admin on the system databases by virtue of being the first user.
//! - [`InstanceAdmin::create_account`](crate::account::InstanceAdmin::create_account) —
//!   every subsequent user created by an existing admin.
//!
//! There is no separate "initial user" concept: the entity that creates the
//! instance is just the first `NewAccount`, and from that point on additional
//! users are created through the admin path. This eliminates the prior
//! hardcoded `admin/admin` bootstrap.

/// Inputs needed to create a user account.
///
/// All accounts store an authenticated encrypted credential bundle. Passwords
/// may be any string, including empty, with no length or complexity restrictions.
/// [`passwordless`](Self::passwordless) is sugar for an empty password, not a
/// different storage mode. Empty-string wrapping provides no meaningful
/// confidentiality against a bundle holder.
///
/// The struct is marked `#[non_exhaustive]` so we can add fields
/// (key algorithm, locale, initial roles, etc.) without a breaking API
/// change. Construct it through the provided helpers rather than struct
/// literal syntax.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewAccount {
    /// Login identifier for the user. Must be unique within the instance.
    pub username: String,
    /// Password used to encrypt the local credential bundle. Every string,
    /// including empty, uses the same Argon2id and AES-256-GCM path.
    pub password: String,
}

impl NewAccount {
    /// Create an empty-password account using the ordinary encrypted format.
    /// This provides no meaningful confidentiality against a bundle holder.
    pub fn passwordless(name: impl Into<String>) -> Self {
        Self::with_password(name, "")
    }

    /// Create an account with any password string, including empty.
    /// The password is required for login; signing uses already unlocked keys.
    pub fn with_password(name: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: name.into(),
            password: password.into(),
        }
    }
}
