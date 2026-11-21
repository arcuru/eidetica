use eidetica::{
    Account, AccountSession, Database, Entry, Error, ID, Instance, NewAccount, Result,
    account::{AccountError, AccountSigner, KeyStorage, SyncSettings},
    auth::{
        AuthKey, AuthSettings, Permission,
        crypto::{PrivateKey, PublicKey, verify_entry_signature},
        types::{
            DelegatedTreeRef, DelegationStep, KeyHint, PermissionBounds, SigKey, TreeReference,
        },
    },
    backend::VerificationStatus,
    crdt::Doc,
    database::DatabaseKey,
    store::DocStore,
};

use crate::helpers::{LocalBackendTestExt, test_local_instance};

fn grant(permission: Permission) -> AuthKey {
    AuthKey::active(None, permission)
}

fn bounds(max: Permission) -> PermissionBounds {
    PermissionBounds { max, min: None }
}

fn direct_auth(key: &PublicKey) -> Result<AuthSettings> {
    let mut auth = AuthSettings::new();
    auth.add_key(key, grant(Permission::Admin(0)))?;
    Ok(auth)
}

fn settings(auth: &AuthSettings) -> Doc {
    let mut settings = Doc::new();
    settings.set("auth", auth.as_doc().clone());
    settings
}

async fn connect(local: &Instance, url: Option<&str>) -> Result<Instance> {
    match url {
        Some(url) => Instance::connect(url).await,
        None => Ok(local.clone()),
    }
}

async fn write(db: &Database, value: &str) -> Result<Entry> {
    let tx = db.new_transaction().await?;
    tx.get_store::<DocStore>("example")
        .await?
        .set("written", value)
        .await?;
    let id = tx.commit().await?;
    db.get_entry(id).await
}

async fn transfer_verified(from: &Instance, to: &Instance, root: &ID) -> Result<()> {
    let source = from
        .backend()
        .local_engine()
        .unwrap()
        .get_tree(root)
        .await?;
    assert!(!source.is_empty());
    let ids: Vec<_> = source.iter().map(Entry::id).collect();
    for entry in source {
        to.backend().put(entry).await?;
    }
    Database::open(to, root).await?.verify().await?;
    for id in ids {
        assert_eq!(
            to.backend().get_verification_status(&id).await?,
            VerificationStatus::Verified
        );
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|bytes| bytes == needle)
}

fn entry_mentions(entry: &Entry, text: &str) -> bool {
    // JSON contains public SigKey fields; Doc payloads serialize as opaque
    // bytes in an Entry, so check those bytes separately.
    serde_json::to_string(entry).unwrap().contains(text)
        || entry
            .metadata()
            .is_some_and(|data| contains(data, text.as_bytes()))
        || entry.subtrees().iter().any(|store| {
            entry
                .data(store)
                .is_ok_and(|data| contains(data, text.as_bytes()))
        })
}

async fn exercise_owned_app(
    first_host: &Instance,
    second_host: &Instance,
    first_url: Option<&str>,
    second_url: Option<&str>,
) -> Result<()> {
    let personal_client = connect(first_host, first_url).await?;
    let mut person = personal_client.login_account("admin", "").await?;
    let personal_key = person.get_default_key()?;
    let owner_key = person
        .add_private_key(Some("chosen Identity member"))
        .await?;
    let owner = person.create_identity("owner", &owner_key).await?;
    let owner_grant = owner.as_delegation(bounds(Permission::Admin(0))).await?;
    // Reuse the same existing Identity on a group Account, with no extra
    // direct grant to the creator. The group is a sibling of the app.
    let mut group_auth = AuthSettings::new();
    group_auth.add_delegated_tree(owner_grant.clone())?;
    let owner_path = owner.delegation_key().await?;
    let group_key =
        DatabaseKey::with_identity(person.get_signing_key(&owner_key)?, owner_path.clone());
    let group = Account::create(
        &personal_client,
        group_key.clone(),
        "Working Group",
        group_auth,
    )
    .await?;
    let group_session = AccountSession::from_signer(
        &personal_client,
        AccountSigner::new(group.id().clone(), group_key),
    )
    .await?;
    group_session
        .set_reference("identities", "owner", owner.root_id())
        .await?;
    assert_eq!(
        group_session.identity_id("owner").await?,
        Some(owner.root_id().clone())
    );
    person
        .set_reference("accounts", "group", group.id())
        .await?;
    assert_eq!(group.get_name().await?, "Working Group");

    let admin = person.admin().await?;
    let app_id = admin
        .provision_owned_app(
            "notes",
            &owner,
            NewAccount::with_password("notes-local", "notes password"),
        )
        .await?;
    assert_eq!(
        person.reference("accounts", "notes").await?,
        Some(app_id.clone())
    );
    let roots = first_host
        .backend()
        .local_engine()
        .unwrap()
        .all_roots()
        .await?;
    assert!(
        admin
            .provision_owned_app("notes", &owner, NewAccount::passwordless("other"))
            .await
            .is_err()
    );
    assert!(
        admin
            .provision_owned_app("other", &owner, NewAccount::passwordless("notes-local"))
            .await
            .is_err()
    );
    assert_eq!(
        first_host
            .backend()
            .local_engine()
            .unwrap()
            .all_roots()
            .await?,
        roots
    );

    let app_client = connect(first_host, first_url).await?;
    assert!(
        app_client
            .login_account("notes-local", "wrong")
            .await
            .is_err()
    );
    let mut app = app_client
        .login_account("notes-local", "notes password")
        .await?;
    assert_eq!(app.account().id(), &app_id);
    assert_eq!(app.account().get_name().await?, "notes");
    assert_ne!(app.login_id(), person.login_id());
    let app_key = app.get_default_key()?;
    assert_eq!(app.list_keys()?, std::slice::from_ref(&app_key));
    let KeyStorage::Encrypted { bundle } = &app.account_info().unwrap().credentials.bundle;
    let unlocked = bundle.unlock_for(&app_id, "notes password")?;
    assert_eq!(unlocked.key_ids(), std::slice::from_ref(&app_key));
    assert_ne!(app_key, personal_key);
    assert!(app.get_signing_key(&owner_key).is_err());
    assert!(owner.keys().await?.get_key_by_pubkey(&app_key).is_err());
    assert!(app.admin().await.is_err());
    assert!(app.identity_id("owner").await?.is_none());
    assert!(app.reference("accounts", "parent").await?.is_none());
    let app_actor = app.acting_as(&app_key, None)?;
    for unrelated in [person.account().id(), group.id()] {
        assert!(
            app_actor
                .open_database(&app_client, unrelated)
                .await
                .is_err()
        );
    }
    let app_db = app_actor.open_database(&app_client, &app_id).await?;
    assert_eq!(app_db.current_permission().await?, Permission::Admin(20));
    let genesis = app_db.get_entry(&app_id).await?;
    verify_entry_signature(&genesis, &owner_key)?;
    assert_eq!(genesis.auth().key, owner_path);
    let owner_app = owner.open_database(&app_id).await?;
    assert_eq!(owner_app.auth_identity(), Some(&owner_path));
    // Ordinary Admin is useful, but cannot lower the stronger owner grant.
    let low_key = PrivateKey::generate().public_key();
    let tx = app_db.new_transaction().await?;
    tx.get_settings()?
        .set_auth_key(&low_key, grant(Permission::Write(20)))
        .await?;
    verify_entry_signature(&app_db.get_entry(tx.commit().await?).await?, &app_key)?;
    let mut reduced = owner_grant.clone();
    reduced.permission_bounds = bounds(Permission::Read);
    let tx = app_db.new_transaction().await?;
    tx.get_settings()?.add_delegated_tree(reduced).await?;
    let refused = tx.commit().await;
    assert!(refused.is_err(), "app lowered the stronger owner grant");
    assert_eq!(
        owner_app
            .get_settings()
            .await?
            .auth_snapshot()
            .await?
            .get_delegated_tree(owner.root_id())?,
        owner_grant
    );

    // One target gives the app key its own direct grant. The lower app key
    // authors genesis without gaining independent Admin(0).
    let mut auth = AuthSettings::new();
    auth.add_delegated_tree(owner_grant)?;
    auth.add_key(&app_key, grant(Permission::Admin(20)))?;
    let direct =
        Database::create(&app_client, app.get_signing_key(&app_key)?, settings(&auth)).await?;
    assert_eq!(direct.current_permission().await?, Permission::Admin(20));
    verify_entry_signature(&direct.get_entry(direct.root_id()).await?, &app_key)?;
    verify_entry_signature(&write(&direct, "direct").await?, &app_key)?;

    // A second target explicitly delegates to the app Account. Pin the
    // exact target path and prove it, rather than try the reference as Auth.
    let tips = app_db.snapshot().await?.into_tips();
    let mut delegated_auth = AuthSettings::new();
    delegated_auth.add_delegated_tree(DelegatedTreeRef {
        tree: TreeReference {
            root: app_id.clone(),
            tips: tips.clone(),
        },
        permission_bounds: bounds(Permission::Admin(20)),
    })?;
    let app_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: app_id.clone(),
            tips,
        }],
        hint: KeyHint::from_pubkey(&app_key),
    };
    let delegated = Database::create(
        &app_client,
        DatabaseKey::with_identity(app.get_signing_key(&app_key)?, app_path.clone()),
        settings(&delegated_auth),
    )
    .await?;
    app.set_reference("notes", "direct", direct.root_id())
        .await?;
    app.set_reference("notes", "delegated", delegated.root_id())
        .await?;
    assert!(
        app.open_reference("notes", "direct").await.is_err(),
        "reference installed a mapping"
    );
    assert!(
        app_actor
            .open_database(&app_client, delegated.root_id())
            .await
            .is_err()
    );
    let act = app.acting_as(&app_key, Some(app_path.clone()))?;
    assert!(
        act.open_database(&app_client, direct.root_id())
            .await
            .is_err(),
        "wrong path fell back to a direct grant"
    );
    let data_handle = act.open_database(&app_client, delegated.root_id()).await?;
    let entry = write(&data_handle, "delegated").await?;
    assert_eq!(entry.auth().key, app_path);
    verify_entry_signature(&entry, &app_key)?;
    // Selecting another path left the original direct handle intact.
    verify_entry_signature(&write(&app_db, "still direct").await?, &app_key)?;
    assert_eq!(app_db.auth_identity(), Some(app_actor.identity()));

    // Device enrollment belongs to the ticket workflow. This fixture preloads
    // only independently verified Entries, then uses a locally generated key
    // and the real existing-root registration path on the other installation.
    let second_key = PrivateKey::generate();
    let second_id = second_key.public_key();
    let tx = owner_app.new_transaction().await?;
    tx.get_settings()?
        .set_auth_key(&second_id, grant(Permission::Admin(20)))
        .await?;
    verify_entry_signature(&owner_app.get_entry(tx.commit().await?).await?, &owner_key)?;
    for root in [
        owner.root_id(),
        &app_id,
        direct.root_id(),
        delegated.root_id(),
    ] {
        transfer_verified(first_host, second_host, root).await?;
    }
    let second_admin_client = connect(second_host, second_url).await?;
    let second_admin = second_admin_client.login_account("admin", "").await?;
    assert!(
        second_admin
            .admin()
            .await?
            .register_account_login(
                &app_id,
                PrivateKey::generate(),
                NewAccount::passwordless("ungranted")
            )
            .await
            .is_err()
    );
    second_admin
        .admin()
        .await?
        .register_account_login(
            &app_id,
            second_key,
            NewAccount::with_password("second-notes", "second password"),
        )
        .await?;
    let second_client = connect(second_host, second_url).await?;
    let second = second_client
        .login_account("second-notes", "second password")
        .await?;
    assert_eq!(second.account().id(), &app_id);
    assert_eq!(second.list_keys()?, std::slice::from_ref(&second_id));
    assert_eq!(
        second.reference("notes", "direct").await?,
        Some(direct.root_id().clone())
    );
    let second_actor = second.acting_as(&second_id, None)?;
    assert!(
        second_actor
            .open_database(&second_client, direct.root_id())
            .await
            .is_err(),
        "app Account grant leaked to direct target"
    );
    let second_app = second_actor.open_database(&second_client, &app_id).await?;
    let second_path = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: app_id.clone(),
            tips: second_app.snapshot().await?.into_tips(),
        }],
        hint: KeyHint::from_pubkey(&second_id),
    };
    let second_target = second
        .acting_as(&second_id, Some(second_path.clone()))?
        .open_database(&second_client, delegated.root_id())
        .await?;
    let entry = write(&second_target, "second installation").await?;
    assert_eq!(entry.auth().key, second_path);
    verify_entry_signature(&entry, &second_id)?;
    assert!(app.get_signing_key(&second_id).is_err());

    let engine = first_host.backend().local_engine().unwrap();
    let history = engine.get_tree(&app_id).await?;
    let encoded_key = app.get_signing_key(&app_key)?.to_prefixed_string();
    assert!(
        history
            .iter()
            .any(|entry| entry_mentions(entry, &app_key.to_string())),
        "public-key positive control"
    );
    for entry in history {
        assert!(!entry_mentions(&entry, &encoded_key));
        assert!(!entry_mentions(&entry, "notes password"));
        assert!(!entry_mentions(&entry, &person.account().id().to_string()));
        assert!(!entry.in_subtree("keys") && !entry.in_subtree("databases"));
    }

    // Local forget is not unlink or revoke; the mapping may be retained.
    app.track_database(direct.root_id(), &app_key, SyncSettings::disabled())
        .await?;
    app.untrack_database(direct.root_id()).await?;
    assert!(app.tracked_database(direct.root_id()).await?.is_none());
    assert!(app.reference("notes", "direct").await?.is_some());
    assert!(person.remove_reference("accounts", "notes").await?);
    assert!(!person.remove_reference("accounts", "notes").await?);
    assert_eq!(person.reference("accounts", "notes").await?, None);
    app.logout()?;
    let returning = app_client
        .login_account("notes-local", "notes password")
        .await?;
    assert_eq!(returning.account().id(), &app_id);
    assert_eq!(returning.get_default_key()?, app_key);
    // Actual owner revocation is separate from removing an organizational link.
    let tx = owner_app.new_transaction().await?;
    tx.get_settings()?.revoke_auth_key(&app_key).await?;
    verify_entry_signature(&owner_app.get_entry(tx.commit().await?).await?, &owner_key)?;
    assert!(write(&app_db, "revoked").await.is_err());
    Ok(())
}

#[tokio::test]
async fn test_owned_app_embedded() -> Result<()> {
    let first = test_local_instance().await;
    let second = test_local_instance().await;
    exercise_owned_app(&first, &second, None, None).await
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn test_owned_app_service() -> Result<()> {
    use eidetica::service::ServiceServer;
    use std::os::unix::fs::PermissionsExt;

    let first = test_local_instance().await;
    let second = test_local_instance().await;
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let path_a = dir.path().join("first.sock");
    let path_b = dir.path().join("second.sock");
    let a = ServiceServer::bind(first.clone(), &path_a).await?;
    let b = ServiceServer::bind(second.clone(), &path_b).await?;
    let (shutdown, rx) = tokio::sync::watch::channel(());
    let a = tokio::spawn(a.run(rx.clone()));
    let b = tokio::spawn(b.run(rx));
    let result = exercise_owned_app(
        &first,
        &second,
        Some(&format!("unix://{}", path_a.display())),
        Some(&format!("unix://{}", path_b.display())),
    )
    .await;
    drop(shutdown);
    a.await.unwrap()?;
    b.await.unwrap()?;
    result
}

#[tokio::test]
async fn test_owned_app_link_failure_keeps_the_created_root_and_login() -> Result<()> {
    let instance = test_local_instance().await;
    let mut person = instance.login_account("admin", "").await?;
    let personal_key = person.get_default_key()?;
    let owner = person.create_identity("owner", &personal_key).await?;
    person
        .set_reference("accounts", "keep", owner.root_id())
        .await?;
    // An instance admin may only be a reader of its own Account. Creating and
    // registering a new root still works, but publishing the parent link must fail.
    let tx = person.account().database().new_transaction().await?;
    tx.get_settings()?
        .set_auth_key(&personal_key, grant(Permission::Read))
        .await?;
    tx.commit().await?;

    let error = person
        .admin()
        .await?
        .provision_owned_app(
            "notes",
            &owner,
            NewAccount::with_password("notes-local", "notes password"),
        )
        .await
        .expect_err("a reader cannot publish a reference");
    let root = error
        .created_database_id()
        .expect("return the durable app root");
    assert!(matches!(&error, Error::DatabaseCreation { source, .. }
        if matches!(source.as_ref(), Error::Account(e)
            if matches!(e.as_ref(), AccountError::OwnedAppSetup { step: "Account link", .. }))));
    assert_eq!(person.reference("accounts", "notes").await?, None);
    assert_eq!(
        person.reference("accounts", "keep").await?,
        Some(owner.root_id().clone())
    );
    let app = instance
        .login_account("notes-local", "notes password")
        .await?;
    assert_eq!(app.account().id(), root);
    assert_eq!(
        owner
            .open_database(root)
            .await?
            .current_permission()
            .await?,
        Permission::Admin(0)
    );
    Ok(())
}

#[tokio::test]
async fn test_linked_independent_persona_verifies_without_private_organizer() -> Result<()> {
    let instance = test_local_instance().await;
    let organizer = instance.login_account("admin", "").await?;
    let organizer_id = organizer.account().id();
    let key_id = organizer.get_default_key()?;
    let organizer_actor = organizer.acting_as(&key_id, None)?;

    let persona_key = PrivateKey::generate();
    let persona_id = persona_key.public_key();
    let auth = direct_auth(&persona_id)?;
    assert!(
        Account::create(
            &instance,
            persona_key.clone(),
            "No Auth",
            AuthSettings::new()
        )
        .await
        .is_err()
    );
    let persona = Account::create(&instance, persona_key.clone(), "Independent", auth).await?;
    let mut persona_session = AccountSession::from_signer(
        &instance,
        AccountSigner::new(persona.id().clone(), persona_key.clone()),
    )
    .await?;
    assert_eq!(persona_session.identity_id("public").await?, None);
    let identity = persona_session
        .create_identity("public", &persona_id)
        .await?;
    organizer
        .set_reference("accounts", "independent", persona.id())
        .await?;
    organizer
        .set_reference("identities", "public", identity.root_id())
        .await?;
    assert!(organizer.get_identity("public").await?.is_none());
    assert!(
        organizer_actor
            .open_database(&instance, persona.id())
            .await
            .is_err()
    );

    let mut auth = AuthSettings::new();
    auth.set_global_permission(grant(Permission::Read));
    auth.add_delegated_tree(identity.as_delegation(bounds(Permission::Admin(0))).await?)?;
    let path = identity.delegation_key().await?;
    let public = Database::create(
        &instance,
        DatabaseKey::with_identity(persona_key, path.clone()),
        settings(&auth),
    )
    .await?;
    let signed = write(&public, "independent history").await?;
    assert_eq!(signed.auth().key, path);
    verify_entry_signature(&signed, &persona_id)?;
    let read_only = organizer_actor
        .open_database(&instance, public.root_id())
        .await?;
    assert_eq!(read_only.current_permission().await?, Permission::Read);
    let admin = organizer.admin().await?;
    // Public Read is a working route, but it is not an installation key grant.
    assert!(
        admin
            .register_account_login(
                public.root_id(),
                organizer.get_signing_key(&key_id)?,
                NewAccount::passwordless("public-reader")
            )
            .await
            .is_err()
    );
    assert_eq!(admin.list_accounts().await?, ["admin"]);
    assert!(
        write(&read_only, "organizer cannot sign for persona")
            .await
            .is_err()
    );

    let observer = test_local_instance().await;
    let engine = instance.backend().local_engine().unwrap();
    let private = engine.get_tree(organizer_id).await?;
    assert!(
        private
            .iter()
            .any(|e| entry_mentions(e, &key_id.to_string()))
    );
    assert!(
        private
            .iter()
            .any(|e| entry_mentions(e, &identity.root_id().to_string()))
    );
    for root in [identity.root_id(), public.root_id()] {
        transfer_verified(&instance, &observer, root).await?;
        for entry in engine.get_tree(root).await? {
            assert!(!entry_mentions(&entry, &organizer_id.to_string()));
            assert!(!entry_mentions(&entry, &key_id.to_string()));
            assert!(!entry_mentions(&entry, &persona.id().to_string()));
        }
    }
    assert!(observer.backend().get(organizer_id).await.is_err());
    assert_eq!(
        Database::open(&observer, public.root_id())
            .await?
            .get_store_viewer::<DocStore>("example")
            .await?
            .get_string("written")
            .await?,
        "independent history"
    );
    Ok(())
}
