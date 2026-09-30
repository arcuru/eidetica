//! Live delegated access uses accepted state, not the snapshot in a retained identity.

use eidetica::{
    Database, Instance,
    auth::{
        crypto::{PublicKey, generate_keypair},
        types::{
            AuthKey, DelegatedTreeRef, DelegationStep, KeyHint, Permission, PermissionBounds,
            SigKey, TreeReference,
        },
        validation::AuthValidator,
    },
    crdt::Doc,
    database::DatabaseKey,
    store::DocStore,
};

/// Public identity metadata does not make an arbitrary reader a delegated member.
pub(crate) async fn live_delegation_pair(
    instance: &Instance,
    member: &PublicKey,
) -> (Database, Database, SigKey) {
    let identity = Database::create(
        instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();
    let txn = identity.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
        .set_auth_key(
            member,
            AuthKey::active(Some("member"), Permission::Write(5)),
        )
        .await
        .unwrap();
    settings
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let tips = identity.snapshot().await.unwrap().into_tips();
    let target = Database::create(
        instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();
    let txn = target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: None,
            },
            tree: TreeReference {
                root: identity.root_id().clone(),
                tips: tips.clone(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let sigkey = SigKey::Delegation {
        path: vec![DelegationStep {
            tree: identity.root_id().clone(),
            tips,
        }],
        hint: KeyHint::from_pubkey(member),
    };
    (identity, target, sigkey)
}

#[tokio::test]
async fn live_delegation_removal_preserves_historical_entry_not_access() {
    let (instance, _) = Instance::create_backend(
        crate::helpers::test_backend().await,
        eidetica::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, member) = generate_keypair();
    let (identity, target, sigkey) = live_delegation_pair(&instance, &member).await;
    let retained = target
        .clone()
        .with_key(DatabaseKey::with_identity(key, sigkey.clone()));
    assert_eq!(
        retained.current_permission().await.unwrap(),
        Permission::Write(10)
    );
    let txn = retained.new_transaction().await.unwrap();
    txn.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("secret", "retained history")
        .await
        .unwrap();
    let entry_id = txn.commit().await.unwrap();
    let historical_auth = target
        .get_settings()
        .await
        .unwrap()
        .auth_snapshot()
        .await
        .unwrap();

    let txn = identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .as_doc_store()
        .delete(format!("auth.keys.{member}"))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    identity.verify().await.unwrap();
    let entry = instance.backend().get(&entry_id).await.unwrap();
    assert!(
        AuthValidator::new()
            .validate_entry(&entry, &historical_auth, Some(&instance))
            .await
            .unwrap(),
        "old entry still validates at its pinned identity snapshot"
    );
    assert!(
        retained.current_permission().await.is_err(),
        "a retained pre-removal identity must not authorize live access"
    );
    assert!(
        !Database::can_access(&instance, target.root_id(), &member, &Permission::Read)
            .await
            .unwrap()
    );
    // Reading the identity's public metadata remains legitimate.
    assert!(
        Database::can_access(&instance, identity.root_id(), &member, &Permission::Read)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn live_delegation_downgrade_and_min_promotion() {
    let (instance, _) = Instance::create_backend(
        crate::helpers::test_backend().await,
        eidetica::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, member) = generate_keypair();
    let (identity, target, sigkey) = live_delegation_pair(&instance, &member).await;
    let retained = target
        .clone()
        .with_key(DatabaseKey::with_identity(key, sigkey));
    let txn = identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(&member, AuthKey::active(Some("member"), Permission::Read))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        retained.current_permission().await.unwrap(),
        Permission::Read,
        "old tips must not recover Write"
    );

    // The target may intentionally promote a Read member through its minimum.
    let txn = target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(10),
                min: Some(Permission::Write(20)),
            },
            tree: TreeReference {
                root: identity.root_id().clone(),
                tips: identity.snapshot().await.unwrap().into_tips(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        retained.current_permission().await.unwrap(),
        Permission::Write(20)
    );
    let txn = identity.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .revoke_auth_key(&member)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert!(
        retained.current_permission().await.is_err(),
        "min must not promote a revoked member or wildcard reader"
    );

    // An independent direct grant on the target still works after removal.
    let txn = target.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .set_auth_key(&member, AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert!(
        Database::can_access(&instance, target.root_id(), &member, &Permission::Read)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn live_delegation_checks_current_intermediate_path_and_target_bounds() {
    let instance = crate::helpers::test_local_instance().await;
    let (key, member) = generate_keypair();
    let (identity, intermediate, inner_sigkey) = live_delegation_pair(&instance, &member).await;
    let outer = Database::create(
        &instance,
        instance.signing_key().unwrap().clone(),
        Doc::new(),
    )
    .await
    .unwrap();
    let txn = outer.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Write(15),
                min: None,
            },
            tree: TreeReference {
                root: intermediate.root_id().clone(),
                tips: intermediate.snapshot().await.unwrap().into_tips(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let SigKey::Delegation {
        path: inner_path,
        hint,
    } = inner_sigkey
    else {
        unreachable!()
    };
    let sigkey = SigKey::Delegation {
        path: vec![
            DelegationStep {
                tree: intermediate.root_id().clone(),
                tips: intermediate.snapshot().await.unwrap().into_tips(),
            },
            inner_path[0].clone(),
        ],
        hint,
    };
    let retained = outer
        .clone()
        .with_key(DatabaseKey::with_identity(key.clone(), sigkey.clone()));
    assert_eq!(
        retained.current_permission().await.unwrap(),
        Permission::Write(15)
    );
    let txn = intermediate.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(DelegatedTreeRef {
            permission_bounds: PermissionBounds {
                max: Permission::Read,
                min: None,
            },
            tree: TreeReference {
                root: identity.root_id().clone(),
                tips: identity.snapshot().await.unwrap().into_tips(),
            },
        })
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        retained.current_permission().await.unwrap(),
        Permission::Read,
        "every hop uses current accepted bounds"
    );
    let txn = intermediate.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .as_doc_store()
        .delete(format!("auth.delegations.{}", identity.root_id()))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert!(
        retained.current_permission().await.is_err(),
        "an old intermediate snapshot cannot revive a removed path"
    );

    let SigKey::Delegation { path, hint } = sigkey else {
        unreachable!()
    };
    for bad in [
        SigKey::Delegation {
            path: vec![path[1].clone(), path[0].clone()],
            hint: hint.clone(),
        },
        SigKey::Delegation {
            path: vec![path[0].clone(); 11],
            hint: hint.clone(),
        },
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: path[0].tree.clone(),
                tips: vec![identity.root_id().clone(); 65],
            }],
            hint: hint.clone(),
        },
        SigKey::Delegation {
            path,
            hint: KeyHint::from_pubkey(&generate_keypair().1),
        },
    ] {
        assert!(
            outer
                .clone()
                .with_key(DatabaseKey::with_identity(key.clone(), bad))
                .current_permission()
                .await
                .is_err()
        );
    }
}
