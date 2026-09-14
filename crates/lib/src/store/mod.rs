use crate::HeightStrategy;
use crate::crdt::{CRDT, Doc};
use crate::{Result, Transaction};
use async_trait::async_trait;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::backend::RecordMutations;

/// Converts canonical Entry deltas to and from a Store's cached record format.
pub trait RecordProjection<D: CRDT>: Send + Sync {
    fn descriptor(&self) -> ProjectionDescriptor;
    fn project_delta(&self, delta: &D, out: &mut RecordMutations) -> Result<()>;
    fn encode_entry_delta(&self, mutations: &RecordMutations) -> Result<D>;

    /// Converts a caller-facing key into its persisted record key.
    fn normalize_record_key(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(Some(key.to_vec()))
    }

    /// Whether two staged keys conflict.
    ///
    /// Transactions retain only the newest staged value for each conflicting
    /// pair. The default preserves exact-key projections.
    fn staged_keys_conflict(&self, left: &[u8], right: &[u8]) -> bool {
        left == right
    }

    /// Whether a staged key makes a cached key stale.
    fn staged_key_shadows_cached(&self, staged_key: &[u8], cached_key: &[u8]) -> bool {
        self.staged_keys_conflict(staged_key, cached_key)
    }

    /// Whether a staged key is a descendant of a caller-facing key.
    fn staged_key_descends_from(&self, staged_key: &[u8], key: &[u8]) -> bool {
        staged_key
            .strip_prefix(key)
            .is_some_and(|suffix| suffix.starts_with(b"."))
    }
}

struct DescribedProjection<D: CRDT + 'static> {
    descriptor: ProjectionDescriptor,
    inner: Arc<dyn RecordProjection<D>>,
}

impl<D: CRDT + 'static> RecordProjection<D> for DescribedProjection<D> {
    fn descriptor(&self) -> ProjectionDescriptor {
        self.descriptor.clone()
    }

    fn project_delta(&self, delta: &D, out: &mut RecordMutations) -> Result<()> {
        self.inner.project_delta(delta, out)
    }

    fn encode_entry_delta(&self, mutations: &RecordMutations) -> Result<D> {
        self.inner.encode_entry_delta(mutations)
    }

    fn normalize_record_key(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.normalize_record_key(key)
    }

    fn staged_keys_conflict(&self, left: &[u8], right: &[u8]) -> bool {
        self.inner.staged_keys_conflict(left, right)
    }

    fn staged_key_shadows_cached(&self, staged_key: &[u8], cached_key: &[u8]) -> bool {
        self.inner.staged_key_shadows_cached(staged_key, cached_key)
    }

    fn staged_key_descends_from(&self, staged_key: &[u8], key: &[u8]) -> bool {
        self.inner.staged_key_descends_from(staged_key, key)
    }
}

pub mod state;
pub use crate::backend::ProjectionDescriptor;
pub use state::OPAQUE_STATE_KEY;

/// Store-owned representation of cached current state.
///
/// [`StoreStateModel::Opaque`] caches the complete state in one record.
/// [`StoreStateModel::Records`] stores individually addressable records derived
/// from canonical Store deltas.
pub enum StoreStateModel<D: CRDT + 'static> {
    /// Safe default: one opaque serialized whole-state record.
    Opaque {
        /// Record-format identity; see [`ProjectionDescriptor`].
        descriptor: ProjectionDescriptor,
        data: PhantomData<D>,
    },
    /// A Store-defined cached record format derived from canonical Store deltas.
    Records(Arc<dyn RecordProjection<D>>),
}

impl<D: CRDT + 'static> StoreStateModel<D> {
    pub fn opaque(name: impl Into<String>, version: u32) -> Self {
        Self::Opaque {
            descriptor: ProjectionDescriptor {
                name: name.into(),
                version,
            },
            data: PhantomData,
        }
    }

    pub fn descriptor(&self) -> ProjectionDescriptor {
        match self {
            Self::Opaque { descriptor, .. } => descriptor.clone(),
            Self::Records(projection) => projection.descriptor(),
        }
    }

    pub(crate) fn with_descriptor(self, descriptor: ProjectionDescriptor) -> Self {
        match self {
            Self::Opaque { .. } => Self::Opaque {
                descriptor,
                data: PhantomData,
            },
            Self::Records(inner) => {
                Self::Records(Arc::new(DescribedProjection { descriptor, inner }))
            }
        }
    }
}

mod errors;
pub use errors::StoreError;

mod docstore;
pub use docstore::{DocStore, DocStoreInit};

mod value_editor;
pub use value_editor::ValueEditor;

pub(crate) mod table;
pub use table::{Table, TableCursor, TablePage};

mod settings_store;
pub use settings_store::SettingsStore;

mod registry;
pub use registry::Registered;
pub use registry::Registry;
pub use registry::RegistryEntry;
pub use registry::SubtreeSettings;

mod password_store;
pub use password_store::{
    DEFAULT_ARGON2_M_COST, DEFAULT_ARGON2_P_COST, DEFAULT_ARGON2_T_COST, EncryptedFragment,
    EncryptionInfo, PasswordStore, PasswordStoreConfig,
};

#[cfg(feature = "y-crdt")]
mod ydoc;
#[cfg(feature = "y-crdt")]
pub use ydoc::{YDoc, YrsBinary};

/// A trait representing a named, CRDT-based data structure within a `Database`.
///
/// `Store` implementations define how data within a specific named partition of a `Database`
/// is structured, accessed, and modified. They work in conjunction with a `Transaction`
/// to stage changes before committing them as a single `Entry`.
///
/// Users typically interact with `Store` implementations obtained either via:
/// 1. `Database::get_store_viewer`: For read-only access to the current merged state.
/// 2. `Transaction::get_store`: For staging modifications within a transaction.
///
/// Store types must also implement [`Registered`] to provide their type identifier.
#[async_trait]
pub trait Store: Sized + Registered + Send + Sync {
    /// The CRDT data type used for local (staged) data in this store.
    ///
    /// This is the type stored within each individual Entry.
    type Data: CRDT + 'static;

    /// Representation of this store's cached current state.
    ///
    /// By default, the complete state is cached in one opaque record. A Store can
    /// instead return [`StoreStateModel::Records`] to define addressable records.
    fn state_model() -> StoreStateModel<Self::Data> {
        StoreStateModel::opaque("eidetica/opaque", 0)
    }

    /// Creates a new `Store` handle associated with a specific transaction.
    ///
    /// This constructor is typically called internally by `Transaction::get_store` or
    /// `Database::get_store_viewer`. The resulting `Store` instance provides methods
    /// to interact with the data of the specified `subtree_name`, potentially staging
    /// changes within the provided `txn`.
    ///
    /// # Arguments
    /// * `txn` - The `Transaction` this `Store` instance will read from and potentially write to.
    /// * `subtree_name` - The name identifying this specific data partition within the `Database`.
    async fn load(txn: &Transaction, subtree_name: String) -> Result<Self>;

    /// Returns the name of this subtree.
    fn name(&self) -> &str;

    /// Returns a reference to the transaction this Store is associated with.
    ///
    /// This is used by the default implementations of `register()`, `get_config()`,
    /// and `set_config()` to access the index store.
    fn transaction(&self) -> &Transaction;

    /// Returns the default configuration for this Store type as a [`Doc`].
    ///
    /// This configuration is stored in the `_index` subtree when a new subtree is
    /// first created. The Store implementation owns the format and interpretation
    /// of this configuration data.
    ///
    /// The default implementation returns an empty `Doc`. Store implementations
    /// that require specific configuration should override this method.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::{Store, store::DocStore};
    /// let config = DocStore::default_config();
    /// assert!(config.is_empty());
    /// ```
    fn default_config() -> Doc {
        Doc::new()
    }

    /// Initializes a new subtree and registers it in the `_index`.
    ///
    /// This method is called by `Transaction::get_store()` when accessing a subtree
    /// that doesn't yet exist in the `_index`. It creates the Store and registers
    /// its type and default configuration in the index.
    ///
    /// The default implementation:
    /// 1. Creates the Store using `Self::load()`
    /// 2. Registers it in `_index` with `Self::type_id()` and `Self::default_config()`
    ///
    /// Store implementations can override this to customize initialization behavior.
    ///
    /// # Arguments
    /// * `txn` - The `Transaction` this `Store` instance will operate within.
    /// * `subtree_name` - The name identifying this specific data partition.
    ///
    /// # Returns
    /// A `Result<Self>` containing the initialized Store.
    async fn register(txn: &Transaction, subtree_name: String) -> Result<Self> {
        let store = Self::load(txn, subtree_name).await?;
        store.set_config(Self::default_config()).await?;
        Ok(store)
    }

    /// Opens this Store on a transaction, registering the subtree if it does not
    /// yet exist.
    ///
    /// This is the consumer-facing entry point for attaching a Store to a
    /// transaction. The default implementation delegates to
    /// `Transaction::get_store`, which checks `_index` and dispatches to either
    /// [`Self::load`] (for an existing subtree) or [`Self::register`] (for a new
    /// one). Stores with construction needs that do not fit the load/register
    /// split — cross-subtree merge, external state, conditional initialization —
    /// may override this method directly.
    ///
    /// # Arguments
    /// * `txn` - The transaction this Store will operate within.
    /// * `name` - The subtree name identifying this data partition.
    ///
    /// # Returns
    /// A `Result<Self>` containing the opened Store.
    async fn open(txn: &Transaction, name: impl Into<String> + Send) -> Result<Self> {
        txn.get_store::<Self>(name).await
    }

    /// Gets the current configuration for this Store from the `_index` subtree.
    ///
    /// # Returns
    /// A `Result<Doc>` containing the configuration document.
    ///
    /// # Errors
    /// Returns an error if the subtree is not registered in `_index`.
    async fn get_config(&self) -> Result<Doc> {
        let index = self.transaction().get_index().await?;
        let info = index.get_entry(self.name()).await?;
        Ok(info.config)
    }

    /// Sets the configuration for this Store in the `_index` subtree.
    ///
    /// This method updates the `_index` with the Store's type ID and the provided
    /// configuration. It's called automatically by `register()` and can be used to
    /// update configuration during a transaction.
    ///
    /// # Arguments
    /// * `config` - The configuration document to store.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or failure.
    async fn set_config(&self, config: Doc) -> Result<()> {
        let index = self.transaction().get_index().await?;
        index
            .set_entry(self.name(), Self::type_id(), config)
            .await?;
        Ok(())
    }

    /// Gets the height strategy for this Store from the `_index` subtree.
    ///
    /// Returns `None` if no strategy is set (meaning the subtree inherits
    /// from the database-level height strategy).
    ///
    /// # Returns
    /// A `Result<Option<HeightStrategy>>` containing the strategy if set.
    ///
    /// # Errors
    /// Returns an error if the subtree is not registered in `_index`.
    async fn get_height_strategy(&self) -> Result<Option<HeightStrategy>> {
        let index = self.transaction().get_index().await?;
        let settings = index.get_subtree_settings(self.name()).await?;
        Ok(settings.height_strategy)
    }

    /// Sets the height strategy for this Store in the `_index` subtree.
    ///
    /// Pass `None` to inherit from the database-level strategy,
    /// or `Some(strategy)` for independent height calculation.
    ///
    /// # Arguments
    /// * `strategy` - The height strategy to use, or None for inheritance.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or failure.
    ///
    /// # Errors
    /// Returns an error if the subtree is not registered in `_index`.
    async fn set_height_strategy(&self, strategy: Option<HeightStrategy>) -> Result<()> {
        let index = self.transaction().get_index().await?;
        let mut settings = index.get_subtree_settings(self.name()).await?;
        settings.height_strategy = strategy;
        index.set_subtree_settings(self.name(), settings).await
    }

    /// Returns the local (staged) data for this store from the current transaction.
    ///
    /// This is a convenience method that retrieves data staged in the transaction
    /// for this store's subtree. Returns `Ok(None)` if no data has been staged.
    fn local_data(&self) -> Result<Option<Self::Data>> {
        self.transaction().get_local_data::<Self::Data>(self.name())
    }
}
