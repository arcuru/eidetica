# Identity Databases

An identity is an ordinary Eidetica database whose authentication settings contain the
identity's member keys.
Its root ID is a stable address that another database can name in a `DelegatedTreeRef`.

## Properties

- The identity root is the identity address.
- Member keys live in the identity database's `_settings.auth` map.
- At least one member needs `Admin` permission to manage membership.
- Identity metadata is globally readable through an active global `Read` grant so a peer can verify a delegation.
- The global grant permits reading the identity database; it is not membership in the identity and cannot authorize a target database that delegates to it.
- Delegation permission bounds keep their existing behavior, including `PermissionBounds.min` promotion.

The local user database stores only a convenience index from a caller-selected name to the identity root, lifecycle status, and selected local signing key.
The identity database remains the source of truth for membership.
Removing local tracking does not delete or revoke the identity.

## API

`User::create_identity` creates an identity with the selected key as `Admin(0)` and a global `Read` grant.
`User::register_identity` records an existing root and uses the existing bootstrap request lifecycle; a pending approval is a successful registration state.
`User::activate_identity` checks the selected key after approved state is synced and promotes the local record from pending to active.
`User::get_identity` is read-only and returns `None` for unknown or pending records.

An `Identity` exposes key addition, revocation, local signing-key selection, delegation-reference creation, and target database opening.
`Identity::set_key` rejects a private key that does not match the supplied public key and rebinds the wrapped database immediately, so subsequent identity writes use the new signer.
`Identity::open_database` always constructs a delegation path whose first step is this identity root; it does not silently select a direct, global, or different-identity path held by the same key.

Connected clients register the selected private key in the service connection's proof-of-possession keyset and use an identity-bound remote database handle.
Delegated entries are validated by the daemon that owns the local engine rather than by the connected client.

## Scope

This API does not implement automatic identity replication, background device enrollment, per-identity daemon keys, inherited snapshot floors, forward-only delegation pointers, or a different live authorization policy.
Those concerns remain separate from the lifecycle and key-management surface described here.
