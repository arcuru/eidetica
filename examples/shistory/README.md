# shistory

`shistory` is a small zsh history example for an existing Eidetica daemon. Its
start/finish model was inspired by [Atuin](https://atuin.sh/), without Atuin's
interactive search or broader shell tooling.

It connects only to the daemon's Unix service socket. It never starts or
manages a daemon, opens a backend directly, or runs a second sync engine. One
`shistory` database holds a small `hosts` catalog and one `Table` Store per
local host UUID. The UUID is generated once by setup and is the stable Store
identity; the host's display name can change without moving its history.

## Before setup

Every host needs a running Eidetica daemon and a passwordless daemon user.
By default, the daemon and `shistory` use
`$XDG_RUNTIME_DIR/eidetica/service.sock`. If `XDG_RUNTIME_DIR` is absent, they
fall back to `/tmp/eidetica-$USER/service.sock`. A systemd user service
commonly resolves the default to `/run/user/$UID/eidetica/service.sock`.

In a restored shell where the runtime directory variable is missing, set the
socket explicitly instead:

```console
export EIDETICA_SOCKET=/path/to/eidetica.sock
```

This is enough on its own; `XDG_RUNTIME_DIR` does not also need to be exported.
Use the same socket path for the daemon and its clients. A passwordless user's
root key is unencrypted, so protect the daemon data directory and socket path;
socket filesystem permissions are the local trust boundary.

`eidetica serve` is a separate HTTP server and dashboard mode, normally on port 3000. It opens the backend directly, so do not run it against the same SQLite
data directory while `eidetica daemon` owns that directory.

The daemon owns synchronization and advertises the addresses embedded in an
Eidetica database ticket. A second host needs only that ticket: `setup` sends it
through the service socket, asks the daemon to join and track the database, and
registers a new local host UUID. The history database grants global Write
access, so any daemon user that proves possession of its own key can join from
the ticket without separate approval. Treat the ticket as a write-capability:
no peer or database access preconfiguration is required.

## Install the hook

Build the example and initialize the first host:

```console
$ cargo build -p shistory
$ export SHISTORY_BIN="$PWD/target/debug/shistory"
$ export SHISTORY_USER=alice
$ export SHISTORY_HOST_NAME='work laptop'
$ # Set this only when the daemon does not use Eidetica's default socket.
$ export EIDETICA_SOCKET=/path/to/eidetica.sock
$ "$SHISTORY_BIN" setup --name "$SHISTORY_HOST_NAME"
history database: bafyr4i...
host id: 4cc330d8-b6af-44e5-a46b-eb700df805c5
$ export SHISTORY_HOST_ID=4cc330d8-b6af-44e5-a46b-eb700df805c5
$ source "$PWD/examples/shistory/shistory.zsh"
```

Print a ticket on the first host, then join from a second host with one command:

```console
$ "$SHISTORY_BIN" ticket
eidetica:?db=...&pr=iroh:...
$ "$SHISTORY_BIN" setup 'eidetica:?db=...&pr=iroh:...' --name 'home server'
history database: bafyr4i...
host id: e4e2f3f6-d723-42b8-a0b7-f245624960b4
```

Set `SHISTORY_HOST_ID` on each host to the UUID printed by its own setup. Add
`SHISTORY_BIN`, `SHISTORY_USER`, `SHISTORY_HOST_ID`, and the `source` command to
`.zshrc`, using absolute paths. The display name is stored in Eidetica rather
than read from the environment after setup. Rename it without changing the UUID
or Store:

```console
"$SHISTORY_BIN" rename 'travel laptop'
```

The `preexec` hook starts a record before a command runs. The following
`precmd` hook fills in its duration and exit status. If either hook call cannot
reach the daemon, it stays quiet, preserves the command's exit status, and never
prints the command text. Each start or finish request has a fixed one-second
deadline covering the socket connection, login, and database request.
Cancellation can race a daemon commit, so a timed-out operation may still have
committed.

Command text is sent through standard input, not a process argument. Commands
beginning with a space are skipped. To stop capture, remove the `source` line
and start a new shell, or remove both hooks from the current shell with
`add-zsh-hook -d preexec _shistory_preexec` and
`add-zsh-hook -d precmd _shistory_precmd`.

## Query history

```console
"$SHISTORY_BIN" list --limit 20
"$SHISTORY_BIN" list --host-id e4e2f3f6-d723-42b8-a0b7-f245624960b4 --limit 20
"$SHISTORY_BIN" list --all-hosts --limit 50
"$SHISTORY_BIN" search 'git log' --all-hosts --limit 25
"$SHISTORY_BIN" summary
"$SHISTORY_BIN" summary --host-id e4e2f3f6-d723-42b8-a0b7-f245624960b4
```

List and search output is tab-separated with a header:

```text
start time    host    exit status    duration (ms)    working directory    command
```

An unfinished command has `incomplete` for its exit status and `-` for its
duration. Control characters are escaped, so each record occupies one row;
stored text is unchanged. Displayed commands, working directories, and host
names are bounded to 80, 40, and 20 characters respectively, with an ellipsis
when truncated. Queries default to 20 rows and accept limits from 1 through 1000.

`summary` scans all stored records, regardless of the list/search limit. It
reports the total, date range, success/failure/incomplete counts, the most
common first command word, the longest completed runtime, and per-machine
counts. It includes all hosts by default; `--host-id` selects the stable UUID
for one machine. Display names are only labels, so two hosts with the same name
remain separate summary rows.

## Security and limits

History includes command text, working directories, timestamps, status, and
runtime. It may contain secrets. Eidetica history is replicated and append-only:
there is no redaction, retention policy, import path, or erasure guarantee here.
A leading space is not protection for a secret that must not persist.

This is a v0 example, not a history service. It has no daemon lifecycle,
embedded-backend fallback, application-owned sync, retention controls, secret
filtering, imports, or interactive search. It supports zsh hooks only.
