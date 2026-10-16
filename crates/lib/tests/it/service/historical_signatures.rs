//! Historical authentication over the socket, including Read gates on delegated proof.

use eidetica::{
    Database, Entry, Instance,
    auth::{
        crypto::{PrivateKey, PublicKey, sign_entry, verify_entry_signature},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    backend::VerificationStatus,
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    store::{DocStore, SettingsStore},
};

async fn grant(
    settings: &SettingsStore,
    keys: &[(&PrivateKey, Permission)],
) -> eidetica::Result<()> {
    for (key, permission) in keys {
        settings
            .set_auth_key(&key.public_key(), AuthKey::active(None, *permission))
            .await?;
    }
    Ok(())
}

async fn step(db: &Database) -> eidetica::Result<DelegationStep> {
    Ok(DelegationStep {
        tree: db.root_id().clone(),
        tips: db.snapshot().await?.into_tips(),
    })
}

async fn delegate(db: &Database, authority: &Database, max: Permission) -> eidetica::Result<()> {
    let tx = db.new_transaction().await?;
    let step = step(authority).await?;
    tx.get_settings()?
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds { max, min: None },
            tree: TreeReference {
                root: step.tree,
                tips: step.tips,
            },
        })
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn publish(db: &Database, key: impl Into<DatabaseKey>) -> eidetica::Result<ID> {
    let tx = db.clone().with_key(key).new_transaction().await?;
    tx.get_store::<DocStore>("records")
        .await?
        .set("value", "signed")
        .await?;
    tx.commit().await
}

fn resign(entry: Entry, identity: SigKey, key: &PrivateKey) -> eidetica::Result<Entry> {
    let entry = entry.with_auth(|auth| auth.key = identity);
    let signature = sign_entry(&entry, key)?;
    Ok(entry.with_auth(|auth| auth.signature = Some(signature)))
}

async fn open_as(remote: &Instance, db: &Database, key: &PrivateKey) -> eidetica::Result<Database> {
    Database::open_remote(
        remote,
        remote.remote_connection().unwrap().clone(),
        db.root_id(),
        SigKey::from_pubkey(&key.public_key()),
    )
    .await
}

struct Fixture {
    // Database handles hold weak Instance references.
    _client: Instance,
    local: Database,
    remote: Database,
    authority: Database,
    nested: Database,
    direct: PrivateKey,
    delegated: PrivateKey,
    forger: PrivateKey,
    reader: PrivateKey,
    identity: SigKey,
    socket: std::path::PathBuf,
    _shutdown: tokio::sync::watch::Sender<()>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> eidetica::Result<Self> {
        let (socket, shutdown, owner, dir) = super::start_test_server().await;
        let mut user = owner.login_user("admin", None).await?;
        let admin = user.get_default_key()?;
        let authority = user.create_database(Doc::new(), &admin).await?;
        let local = user.create_database(Doc::new(), &admin).await?;
        let nested = user.create_database(Doc::new(), &admin).await?;
        let direct = PrivateKey::generate();
        let delegated = PrivateKey::generate();
        let forger = PrivateKey::generate();
        let reader = PrivateKey::generate();

        let tx = nested.new_transaction().await?;
        grant(
            &tx.get_settings()?,
            &[
                (&delegated, Permission::Admin(10)),
                (&reader, Permission::Read),
            ],
        )
        .await?;
        tx.commit().await?;
        delegate(&authority, &nested, Permission::Admin(10)).await?;
        let tx = authority.new_transaction().await?;
        let settings = tx.get_settings()?;
        // A shared name must not be mistaken for a concrete signer.
        for (key, permission) in [
            (&direct, Permission::Write(10)),
            (&delegated, Permission::Admin(10)),
        ] {
            settings
                .set_auth_key(
                    &key.public_key(),
                    AuthKey::active(Some("operator"), permission),
                )
                .await?;
        }
        grant(&settings, &[(&reader, Permission::Read)]).await?;
        tx.commit().await?;
        let tx = local.new_transaction().await?;
        grant(
            &tx.get_settings()?,
            &[
                (&direct, Permission::Write(10)),
                (&forger, Permission::Write(10)),
                (&reader, Permission::Read),
            ],
        )
        .await?;
        tx.commit().await?;
        delegate(&local, &authority, Permission::Write(10)).await?;
        let identity = SigKey::Delegation {
            path: vec![step(&authority).await?],
            hint: KeyHint::from_name("operator"),
        };

        super::create_user_via_admin(&owner, "reader").await;
        let client = Instance::connect(format!("unix://{}", socket.display())).await?;
        let login = client.login_user("reader", None).await?;
        assert!(login.admin().await.is_err(), "reader is not a host admin");
        assert!(
            Database::open(&client, local.root_id()).await.is_err(),
            "login alone grants no target access"
        );
        // Native proof-of-possession registers a separate acting key, not a target grant.
        Database::create(&client, reader.clone(), Doc::new()).await?;
        let remote = open_as(&client, &local, &reader).await?;
        Ok(Self {
            _client: client,
            local,
            remote,
            authority,
            nested,
            direct,
            delegated,
            forger,
            reader,
            identity,
            socket,
            _shutdown: shutdown,
            _dir: dir,
        })
    }

    async fn delegated_entry(&self) -> eidetica::Result<ID> {
        publish(
            &self.local,
            DatabaseKey::with_identity(self.delegated.clone(), self.identity.clone()),
        )
        .await
    }

    async fn nested_entry(&self) -> eidetica::Result<ID> {
        let SigKey::Delegation { mut path, .. } = self.identity.clone() else {
            unreachable!()
        };
        path.push(step(&self.nested).await?);
        publish(
            &self.local,
            DatabaseKey::with_identity(
                self.delegated.clone(),
                SigKey::Delegation {
                    path,
                    hint: KeyHint::from_pubkey(&self.delegated.public_key()),
                },
            ),
        )
        .await
    }

    async fn verdict(&self, id: &ID, expected: bool) -> eidetica::Result<()> {
        for db in [&self.local, &self.remote] {
            assert_eq!(db.verify_entry_signature(id).await?, expected, "entry {id}");
        }
        Ok(())
    }

    async fn put(&self, entry: Entry) -> eidetica::Result<ID> {
        let id = entry.id();
        self.local.backend()?.put(entry).await?;
        Ok(id)
    }
}

async fn with_timeout(
    test: impl std::future::Future<Output = eidetica::Result<()>>,
) -> eidetica::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), test)
        .await
        .expect("historical signature test timed out")
}

#[tokio::test]
async fn test_historical_signature_parity_and_operating_key() -> eidetica::Result<()> {
    with_timeout(async {
    let f = Fixture::new().await?;
    let direct = publish(&f.local, f.direct.clone()).await?;
    let forged = publish(&f.local, f.forger.clone()).await?;
    let delegated = f.delegated_entry().await?;
    let nested = f.nested_entry().await?;
    for (id, signer) in [
        (&direct, &f.direct),
        (&forged, &f.forger),
        (&delegated, &f.delegated),
        (&nested, &f.delegated),
    ] {
        f.verdict(id, true).await?;
        let entry = f.remote.get_entry(id).await?;
        assert_eq!(entry, f.local.get_entry(id).await?);
        assert!(verify_entry_signature(&entry, &signer.public_key()).is_ok());
    }
    let forged = f.remote.get_entry(&forged).await?;
    for key in [&f.direct, &f.delegated] {
        assert!(verify_entry_signature(&forged, &key.public_key()).is_err());
    }
    // Enumerate authorized concrete keys through that database's own Read handle.
    let client = f.remote.instance()?;
    let authority = open_as(&client, &f.authority, &f.reader).await?;
    let auth = authority.get_settings().await?.auth_snapshot().await?;
    assert!(
        auth.get_key_by_pubkey(&f.direct.public_key())?
            .permissions()
            .can_write()
    );
    let entry = f.remote.get_entry(&delegated).await?;
    let signers = auth
        .get_all_keys()?
        .into_iter()
        .filter_map(|(key, auth)| {
            let public = PublicKey::from_prefixed_string(&key).ok()?;
            (auth.is_active()
                && auth.permissions().can_write()
                && verify_entry_signature(&entry, &public).is_ok())
            .then_some(public)
        })
        .collect::<Vec<_>>();
    assert_eq!(signers, vec![f.delegated.public_key()]);

    let auth = f.local.get_settings().await?.auth_snapshot().await?;
    let owner = f.local.instance()?;
    let candidates = AuthValidator::new()
        .resolve_sig_key(&entry.auth().key, &auth, Some(&owner))
        .await?;
    assert_eq!(candidates.len(), 2);
    let signer = candidates
        .iter()
        .find(|key| verify_entry_signature(&entry, &key.public_key).is_ok())
        .unwrap();
    assert_eq!(signer.public_key, f.delegated.public_key());
    assert_eq!(signer.effective_permission, Permission::Write(10));
    let error = AuthValidator::new()
        .resolve_sig_key(&entry.auth().key, &auth, Some(&client))
        .await
        .unwrap_err();
    assert!(
        matches!(error, eidetica::Error::Instance(ref error)
        if matches!(error.as_ref(), eidetica::instance::InstanceError::OperationNotSupported { .. })),
        "{error}"
    );
    Ok(())
    }).await
}

#[tokio::test]
async fn test_historical_signature_read_and_session_gates() -> eidetica::Result<()> {
    with_timeout(async {
        let f = Fixture::new().await?;
        let direct = publish(&f.local, f.direct.clone()).await?;
        let delegated = f.delegated_entry().await?;
        let anonymous = Instance::connect(format!("unix://{}", f.socket.display())).await?;
        let identity = SigKey::from_pubkey(&f.reader.public_key());
        assert!(
            anonymous
                .remote_connection()
                .unwrap()
                .verify_entry_signature(
                    f.local.root_id().clone(),
                    identity.clone(),
                    delegated.clone()
                )
                .await
                .is_err()
        );
        let client = f.remote.instance()?;
        let conn = client.remote_connection().unwrap();
        assert!(
            conn.verify_entry_signature(
                f.local.root_id().clone(),
                SigKey::from_pubkey(&PrivateKey::generate().public_key()),
                delegated.clone()
            )
            .await
            .is_err(),
            "unregistered identity"
        );
        assert!(
            conn.verify_entry_signature(
                f.local.root_id().clone(),
                identity,
                f.authority.root_id().clone()
            )
            .await
            .is_err(),
            "foreign entry ID"
        );

        let target_only = PrivateKey::generate();
        let tx = f.local.new_transaction().await?;
        grant(&tx.get_settings()?, &[(&target_only, Permission::Read)]).await?;
        tx.commit().await?;
        Database::create(&client, target_only.clone(), Doc::new()).await?;
        let target = open_as(&client, &f.local, &target_only).await?;
        assert!(target.verify_entry_signature(&direct).await?);
        assert!(
            target.verify_entry_signature(&delegated).await.is_err(),
            "target Read does not grant delegated-proof Read"
        );
        assert!(open_as(&client, &f.authority, &target_only).await.is_err());
        assert!(
            publish(&f.remote, f.reader.clone()).await.is_err(),
            "Read is not Write"
        );
        assert!(
            f.remote
                .clone()
                .with_key(PrivateKey::generate())
                .verify_entry_signature(&delegated)
                .await?,
            "attached signing key must not replace the bound reader"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_historical_signature_invalid_and_incomplete_claims() -> eidetica::Result<()> {
    with_timeout(async {
        let f = Fixture::new().await?;
        let id = f.delegated_entry().await?;
        let entry = f.local.get_entry(&id).await?;
        let outsider = PrivateKey::generate();
        let tx = f.local.new_transaction().await?;
        tx.get_settings()?.set_name("settings operation").await?;
        let settings = f.local.get_entry(tx.commit().await?).await?;
        let attenuated = resign(settings, f.identity.clone(), &f.delegated)?;
        assert!(verify_entry_signature(&attenuated, &f.delegated.public_key()).is_ok());
        let mut invalid = vec![
            attenuated,
            entry
                .clone()
                .with_auth(|auth| auth.signature = Some(sign_entry(&entry, &outsider).unwrap())),
        ];
        for key in [&outsider, &f.reader] {
            invalid.push(resign(
                entry.clone(),
                SigKey::from_pubkey(&key.public_key()),
                key,
            )?);
        }
        let SigKey::Delegation { path, .. } = &f.identity else {
            unreachable!()
        };
        let step = &path[0];
        let mut paths = vec![vec![], vec![step.clone(); 11]];
        for tips in [
            vec![],
            vec![f.local.root_id().clone()],
            vec![f.authority.root_id().clone()],
            vec![step.tips[0].clone(); 65],
        ] {
            paths.push(vec![DelegationStep {
                tree: step.tree.clone(),
                tips,
            }]);
        }
        for path in paths {
            invalid.push(resign(
                entry.clone(),
                SigKey::Delegation {
                    path,
                    hint: KeyHint::from_pubkey(&f.delegated.public_key()),
                },
                &f.delegated,
            )?);
        }
        invalid.push(resign(
            entry.clone(),
            SigKey::Delegation {
                path: path.clone(),
                hint: KeyHint::from_pubkey(&f.forger.public_key()),
            },
            &f.delegated,
        )?);
        for bad in invalid {
            f.verdict(&f.put(bad).await?, false).await?;
        }
        let incomplete = resign(
            entry,
            SigKey::Delegation {
                path: vec![DelegationStep {
                    tree: step.tree.clone(),
                    tips: vec![ID::from_bytes("missing-delegated-tip")],
                }],
                hint: KeyHint::from_pubkey(&f.delegated.public_key()),
            },
            &f.delegated,
        )?;
        let id = f.put(incomplete).await?;
        for db in [&f.local, &f.remote] {
            assert!(
                db.verify_entry_signature(&id).await.is_err(),
                "incomplete is not a valid or invalid verdict"
            );
        }
        assert_eq!(
            f.local
                .backend()?
                .local_engine()
                .unwrap()
                .get_verification_status(&id)
                .await?,
            VerificationStatus::Unverified
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_historical_signature_rejects_inherited_floor_regression() -> eidetica::Result<()> {
    with_timeout(async {
        let f = Fixture::new().await?;
        let original = f.delegated_entry().await?;
        let tx = f.authority.new_transaction().await?;
        tx.get_settings()?.set_name("advanced").await?;
        tx.commit().await?;
        let key = DatabaseKey::with_identity(
            f.delegated.clone(),
            SigKey::Delegation {
                path: vec![step(&f.authority).await?],
                hint: KeyHint::from_pubkey(&f.delegated.public_key()),
            },
        );
        publish(&f.local, key.clone()).await?;
        let child = publish(&f.local, key).await?;
        f.verdict(&original, true).await?;
        f.verdict(&child, true).await?;
        let regressed = resign(
            f.local.get_entry(&child).await?,
            f.identity.clone(),
            &f.delegated,
        )?;
        f.verdict(&f.put(regressed).await?, false).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_historical_signature_revocation_and_each_hops_current_read() -> eidetica::Result<()> {
    with_timeout(async {
        let f = Fixture::new().await?;
        let direct = publish(&f.local, f.direct.clone()).await?;
        let delegated = f.delegated_entry().await?;
        let nested = f.nested_entry().await?;
        // Revoking writers is not retroactive signature invalidation.
        for (db, keys) in [
            (&f.authority, vec![&f.direct, &f.delegated]),
            (&f.local, vec![&f.direct]),
        ] {
            let tx = db.new_transaction().await?;
            for key in keys {
                tx.get_settings()?
                    .revoke_auth_key(&key.public_key())
                    .await?;
            }
            tx.commit().await?;
        }
        for id in [&direct, &delegated, &nested] {
            f.verdict(id, true).await?;
        }
        assert!(publish(&f.local, f.direct.clone()).await.is_err());
        let client = f.remote.instance()?;
        let authority = open_as(&client, &f.authority, &f.reader).await?;
        assert!(
            !authority
                .get_settings()
                .await?
                .auth_snapshot()
                .await?
                .get_key_by_pubkey(&f.delegated.public_key())?
                .is_active()
        );
        // Recheck Read on already-open handles, including the second delegated hop.
        for (db, denied, readable) in [
            (&f.nested, vec![&nested], vec![&direct, &delegated]),
            (&f.authority, vec![&nested, &delegated], vec![&direct]),
            (&f.local, vec![&nested, &delegated, &direct], vec![]),
        ] {
            let tx = db.new_transaction().await?;
            tx.get_settings()?
                .revoke_auth_key(&f.reader.public_key())
                .await?;
            tx.commit().await?;
            for id in denied {
                assert!(f.local.verify_entry_signature(id).await?);
                assert!(
                    f.remote.verify_entry_signature(id).await.is_err(),
                    "Read revoked on {} for {id}",
                    db.root_id()
                );
            }
            for id in readable {
                assert!(f.remote.verify_entry_signature(id).await?);
            }
        }
        Ok(())
    })
    .await
}
