//! State domain behavior without a transport or credentials adapter.

use eidetica::{
    Database, DatabaseKey, Error, ID, Instance, NewAccount, Snapshot,
    auth::{
        crypto::generate_keypair,
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
    },
    backend::{VerificationStatus, database::InMemory},
    crdt::Doc,
    state::{ReadContext, ReadScope, StateError, StateOperation, StateReader},
    store::DocStore,
};

#[tokio::test]
async fn snapshot_first_read_and_staged_overlays_keep_one_cut() -> eidetica::Result<()> {
    let (instance, _) = Instance::create_backend(
        crate::helpers::test_backend().await,
        NewAccount::passwordless("admin"),
    )
    .await?;
    let (owner, _) = generate_keypair();
    let key = DatabaseKey::new(owner.clone());
    let mut settings = Doc::new();
    settings.set("name", "base");
    let db = Database::create(&instance, owner, settings).await?;
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("data")
            .await?
            .set("base", "old")
            .await?;
        tx.get_store::<DocStore>("unrequested")
            .await?
            .set("key", "another store")
            .await
    })
    .await?;
    let reader = StateReader::new(&instance, &key);
    let engine = instance.backend().local_engine().unwrap();
    engine.clear_derived_store_state().await?;
    let memory = engine.as_any().downcast_ref::<InMemory>();
    let before = memory.map(InMemory::store_history_read_count);
    let cut = reader.capture(db.root_id(), ReadScope::Verified).await?;
    if let (Some(memory), Some(before)) = (memory, before) {
        // Cold capture reads only auth Settings; a named read below is the
        // positive control that this backend counter detects other folds.
        assert_eq!(memory.store_history_read_count() - before, 1);
    }

    let tx = db.new_transaction_at(cut.snapshot()).await?;
    let clone = tx.clone();
    clone.get_settings()?.set_name("staged").await?;
    let store = clone.get_store::<DocStore>("data").await?;
    assert!(store.delete_literal("base").await?);
    store.set("local", "only staged").await?;
    clone
        .get_store::<DocStore>("local-store")
        .await?
        .set("value", "staged")
        .await?;

    let writer = db.clone();
    tokio::spawn(async move {
        writer
            .with_transaction(|tx| async move {
                tx.get_settings()?.set_name("advanced").await?;
                tx.get_store::<DocStore>("data")
                    .await?
                    .set("base", "new")
                    .await
            })
            .await
    })
    .await
    .unwrap()?;
    assert_ne!(
        reader.capture(db.root_id(), ReadScope::Verified).await?,
        cut
    );
    let before = memory.map(InMemory::store_history_read_count);
    let computed = reader.read_doc(&cut, "data").await?;
    if let (Some(memory), Some(before)) = (memory, before) {
        assert!(memory.store_history_read_count() - before >= 2);
    }
    assert_eq!(computed.context, cut);
    assert_eq!(computed.store, "data");
    assert_eq!(
        computed.value.get("base").and_then(|v| v.as_text()),
        Some("old")
    );
    assert!(computed.value.get("local").is_none());
    assert_eq!(
        reader
            .read_doc(&cut, "_settings")
            .await?
            .value
            .get("name")
            .and_then(|v| v.as_text()),
        Some("base")
    );
    assert!(
        reader
            .read_doc(&cut, "_index")
            .await?
            .value
            .get("local-store")
            .is_none()
    );
    assert_eq!(tx.get_settings()?.get_name().await?, "staged");
    assert!(tx.get_index().await?.get_entry("local-store").await.is_ok());
    let staged = tx.get_store::<DocStore>("data").await?.get_all().await?;
    assert!(staged.get("base").is_none());
    assert!(staged.get("local").is_some());

    // Commit still uses the captured main parents and pre-write signing pin.
    let id = tx.commit().await?;
    assert_eq!(db.get_entry(&id).await?.parents()?, cut.snapshot().tips());
    assert!(db.verify_entry_signature(id).await?);
    Ok(())
}

#[tokio::test]
async fn snapshot_first_checks_read_at_the_requested_or_latest_cut() -> eidetica::Result<()> {
    let (instance, _) = Instance::create_backend(
        crate::helpers::test_backend().await,
        NewAccount::passwordless("admin"),
    )
    .await?;
    let (owner, _) = generate_keypair();
    let (member, pubkey) = generate_keypair();
    let key = DatabaseKey::new(member.clone());
    let db = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let foreign = Database::create(&instance, member, Doc::new()).await?;
    let foreign_tips = foreign.snapshot().await?;
    let owner_key = DatabaseKey::new(owner);
    let admin = StateReader::new(&instance, &owner_key);
    let reader = StateReader::new(&instance, &key);
    let before_joining = admin.capture(db.root_id(), ReadScope::Verified).await?;
    assert!(matches!(
        reader.capture(db.root_id(), ReadScope::Verified).await,
        Err(Error::State(StateError::Denied))
    ));
    assert!(matches!(
        admin
            .capture(&ID::from_bytes("missing-db"), ReadScope::Verified)
            .await,
        Err(Error::State(StateError::Denied))
    ));
    db.with_transaction(|tx| async move {
        let settings = tx.get_settings()?;
        settings
            .set_auth_key(&pubkey, AuthKey::active(None, Permission::Read))
            .await?;
        settings.set_name("while a member").await
    })
    .await?;
    let pubkey = key.public_key();
    let cut = reader.capture(db.root_id(), ReadScope::Verified).await?;
    assert!(reader.read_doc(&cut, "_index").await?.value.is_empty());
    // No grant at the requested cut, but current Read is enough.
    let computed = reader.read_doc(&before_joining, "_settings").await?;
    assert_eq!(computed.context, before_joining);
    assert_ne!(
        computed.value.get("name").and_then(|v| v.as_text()),
        Some("while a member")
    );

    let refuse = |_: &ID, op: StateOperation<'_>| {
        !matches!(
            op,
            StateOperation::Capture | StateOperation::ReadDoc { store: "_index" }
        )
    };
    let refusing = StateReader::new(&instance, &key).with_admission(Some(&refuse));
    assert!(matches!(
        refusing.capture(db.root_id(), ReadScope::Verified).await,
        Err(Error::State(StateError::Refused))
    ));
    assert!(matches!(
        refusing.read_doc(&cut, "_index").await,
        Err(Error::State(StateError::Refused))
    ));
    assert!(refusing.read_doc(&cut, "_settings").await.is_ok());

    for invalid in [Snapshot::EMPTY, foreign_tips.clone()] {
        let context = ReadContext::at(db.root_id().clone(), invalid, ReadScope::Verified);
        assert!(matches!(
            reader.read_doc(&context, "_index").await,
            Err(Error::State(StateError::InvalidBoundary))
        ));
    }
    let unavailable = ReadContext::at(
        db.root_id().clone(),
        Snapshot::from([ID::from_bytes("missing-tip")]),
        ReadScope::Verified,
    );
    assert!(matches!(
        reader.read_doc(&unavailable, "_index").await,
        Err(Error::State(StateError::UnavailableHistory))
    ));

    let tx = db
        .clone()
        .with_key(key.clone())
        .new_transaction_at(cut.snapshot())
        .await?;
    tx.get_settings()?
        .set_auth_key(&pubkey, AuthKey::active(None, Permission::Admin(0)))
        .await?;
    tx.get_store::<DocStore>("unauthorized")
        .await?
        .set("key", "must not commit")
        .await?;
    assert!(tx.commit().await.unwrap_err().is_permission_denied());
    db.with_transaction(|tx| async move {
        let settings = tx.get_settings()?;
        settings.revoke_auth_key(&pubkey).await?;
        settings.set_name("after leaving").await
    })
    .await?;
    // Historical Read is sufficient for this exact cut, not discovery of
    // the latest boundary or a read at a cut without a grant.
    let computed = reader.read_doc(&cut, "_settings").await?;
    assert_eq!(computed.context, cut);
    assert_eq!(
        computed.value.get("name").and_then(|v| v.as_text()),
        Some("while a member")
    );
    for scope in [ReadScope::Verified, ReadScope::AllowUnverified] {
        assert!(matches!(
            reader.capture(db.root_id(), scope).await,
            Err(Error::State(StateError::Denied))
        ));
    }
    let latest = admin.capture(db.root_id(), ReadScope::Verified).await?;
    let diagnostic = ReadContext::at(
        db.root_id().clone(),
        cut.snapshot().clone(),
        ReadScope::AllowUnverified,
    );
    let wrong_tree = ReadContext::at(db.root_id().clone(), foreign_tips, ReadScope::Verified);
    for denied in [before_joining, latest, diagnostic, wrong_tree] {
        assert!(matches!(
            reader.read_doc(&denied, "_settings").await,
            Err(Error::State(StateError::Denied))
        ));
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_first_delegated_read_uses_the_committed_pointer() -> eidetica::Result<()> {
    let instance = crate::helpers::test_local_instance().await;
    let (owner, _) = generate_keypair();
    let (member, pubkey) = generate_keypair();
    let db = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let delegated = Database::create(&instance, owner.clone(), Doc::new()).await?;
    let delegated_root = delegated.root_id().clone();
    let admin_key = DatabaseKey::new(owner);
    let admin = StateReader::new(&instance, &admin_key);
    let point_at = |snapshot: Snapshot| {
        let pointer = DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                min: None,
                max: Permission::Read,
            },
            tree: TreeReference {
                root: delegated_root.clone(),
                tips: snapshot.into_tips(),
            },
        };
        db.with_transaction(
            |tx| async move { tx.get_settings()?.add_delegated_tree(pointer).await },
        )
    };
    point_at(delegated.snapshot().await?).await?;
    let ungranted = admin.capture(db.root_id(), ReadScope::Verified).await?;
    let member_pubkey = pubkey.clone();
    delegated
        .with_transaction(|tx| async move {
            tx.get_settings()?
                .set_auth_key(&member_pubkey, AuthKey::active(None, Permission::Read))
                .await
        })
        .await?;
    let granted_tips = delegated.snapshot().await?;
    point_at(granted_tips.clone()).await?;
    let granted = admin.capture(db.root_id(), ReadScope::Verified).await?;
    let identity = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: delegated_root.clone(),
            tips: granted_tips.into_tips(),
        }],
        hint: KeyHint::from_pubkey(&pubkey),
    };
    let key = DatabaseKey::with_identity(member, identity);
    let reader = StateReader::new(&instance, &key);
    // The live delegated frontier grants Read, despite no grant in the old pin.
    assert!(
        reader
            .read_doc(&ungranted, "_index")
            .await?
            .value
            .is_empty()
    );
    delegated
        .with_transaction(|tx| async move { tx.get_settings()?.revoke_auth_key(&pubkey).await })
        .await?;
    point_at(delegated.snapshot().await?).await?;
    assert!(matches!(
        reader.capture(db.root_id(), ReadScope::Verified).await,
        Err(Error::State(StateError::Denied))
    ));
    // The caller's delegation hint names a grant, but only the committed
    // pointer of the requested cut can make that grant relevant.
    assert!(matches!(
        reader.read_doc(&ungranted, "_index").await,
        Err(Error::State(StateError::Denied))
    ));
    assert_eq!(reader.read_doc(&granted, "_index").await?.context, granted);
    Ok(())
}

#[tokio::test]
async fn snapshot_first_capture_never_promotes_raw_history() -> eidetica::Result<()> {
    let instance = crate::helpers::test_local_instance().await;
    let (owner, _) = generate_keypair();
    let key = DatabaseKey::new(owner.clone());
    let db = Database::create(&instance, owner, Doc::new()).await?;
    let engine = instance.backend().local_engine().unwrap();
    let reader = StateReader::new(&instance, &key);
    let cut = reader.capture(db.root_id(), ReadScope::Verified).await?;
    let raw = db
        .insert_raw(
            eidetica::Entry::builder(db.root_id().clone())
                .set_parents(cut.snapshot().tips().to_vec())
                .build()?,
        )
        .await?;
    assert_eq!(
        reader.capture(db.root_id(), ReadScope::Verified).await?,
        cut
    );
    let diagnostic = reader
        .capture(db.root_id(), ReadScope::AllowUnverified)
        .await?;
    assert_eq!(diagnostic.snapshot().tips(), std::slice::from_ref(&raw));
    assert_eq!(
        engine.get_verification_status(&raw).await?,
        VerificationStatus::Unverified
    );
    assert!(matches!(
        reader.read_doc(&diagnostic, "_index").await,
        Err(Error::State(StateError::UnsupportedProjection))
    ));
    let mislabeled = ReadContext::at(
        db.root_id().clone(),
        diagnostic.snapshot().clone(),
        ReadScope::Verified,
    );
    assert!(matches!(
        reader.read_doc(&mislabeled, "_index").await,
        Err(Error::State(StateError::InvalidBoundary))
    ));
    Ok(())
}
