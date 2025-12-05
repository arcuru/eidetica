use crate::{Result, Transaction};

mod errors;
pub use errors::StoreError;

mod docstore;
pub use docstore::DocStore;

mod table;
pub use table::Table;

mod settings_store;
pub use settings_store::SettingsStore;

mod index_store;
pub(crate) use index_store::IndexStore;

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
/// 2. `Transaction::get_store`: For staging modifications within an atomic operation.
pub trait Store: Sized {
    /// Creates a new `Store` handle associated with a specific transaction.
    ///
    /// This constructor is typically called internally by `Transaction::get_store` or
    /// `Database::get_store_viewer`. The resulting `Store` instance provides methods
    /// to interact with the data of the specified `subtree_name`, potentially staging
    /// changes within the provided `op`.
    ///
    /// # Arguments
    /// * `op` - The `Transaction` this `Store` instance will read from and potentially write to.
    /// * `subtree_name` - The name identifying this specific data partition within the `Database`.
    fn new(op: &Transaction, subtree_name: impl Into<String>) -> Result<Self>;

    /// Returns the name of this subtree.
    fn name(&self) -> &str;

    /// Returns a reference to the transaction this Store is associated with.
    ///
    /// This is used by the default implementations of `init()`, `get_config()`,
    /// and `set_config()` to access the index store.
    fn transaction(&self) -> &Transaction;

    /// Returns a unique identifier for this Store type, including version information.
    ///
    /// This identifier is stored in the `_index` subtree to record what type of Store
    /// manages each subtree's data. The format should be `"storetype:vN"` where N is
    /// the version number (e.g., "docstore:v1", "table:v1", "ydoc:v1").
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::{Store, store::DocStore};
    /// assert_eq!(DocStore::type_id(), "docstore:v1");
    /// ```
    fn type_id() -> &'static str;

    /// Returns the default configuration for this Store type as a JSON string.
    ///
    /// This configuration is stored in the `_index` subtree when a new subtree is
    /// first created. The Store implementation owns the format and interpretation
    /// of this configuration data.
    ///
    /// The default implementation returns `"{}"` (empty JSON object). Store implementations
    /// that require specific configuration should override this method.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::{Store, store::DocStore};
    /// let config = DocStore::default_config();
    /// assert_eq!(config, "{}");
    /// ```
    fn default_config() -> String {
        "{}".to_string()
    }

    /// Initializes a new subtree and registers it in the `_index`.
    ///
    /// This method is called by `Transaction::get_store()` when accessing a subtree
    /// that doesn't yet exist in the `_index`. It creates the Store and registers
    /// its type and default configuration in the index.
    ///
    /// The default implementation:
    /// 1. Creates the Store using `Self::new()`
    /// 2. Registers it in `_index` with `Self::type_id()` and `Self::default_config()`
    ///
    /// Store implementations can override this to customize initialization behavior.
    ///
    /// # Arguments
    /// * `op` - The `Transaction` this `Store` instance will operate within.
    /// * `subtree_name` - The name identifying this specific data partition.
    ///
    /// # Returns
    /// A `Result<Self>` containing the initialized Store.
    fn init(op: &Transaction, subtree_name: impl Into<String>) -> Result<Self> {
        let name = subtree_name.into();
        let store = Self::new(op, name)?;
        store.set_config(Self::default_config())?;
        Ok(store)
    }

    /// Gets the current configuration for this Store from the `_index` subtree.
    ///
    /// # Returns
    /// A `Result<String>` containing the JSON configuration string.
    ///
    /// # Errors
    /// Returns an error if the subtree is not registered in `_index`.
    fn get_config(&self) -> Result<String> {
        let index = self.transaction().get_index_store()?;
        let info = index.get_subtree_info(self.name())?;
        Ok(info.config)
    }

    /// Sets the configuration for this Store in the `_index` subtree.
    ///
    /// This method updates the `_index` with the Store's type ID and the provided
    /// configuration. It's called automatically by `init()` and can be used to
    /// update configuration during a transaction.
    ///
    /// # Arguments
    /// * `config` - The JSON configuration string to store.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or failure.
    fn set_config(&self, config: impl Into<String>) -> Result<()> {
        let index = self.transaction().get_index_store()?;
        index.set_subtree_info(self.name(), Self::type_id(), config.into())?;
        Ok(())
    }
}
