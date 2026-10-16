//! Bounded native provenance probe, not a Provider protocol implementation.
//! All databases/keys are disposable; the only listener is a temporary Unix socket.
//! The service parity assertion intentionally exposes a pinned Eidetica API gap.

use eidetica::{
    Database, Entry, Instance, NewUser,
    auth::{
        crypto::{PrivateKey, verify_entry_signature},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    backend::database::InMemory,
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    service::{ServiceServer, protocol::ReadScope},
    store::DocStore,
};
use tokio::sync::watch;

async fn publish(db: &Database, key: DatabaseKey, provider: &ID) -> eidetica::Result<ID> {
    let tx = db.clone().with_key(key).new_transaction().await?;
    let store = tx.get_store::<DocStore>("records").await?;
    // This label is data, not proof of who wrote it.
    store.set("provider", provider.to_string()).await?;
    tx.commit().await
}

async fn records(db: &Database) -> eidetica::Result<Vec<Entry>> {
    let frontier = db.snapshot().await?;
    db.get_store_entries("records", frontier.tips(), ReadScope::Verified)
        .await
}

#[tokio::test]
async fn native_provider_service_parity() -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(30), proof()).await??;
    Ok(())
}

async fn proof() -> Result<(), Box<dyn std::error::Error>> {
    let (owner, mut user) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("proof")).await?;
    let admin = user.get_default_key()?;
    let provider = user.create_database(Doc::new(), &admin).await?;
    let session = user.create_database(Doc::new(), &admin).await?;
    let direct = PrivateKey::generate();
    let delegated = PrivateKey::generate();
    let forger = PrivateKey::generate();

    let tx = provider.new_transaction().await?;
    for key in [&direct, &delegated] {
        tx.get_settings()?
            .set_auth_key(
                &key.public_key(),
                AuthKey::active(None, Permission::Write(10)),
            )
            .await?;
    }
    tx.commit().await?;
    let provider_tips = provider.snapshot().await?.into_tips();
    let tx = session.new_transaction().await?;
    for key in [&direct, &forger] {
        tx.get_settings()?
            .set_auth_key(
                &key.public_key(),
                AuthKey::active(None, Permission::Write(10)),
            )
            .await?;
    }
    tx.get_settings()?
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: None,
            },
            tree: TreeReference {
                root: provider.root_id().clone(),
                tips: provider_tips.clone(),
            },
        })
        .await?;
    tx.commit().await?;

    let direct_id = publish(
        &session,
        DatabaseKey::new(direct.clone()),
        provider.root_id(),
    )
    .await?;
    let forged_id = publish(&session, DatabaseKey::new(forger), provider.root_id()).await?;
    let identity = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: provider.root_id().clone(),
            tips: provider_tips,
        }],
        hint: KeyHint::from_pubkey(&delegated.public_key()),
    };
    let delegated_id = publish(
        &session,
        DatabaseKey::with_identity(delegated.clone(), identity),
        provider.root_id(),
    )
    .await?;

    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("proof.sock");
    let service = ServiceServer::bind(owner.clone(), &socket).await?;
    let (shutdown, receiver) = watch::channel(());
    let running = tokio::spawn(service.run(receiver));
    let remote = Instance::connect(format!("unix://{}", socket.display())).await?;
    let _login = remote.login_user("proof", None).await?;
    let remote_session = Database::open(&remote, session.root_id()).await?;

    // The same server-verified Entry bytes reach both readers. No callback tips
    // or plain provider/sender field is used as the authorization boundary.
    let local_records = records(&session).await?;
    let remote_records = records(&remote_session).await?;
    assert_eq!(
        serde_json::to_value(&local_records)?,
        serde_json::to_value(&remote_records)?
    );
    assert_eq!(local_records.len(), 3);
    for db in [&session, &remote_session] {
        let direct_entry = db.get_entry(&direct_id).await?;
        assert!(db.verify_entry_signature(&direct_id).await?);
        assert!(verify_entry_signature(&direct_entry, &direct.public_key()).is_ok());
        let provider_auth = provider.get_settings().await?.auth_snapshot().await?;
        assert!(
            provider_auth
                .get_key_by_pubkey(&direct.public_key())?
                .permissions()
                .can_write()
        );
        let forged = db.get_entry(&forged_id).await?;
        assert!(
            db.verify_entry_signature(&forged_id).await?,
            "forger is a valid session writer"
        );
        assert!(verify_entry_signature(&forged, &direct.public_key()).is_err());
        assert!(verify_entry_signature(&forged, &delegated.public_key()).is_err());
    }
    println!(
        "PROVED: verified-frontier direct/service entry parity, direct-key Provider association, valid-writer forged-label rejection"
    );

    // Native delegation resolves to an actual operating key, not a trusted hint.
    let entry = session.get_entry(&delegated_id).await?;
    assert!(session.verify_entry_signature(&delegated_id).await?);
    let auth = session.get_settings().await?.auth_snapshot().await?;
    let candidates = AuthValidator::new()
        .resolve_sig_key(&entry.auth().key, &auth, Some(&owner))
        .await?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].public_key, delegated.public_key());
    assert!(candidates[0].effective_permission.can_write());
    assert!(verify_entry_signature(&entry, &candidates[0].public_key).is_ok());
    println!(
        "PROVED: direct native delegated identity resolves and authenticates the Provider operating key"
    );

    // Required parity: this must return true, not panic or rely on a new verifier.
    // At the locked revision DelegationResolver expects a local backend here.
    println!("CHECK: Database::verify_entry_signature on the same delegated Entry via service");
    let verification =
        tokio::spawn(async move { remote_session.verify_entry_signature(delegated_id).await })
            .await;
    drop(shutdown);
    running.await??;
    assert!(
        verification??,
        "delegated signature must verify over the service"
    );
    Ok(())
}
