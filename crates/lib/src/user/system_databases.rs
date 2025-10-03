//! System database initialization for the user system
//!
//! Creates and manages _users and _databases system databases.

use std::sync::Arc;

use crate::{
    Database, Result,
    auth::{
        settings::AuthSettings,
        types::{AuthKey, Permission},
    },
    backend::BackendDB,
    constants::{DATABASES, INSTANCE, USERS},
    crdt::Doc,
};

/// Create the _instance system database
///
/// This database stores Instance-level configuration and metadata.
/// It is authenticated with the Instance's _device_key.
///
/// # Arguments
/// * `backend` - The database backend
/// * `device_pubkey` - The public key for _device_key (used as admin)
///
/// # Returns
/// The _instance Database
pub fn create_instance_database(
    backend: Arc<dyn BackendDB>,
    device_pubkey: &str,
) -> Result<Database> {
    // Create database settings
    let mut settings = Doc::new();
    settings.set_string("name", INSTANCE);
    settings.set_string("type", "system");
    settings.set_string("description", "Instance configuration and management");

    // Set up auth with device key as admin
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(device_pubkey, Permission::Admin(0))?,
    )?;
    settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the database
    let database = Database::new(settings, backend, "_device_key")?;

    Ok(database)
}

/// Create the _users system database
///
/// This database stores the user directory mapping user_id → UserInfo.
/// It is authenticated with the Instance's _device_key.
///
/// # Arguments
/// * `backend` - The database backend
/// * `device_pubkey` - The public key for _device_key (used as admin)
///
/// # Returns
/// The created _users Database
pub fn create_users_database(backend: Arc<dyn BackendDB>, device_pubkey: &str) -> Result<Database> {
    // Create settings for _users database
    let mut settings = Doc::new();
    settings.set_string("name", USERS);
    settings.set_string("type", "system");
    settings.set_string("description", "User directory database");

    // Create auth settings with device key as admin
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(device_pubkey, Permission::Admin(0))?,
    )?;

    settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the database authenticated as _device_key
    let database = Database::new(settings, backend, "_device_key")?;

    Ok(database)
}

/// Create the _databases tracking database
///
/// This database stores the database tracking information mapping
/// database_id → DatabaseTracking.
/// It is authenticated with the Instance's _device_key.
///
/// # Arguments
/// * `backend` - The database backend
/// * `device_pubkey` - The public key for _device_key (used as admin)
///
/// # Returns
/// The created _databases Database
pub fn create_databases_tracking(
    backend: Arc<dyn BackendDB>,
    device_pubkey: &str,
) -> Result<Database> {
    // Create settings for _databases database
    let mut settings = Doc::new();
    settings.set_string("name", DATABASES);
    settings.set_string("type", "system");
    settings.set_string("description", "Database tracking and registry");

    // Create auth settings with device key as admin
    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        "_device_key",
        AuthKey::active(device_pubkey, Permission::Admin(0))?,
    )?;

    settings.set_doc("auth", auth_settings.as_doc().clone());

    // Create the database authenticated as _device_key
    let database = Database::new(settings, backend, "_device_key")?;

    Ok(database)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::crypto::{format_public_key, generate_keypair};
    use crate::backend::database::InMemory;
    use crate::store::DocStore;
    use crate::store::SettingsStore;

    #[test]
    fn test_create_instance_database() {
        let backend = Arc::new(InMemory::new());

        // Generate device key and store it in backend
        let (device_key, device_pubkey) = generate_keypair();
        let pubkey_str = format_public_key(&device_pubkey);
        backend
            .store_private_key("_device_key", device_key)
            .unwrap();

        let instance_db = create_instance_database(backend.clone(), &pubkey_str).unwrap();

        // Verify database was created
        assert!(!instance_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = instance_db.new_transaction().unwrap();
        let doc_store = transaction.get_store::<DocStore>("_settings").unwrap();
        let name = doc_store.get_string("name").unwrap();
        assert_eq!(name, INSTANCE);

        // Verify auth settings
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.get_auth_settings().unwrap();
        let device_key = auth_settings.get_key("_device_key").unwrap();
        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.pubkey(), &pubkey_str);
    }

    #[test]
    fn test_create_users_database() {
        let backend = Arc::new(InMemory::new());

        // Generate device key and store it in backend
        let (device_key, device_pubkey) = generate_keypair();
        let pubkey_str = format_public_key(&device_pubkey);
        backend
            .store_private_key("_device_key", device_key)
            .unwrap();

        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Verify database was created
        assert!(!users_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = users_db.new_transaction().unwrap();
        let doc_store = transaction.get_store::<DocStore>("_settings").unwrap();
        let name = doc_store.get_string("name").unwrap();
        assert_eq!(name, USERS);
    }

    #[test]
    fn test_create_databases_tracking() {
        let backend = Arc::new(InMemory::new());

        // Generate device key and store it in backend
        let (device_key, device_pubkey) = generate_keypair();
        let pubkey_str = format_public_key(&device_pubkey);
        backend
            .store_private_key("_device_key", device_key)
            .unwrap();

        let databases_db = create_databases_tracking(backend.clone(), &pubkey_str).unwrap();

        // Verify database was created
        assert!(!databases_db.root_id().to_string().is_empty());

        // Verify settings
        let transaction = databases_db.new_transaction().unwrap();
        let doc_store = transaction.get_store::<DocStore>("_settings").unwrap();
        let name = doc_store.get_string("name").unwrap();
        assert_eq!(name, DATABASES);
    }

    #[test]
    fn test_system_databases_have_device_key_auth() {
        let backend = Arc::new(InMemory::new());

        // Generate device key and store it in backend
        let (device_key, device_pubkey) = generate_keypair();
        let pubkey_str = format_public_key(&device_pubkey);
        backend
            .store_private_key("_device_key", device_key)
            .unwrap();

        let users_db = create_users_database(backend.clone(), &pubkey_str).unwrap();

        // Verify _device_key has admin access
        let transaction = users_db.new_transaction().unwrap();
        let settings_store = SettingsStore::new(&transaction).unwrap();
        let auth_settings = settings_store.get_auth_settings().unwrap();
        let device_key = auth_settings.get_key("_device_key").unwrap();

        assert_eq!(device_key.permissions(), &Permission::Admin(0));
        assert_eq!(device_key.pubkey(), &pubkey_str);
    }
}
