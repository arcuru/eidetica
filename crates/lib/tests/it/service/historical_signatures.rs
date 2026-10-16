//! Historical authentication and operating-key attribution across the service.
//! Labels and key hints are never treated as authenticated signer identities.

use eidetica::{
    Database, Entry, Instance, NewUser,
    auth::{
        crypto::{PrivateKey, PublicKey, sign_entry, verify_entry_signature},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, KeyStatus, Permission,
            PermissionBounds, SigKey, TreeReference,
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

fn resign(entry: Entry, identity: SigKey, key: &PrivateKey) -> eidetica::Result<Entry> {
    let entry = entry.with_auth(|auth| auth.key = identity);
    let signature = sign_entry(&entry, key)?;
    Ok(entry.with_auth(|auth| auth.signature = Some(signature)))
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
    let reader = PrivateKey::generate();
    let target_only = PrivateKey::generate();
    user.admin()
        .await?
        .create_user(NewUser::passwordless("reader"))
        .await?;

    let tx = provider.new_transaction().await?;
    for (key, permission) in [
        (&direct, Permission::Write(10)),
        (&delegated, Permission::Admin(10)),
    ] {
        tx.get_settings()?
            .set_auth_key(
                &key.public_key(),
                AuthKey::active(Some("operator"), permission),
            )
            .await?;
    }
    tx.get_settings()?
        .set_auth_key(
            &reader.public_key(),
            AuthKey::active(None, Permission::Read),
        )
        .await?;
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
    for key in [&reader, &target_only] {
        tx.get_settings()?
            .set_auth_key(&key.public_key(), AuthKey::active(None, Permission::Read))
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
    let tx = session.new_transaction().await?;
    tx.get_settings()?
        .set_name("signed settings operation")
        .await?;
    let settings_id = tx.commit().await?;

    let direct_id = publish(
        &session,
        DatabaseKey::new(direct.clone()),
        provider.root_id(),
    )
    .await?;
    let forged_id = publish(
        &session,
        DatabaseKey::new(forger.clone()),
        provider.root_id(),
    )
    .await?;
    let identity = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: provider.root_id().clone(),
            tips: provider_tips.clone(),
        }],
        // Both operating keys share this name. Only signature verification
        // can distinguish the actual signer from a matching name hint.
        hint: KeyHint::from_name("operator"),
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
    assert!(
        remote
            .remote_connection()
            .unwrap()
            .verify_entry_signature(
                session.root_id().clone(),
                SigKey::default(),
                delegated_id.clone()
            )
            .await
            .is_err(),
        "login is required even for verification queries"
    );
    let login = remote.login_user("reader", None).await?;
    assert!(login.admin().await.is_err(), "reader is not a host admin");
    assert!(
        Database::open(&remote, session.root_id()).await.is_err(),
        "login identity has no target grant"
    );
    // Creation is a native proof-of-possession registration path. Neither
    // registration nor owning this unrelated tree grants Read on the target.
    Database::create(&remote, reader.clone(), Doc::new()).await?;
    Database::create(&remote, target_only.clone(), Doc::new()).await?;
    let conn = remote.remote_connection().unwrap();
    let remote_session = Database::open_remote(
        &remote,
        conn.clone(),
        session.root_id(),
        SigKey::from_pubkey(&reader.public_key()),
    )
    .await?;
    let remote_provider = Database::open_remote(
        &remote,
        conn.clone(),
        provider.root_id(),
        SigKey::from_pubkey(&reader.public_key()),
    )
    .await?;
    let target_only_session = Database::open_remote(
        &remote,
        conn.clone(),
        session.root_id(),
        SigKey::from_pubkey(&target_only.public_key()),
    )
    .await?;
    assert!(
        target_only_session
            .verify_entry_signature(&direct_id)
            .await?
    );
    assert!(
        target_only_session
            .verify_entry_signature(&delegated_id)
            .await
            .is_err(),
        "a target-only reader cannot read delegated proof"
    );
    assert!(
        Database::open_remote(
            &remote,
            conn.clone(),
            provider.root_id(),
            SigKey::from_pubkey(&target_only.public_key())
        )
        .await
        .is_err()
    );
    assert!(
        conn.verify_entry_signature(
            session.root_id().clone(),
            SigKey::from_pubkey(&PrivateKey::generate().public_key()),
            delegated_id.clone()
        )
        .await
        .is_err(),
        "unregistered identity cannot request a verdict"
    );
    assert!(
        conn.verify_entry_signature(
            session.root_id().clone(),
            SigKey::from_pubkey(&reader.public_key()),
            provider.root_id().clone()
        )
        .await
        .is_err(),
        "a target envelope cannot query another database's entry"
    );

    // The same server-verified Entry bytes reach both readers. No callback tips
    // or plain provider/sender field is used as the authorization boundary.
    let local_records = records(&session).await?;
    let remote_records = records(&remote_session).await?;
    assert_eq!(
        serde_json::to_value(&local_records)?,
        serde_json::to_value(&remote_records)?
    );
    assert_eq!(local_records.len(), 3);
    for (db, authority) in [(&session, &provider), (&remote_session, &remote_provider)] {
        let direct_entry = db.get_entry(&direct_id).await?;
        assert!(db.verify_entry_signature(&direct_id).await?);
        assert!(verify_entry_signature(&direct_entry, &direct.public_key()).is_ok());
        let provider_auth = authority.get_settings().await?.auth_snapshot().await?;
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
        assert!(db.verify_entry_signature(&delegated_id).await?);
        let delegated_entry = db.get_entry(&delegated_id).await?;
        // Native historical validation establishes authorization. Separately
        // verify native Entry bytes with concrete, authorized operating keys:
        // neither the record label, hint, nor frontier inclusion identifies one.
        let signers = provider_auth
            .get_all_keys()?
            .into_iter()
            .filter_map(|(key, auth)| {
                let public = PublicKey::from_prefixed_string(&key).ok()?;
                (auth.is_active()
                    && auth.permissions().can_write()
                    && verify_entry_signature(&delegated_entry, &public).is_ok())
                .then_some(public)
            })
            .collect::<Vec<_>>();
        assert_eq!(signers, vec![delegated.public_key()]);
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
    assert_eq!(candidates.len(), 2);
    let signer = candidates
        .iter()
        .find(|candidate| verify_entry_signature(&entry, &candidate.public_key).is_ok())
        .unwrap();
    assert_eq!(signer.public_key, delegated.public_key());
    assert_eq!(signer.effective_permission, Permission::Write(10));
    println!(
        "PROVED: direct native delegated identity resolves and authenticates the Provider operating key"
    );

    // Keep the original panic regression isolated in a joined task.
    println!("CHECK: Database::verify_entry_signature on the same delegated Entry via service");
    let verification = tokio::spawn({
        let db = remote_session.clone();
        let id = delegated_id.clone();
        async move { db.verify_entry_signature(id).await }
    })
    .await;
    assert!(
        verification??,
        "delegated signature must verify over the service"
    );

    // Freshly signed malformed/unauthorized claims cannot borrow the original
    // Entry's valid signature or its historical authority.
    let outsider = PrivateKey::generate();
    let direct_entry = session.get_entry(&direct_id).await?;
    let attenuated_settings = resign(
        session.get_entry(&settings_id).await?,
        entry.auth().key.clone(),
        &delegated,
    )?;
    assert!(verify_entry_signature(&attenuated_settings, &delegated.public_key()).is_ok());
    let mut invalid = vec![
        attenuated_settings,
        entry
            .clone()
            .with_auth(|auth| auth.signature = Some(sign_entry(&entry, &outsider).unwrap())),
        resign(
            direct_entry.clone(),
            SigKey::from_pubkey(&outsider.public_key()),
            &outsider,
        )?,
        resign(
            direct_entry.clone(),
            SigKey::from_pubkey(&reader.public_key()),
            &reader,
        )?,
        resign(
            entry.clone(),
            SigKey::Delegation {
                path: vec![],
                hint: KeyHint::from_pubkey(&delegated.public_key()),
            },
            &delegated,
        )?,
    ];
    for tips in [
        vec![],
        vec![session.root_id().clone()],
        vec![provider.root_id().clone()],
        vec![provider_tips[0].clone(); 65],
    ] {
        invalid.push(resign(
            entry.clone(),
            SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: provider.root_id().clone(),
                    tips,
                }],
                hint: KeyHint::from_pubkey(&delegated.public_key()),
            },
            &delegated,
        )?);
    }
    invalid.push(resign(
        entry.clone(),
        SigKey::Delegation {
            path: vec![
                DelegationStep {
                    tree: provider.root_id().clone(),
                    tips: provider_tips.clone()
                };
                11
            ],
            hint: KeyHint::from_pubkey(&delegated.public_key()),
        },
        &delegated,
    )?);
    invalid.push(resign(
        entry.clone(),
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: provider.root_id().clone(),
                tips: provider_tips.clone(),
            }],
            hint: KeyHint::from_pubkey(&forger.public_key()),
        },
        &delegated,
    )?);
    for bad in invalid {
        let id = bad.id();
        owner.backend().put(bad).await?;
        assert!(
            !session.verify_entry_signature(&id).await?,
            "local invalid {id}"
        );
        assert!(
            !remote_session.verify_entry_signature(&id).await?,
            "service invalid {id}"
        );
    }
    let incomplete = resign(
        entry.clone(),
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: provider.root_id().clone(),
                tips: vec![ID::from_bytes("missing-delegated-tip")],
            }],
            hint: KeyHint::from_pubkey(&delegated.public_key()),
        },
        &delegated,
    )?;
    let incomplete_id = incomplete.id();
    owner.backend().put(incomplete).await?;
    assert!(
        session
            .verify_entry_signature(&incomplete_id)
            .await
            .is_err()
    );
    assert!(
        remote_session
            .verify_entry_signature(&incomplete_id)
            .await
            .is_err()
    );

    assert_eq!(
        owner
            .backend()
            .local_engine()
            .unwrap()
            .get_verification_status(&incomplete_id)
            .await?,
        eidetica::backend::VerificationStatus::Unverified
    );
    assert!(
        AuthValidator::new()
            .resolve_sig_key(&entry.auth().key, &auth, Some(&remote))
            .await
            .is_err(),
        "low-level local-evidence resolver refuses a remote instance without panicking"
    );

    // An observed newer delegated snapshot is a floor, even though the
    // configured pointer and key remain valid at the older snapshot.
    let tx = provider.new_transaction().await?;
    tx.get_settings()?.set_name("advanced").await?;
    tx.commit().await?;
    let newer = provider.snapshot().await?.into_tips();
    let advanced_identity = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: provider.root_id().clone(),
            tips: newer,
        }],
        hint: KeyHint::from_pubkey(&delegated.public_key()),
    };
    let advanced = DatabaseKey::with_identity(delegated.clone(), advanced_identity);
    publish(&session, advanced.clone(), provider.root_id()).await?;
    let child_id = publish(&session, advanced, provider.root_id()).await?;
    let regressed = resign(
        session.get_entry(&child_id).await?,
        entry.auth().key.clone(),
        &delegated,
    )?;
    let regressed_id = regressed.id();
    owner.backend().put(regressed).await?;
    assert!(!session.verify_entry_signature(&regressed_id).await?);
    assert!(!remote_session.verify_entry_signature(&regressed_id).await?);

    // Later revocation is current policy, not retroactive signature invalidity.
    for authority in [&provider, &session] {
        let tx = authority.new_transaction().await?;
        for key in [&direct, &delegated] {
            tx.get_settings()?
                .set_auth_key(
                    &key.public_key(),
                    AuthKey::new(None, Permission::Write(10), KeyStatus::Revoked),
                )
                .await?;
        }
        tx.commit().await?;
    }
    let current = remote_provider
        .get_settings()
        .await?
        .auth_snapshot()
        .await?;
    assert!(
        !current
            .get_key_by_pubkey(&delegated.public_key())?
            .is_active()
    );
    for db in [&session, &remote_session] {
        assert!(db.verify_entry_signature(&direct_id).await?);
        assert!(db.verify_entry_signature(&delegated_id).await?);
    }
    assert!(
        remote_session
            .clone()
            .with_key(outsider)
            .verify_entry_signature(&delegated_id)
            .await?,
        "verification retains the bound read identity, not an attached signing key"
    );
    assert!(
        publish(&session, DatabaseKey::new(direct), provider.root_id())
            .await
            .is_err()
    );
    assert!(
        publish(
            &remote_session,
            DatabaseKey::new(reader),
            provider.root_id()
        )
        .await
        .is_err(),
        "Read is not Write"
    );
    drop(shutdown);
    running.await??;
    Ok(())
}
