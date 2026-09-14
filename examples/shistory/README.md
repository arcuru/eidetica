# shistory

A small zsh history example backed by an existing Eidetica daemon. Its command-start/command-finish workflow is inspired by [Atuin](https://atuin.sh/); it deliberately omits Atuin's interactive search and broader shell tooling.

`shistory` connects only to Eidetica's Unix service socket. It does not start a daemon, open a backend directly, or implement synchronization. Each host writes a separate `Table` Store in one `shistory` database, while a tiny `hosts` Store records the explicitly configured host labels needed for all-host queries.

## Prerequisites

- A running Eidetica daemon. `EIDETICA_SOCKET` overrides its default socket path.
- An existing **passwordless** daemon user. Passwordless users keep their root key unencrypted, so protect the daemon's data directory with normal filesystem permissions.
- Peer connections and database access preconfigured on each daemon. Eidetica owns subsequent synchronization.
- A stable, unique host label such as `laptop-work` or `server-home`. Do not derive this from a transient hostname.

History may contain secrets. Eidetica history is replicated and append-only; skipping leading-space commands is a capture convention, not secret detection or an erasure guarantee.

## Setup

Build the workspace example, then configure its environment:

```console
$ cargo build -p shistory
$ export SHISTORY_BIN="$PWD/target/debug/shistory"
$ export SHISTORY_USER=alice
$ export SHISTORY_HOST=laptop-work
$ shistory setup
history database: bafyr4i...
$ source examples/shistory/shistory.zsh
```

Add the exports and `source` line to `.zshrc` after confirming the setup command succeeds. Remove that line to uninstall the hooks.

The first configured host creates the database. Other preauthorized hosts open the same tracked database and register their own label with `shistory setup`.

## Queries

```console
shistory list --limit 20
shistory list --host server-home --limit 20
shistory list --all-hosts --limit 50
shistory search 'git log' --all-hosts --limit 25
```

Rows are tab-separated: start time, host, exit status (`incomplete` when no finish hook ran), duration in milliseconds, working directory, then command text.

Set `SHISTORY_CAPTURE_DISABLED=1` to disable capture without removing the hooks. Commands beginning with a space are always skipped.

Hook commands discard their errors, preserve the command's exit status, and never print command text if the daemon is unavailable. Run a query or `shistory setup` directly when diagnosing connection errors.
