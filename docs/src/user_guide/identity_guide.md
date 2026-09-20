# Identity Guide

An identity gives a user one stable database root for a set of device keys.
Databases grant access to that root instead of copying every device key into their own authentication settings.

## Create an identity

```rust,ignore
let key = user.get_default_key()?;
let identity = user.create_identity("personal", &key).await?;
let identity_root = identity.root_id().clone();
```

The selected key starts as `Admin(0)`.
The identity database also has a global `Read` grant so peers can read the metadata needed to verify delegation.
That public metadata grant does not make an arbitrary key a member of the identity.

## Delegate a database

```rust,ignore
let delegation = identity
    .as_delegation(PermissionBounds {
        max: Permission::Write(10),
        min: Some(Permission::Read),
    })
    .await?;

let transaction = target.new_transaction().await?;
transaction
    .get_settings()?
    .add_delegated_tree(delegation)
    .await?;
transaction.commit().await?;
```

`PermissionBounds.min` retains its normal meaning and may promote a member's permission.

Open the target specifically through this identity:

```rust,ignore
let target = identity.open_database(&target_root).await?;
```

This method refuses to substitute another authorization route for the same key.
The target must delegate to this identity root.

## Manage keys

```rust,ignore
identity
    .add_key(
        &phone_key,
        AuthKey::active(Some("phone"), Permission::Write(10)),
    )
    .await?;

identity.revoke_key(&old_key).await?;
```

To select another local member key, provide the matching private key:

```rust,ignore
identity.set_key(new_key_id, new_private_key).await?;
```

The switch is persisted in the user's local identity index and changes the signer used by the existing `Identity` handle immediately.
The new key must already be authorized by the identity.

## Join an existing identity

Use an identity's `DatabaseTicket` with the existing bootstrap approval flow:

```rust,ignore
user.register_identity(
    "personal",
    &ticket,
    &device_key,
    AuthKey::active(Some("laptop"), Permission::Admin(0)),
)
.await?;
```

A manual approval request leaves the identity in `IdentityStatus::Pending`.
`get_identity("personal")` returns `None` while pending and performs no hidden migration or status write.
After an existing administrator approves the request and the identity database is synced, activate it explicitly:

```rust,ignore
let identity = user.activate_identity("personal").await?;
```

A failed registration removes its provisional local record.
Removing a successfully tracked identity with `remove_identity` only removes the local name; it does not delete the database or revoke keys.

## Errors

- `IdentityAlreadyExists`: the local tracking name is in use.
- `IdentityNotFound`: the local tracking name is unknown.
- `IdentityKeyMismatch`: `set_key` received a private key for another public key.
- `NoSigKeyFound`: the selected key is not authorized, or a target does not delegate to this identity root.
- `DatabaseAccessPending`: approved database state is not available locally yet.
