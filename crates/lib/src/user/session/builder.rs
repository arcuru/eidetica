//! Builder for creating a fully-initialized Database in a single genesis entry.
//!
//! See [`User::new_database`] for the entry point. The builder collects
//! settings, key policy, and a list of store initializers, then folds them all
//! into one signed entry at [`DatabaseBuilder::build`] time so the new
//! database is atomically constructed with its full initial shape.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use super::User;
use crate::{
    Database, Result, Store, Transaction, auth::crypto::PublicKey, crdt::Doc,
    user::errors::UserError,
};

/// Type-erased per-store initialization closure. The HRTB lets the closure
/// borrow the genesis `Transaction` for the duration of the returned future.
type StoreInit = Box<
    dyn for<'a> FnOnce(&'a Transaction) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        + Send,
>;

enum KeyPolicy {
    AutoGenerate { label: Option<String> },
    UseExisting(PublicKey),
}

/// Chainable builder for constructing a new Database with all of its initial
/// shape — settings, signing key, registered stores, and seed data — folded
/// into one genesis entry.
///
/// Obtained via [`User::new_database`]. Terminal method is
/// [`DatabaseBuilder::build`].
///
/// # Auto-generated keys on failure
///
/// When no key policy is set (or only [`Self::key_label`] is used), the
/// builder generates a fresh signing key via [`User::add_private_key`]
/// **before** running store initializers. If a store initializer or the
/// genesis commit then fails, the generated key remains in the user's
/// key store. `UserKeyManager` does not currently expose a key-removal
/// API, so this leak is by design — it matches the semantics of the
/// pre-existing `add_private_key` + `create_database` flow.
///
/// If avoiding the leak matters, call [`User::add_private_key`] yourself
/// and pass the result via [`Self::with_key`]; the key persists either way
/// but you keep ownership of when it was created.
///
/// # Example
///
/// ```ignore
/// use eidetica::store::DocStoreInit;
///
/// let (db, key) = user.new_database()
///     .name("agent:demo")
///     .key_label("agent:demo")
///     .empty_doc("config")
///     .initialize_doc("meta", meta)
///     .build()
///     .await?;
/// ```
pub struct DatabaseBuilder<'u> {
    user: &'u mut User,
    settings: Doc,
    key_policy: KeyPolicy,
    store_inits: Vec<(String, StoreInit)>,
}

impl<'u> DatabaseBuilder<'u> {
    pub(super) fn new(user: &'u mut User) -> Self {
        Self {
            user,
            settings: Doc::new(),
            key_policy: KeyPolicy::AutoGenerate { label: None },
            store_inits: Vec::new(),
        }
    }

    /// Set the database's display name (the `name` field in `_settings`).
    /// Shortcut for the common case; for full control over the settings Doc
    /// use [`Self::settings`] instead.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.settings.set("name", name.into());
        self
    }

    /// Replace the entire settings Doc. Overrides any prior [`Self::name`] call.
    pub fn settings(mut self, settings: Doc) -> Self {
        self.settings = settings;
        self
    }

    /// Generate a fresh signing key with the given display label when
    /// [`Self::create`] runs. Default behavior (no label) is also available by
    /// not calling either key method.
    pub fn key_label(mut self, label: impl Into<String>) -> Self {
        self.key_policy = KeyPolicy::AutoGenerate {
            label: Some(label.into()),
        };
        self
    }

    /// Use an existing key from the user's key manager rather than generating
    /// a fresh one. `key` must already have been added to the user via
    /// [`User::add_private_key`]; otherwise [`Self::create`] will fail when
    /// resolving the signing key.
    pub fn with_key(mut self, key: PublicKey) -> Self {
        self.key_policy = KeyPolicy::UseExisting(key);
        self
    }

    /// Register a store named `name` and run `init` against it inside the
    /// genesis transaction. The closure body uses the Store's normal write
    /// API. Pass a no-op closure to register an empty store.
    ///
    /// This is the generic primitive. Each Store module ships its own
    /// extension trait providing ergonomic non-generic variants (for example
    /// [`DocStoreInit::initialize_doc`](crate::store::DocStoreInit::initialize_doc)).
    pub fn initialize_store<S, F, Fut>(mut self, name: impl Into<String>, init: F) -> Self
    where
        S: Store + Send + 'static,
        F: FnOnce(S) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let name = name.into();
        let name_for_open = name.clone();
        let init_box: StoreInit = Box::new(move |txn| {
            Box::pin(async move {
                let store = S::open(txn, name_for_open).await?;
                init(store).await
            })
        });
        self.store_inits.push((name, init_box));
        self
    }

    /// Resolve the key, run every store initializer inside a single genesis
    /// transaction, commit, then perform the user-side tracking write.
    /// Returns the new database and the public key it was created with.
    pub async fn build(self) -> Result<(Database, PublicKey)> {
        let DatabaseBuilder {
            user,
            settings,
            key_policy,
            store_inits,
        } = self;

        // Reject duplicate store names before doing any work.
        let mut seen: HashSet<&str> = HashSet::new();
        for (name, _) in &store_inits {
            if !seen.insert(name.as_str()) {
                return Err(UserError::DuplicateBuilderStore { name: name.clone() }.into());
            }
        }

        // Resolve the signing key.
        let key_id = match key_policy {
            KeyPolicy::AutoGenerate { label } => user.add_private_key(label.as_deref()).await?,
            KeyPolicy::UseExisting(k) => k,
        };

        // Fold all per-store initializers into one async callback for the
        // genesis transaction. Each `init` is invoked once and consumed.
        let database = user
            .create_database_with_init(settings, &key_id, async move |txn| {
                for (_, init) in store_inits {
                    init(txn).await?;
                }
                Ok(())
            })
            .await?;

        Ok((database, key_id))
    }
}
