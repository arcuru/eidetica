# shistory

`shistory` is a small zsh history example for an existing Eidetica daemon. Its
start/finish model was inspired by [Atuin](https://atuin.sh/), without Atuin's
interactive search or broader shell tooling.

It connects only to the daemon's Unix service socket. It never starts or
manages a daemon, opens a backend directly, or manages synchronization. One
`shistory` database holds a small `hosts` catalog and one `Table` Store per
explicit host label. That keeps writes from each host separate while
`--all-hosts` queries combine them.

## Before setup

This example assumes an operator has already done the following on every host:

- Run an Eidetica daemon and grant the shell user filesystem access to its Unix
  socket. `EIDETICA_SOCKET` selects the socket; otherwise Eidetica uses
  `$XDG_RUNTIME_DIR/eidetica/service.sock` or
  `/tmp/eidetica-$USER/service.sock`.
- Create the `SHISTORY_USER` user as a **passwordless** daemon user. `shistory`
  only logs that user in; it cannot create users. A passwordless user's root
  signing key is unencrypted, so protect the daemon data directory and socket
  path. Socket filesystem permissions are the trust boundary.
- Choose a stable, unique `SHISTORY_HOST` label such as `laptop-work` or
  `server-home`. Do not derive it from a hostname that may change.

For two hosts, configure their daemon peer relationship and authorize both for
one shared database _before_ running `setup` on the second host. Its user must
already see exactly one tracked database named `shistory`. The first host creates
that database; each later host only registers its label. Existing ticket,
access-request, and approval APIs are not exposed through the service socket,
and `shistory` has no workflow for them. Do that daemon and database setup
separately; after it is in place, Eidetica handles ongoing replication and
offline local writes converge when the daemons synchronize.

## Install the hook

Build the example, set its identity, then initialize the first host:

```console
$ cargo build -p shistory
$ export SHISTORY_BIN="$PWD/target/debug/shistory"
$ export SHISTORY_USER=alice
$ export SHISTORY_HOST=laptop-work
$ # Set this only when the daemon does not use Eidetica's default socket.
$ export EIDETICA_SOCKET=/path/to/eidetica.sock
$ "$SHISTORY_BIN" setup
history database: bafyr4i...
$ source "$PWD/examples/shistory/shistory.zsh"
```

Add the three `SHISTORY_*` exports and the `source` command to `.zshrc`, using
absolute paths that remain valid after changing directories. Remove the `source`
line to stop installing the hooks.

The `preexec` hook starts a record before a command runs. The following `precmd`
hook fills in its duration and exit status. If either hook call cannot reach the
daemon, it stays quiet, preserves the command's exit status, and never prints the
command text. A failed start leaves no record; a failed finish leaves an
`incomplete` record.

Set `SHISTORY_CAPTURE_DISABLED=1` to leave the hooks installed but stop capture;
`unset SHISTORY_CAPTURE_DISABLED` enables it again. Commands beginning with a
space are always skipped. This is an opt-out convention, not secret detection.

## Query history

```console
"$SHISTORY_BIN" list --limit 20
"$SHISTORY_BIN" list --host server-home --limit 20
"$SHISTORY_BIN" list --all-hosts --limit 50
"$SHISTORY_BIN" search 'git log' --all-hosts --limit 25
```

Each row is tab-separated:

```text
start time    host    exit status    duration (ms)    working directory    command
```

An unfinished command has `incomplete` for its exit status and `-` for its
duration. Queries default to 100 rows and accept limits from 1 through 1000.

## Security and limits

History includes command text, working directories, timestamps, status, and
runtime. It may contain secrets. Eidetica history is replicated and append-only:
there is no redaction, retention policy, import path, or erasure guarantee here.
Do not treat a leading space as protection for a secret that must not persist or
replicate.

This is a v0 example, not a history service. It has no daemon lifecycle,
embedded-backend fallback, sync management, credential provisioning,
ticket/request/approval UI, retention controls, secret filtering, imports, or
interactive search. It supports zsh hooks only.
