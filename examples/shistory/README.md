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

Every host needs a running Eidetica daemon and a passwordless daemon account.
`--user`/`SHISTORY_USER` names that account's login.
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
Eidetica database ticket. `setup` calls the public `AccountSession::join` API through the
service socket with the user's default key, Write(10) permission, and explicit
sync-on-commit settings. The client signs with its own key; the daemon performs
the network exchange. Only a successful join registers a local host UUID.
`ticket` enables this user's sharing preference with `Database::share()` before
calling `Database::ticket()`. The preference and locator do not prove that the
daemon has reconciled serving or that a peer is reachable.

The default history database grants global Write access. Any peer that can
reach it and proves possession of its own key can join without separate
approval; the ticket is a locator, not an access grant or secret credential.
This is unsuitable for sensitive history on an untrusted network.

For an operator-configured database requiring approval, setup reports Pending
and exits unsuccessfully without a host UUID, history reference, or new Account
tracking/preferences.
After approval, explicitly rerun `setup` with the same ticket and `--name`.
Rejection is an error, not a completed setup. There is no automatic or
restart-safe completion. An interrupted or failed call can still leave peer
requests, replicated data, routing, or a committed Account update; retry rather
than assuming rollback.

## Database selection and upgrades

Setup remembers the history database root ID in the Account as the `history`
reference in the `shistory` namespace, using `AccountSession::set_reference`.
Start, finish, and queries read that one reference and open the named database
through the normal key and Auth checks; they do not open unrelated tracked
databases or look at display names, so renaming the database changes nothing.
Capture also checks that the database is still tracked and contains the
configured host before writing. Permissions and tips are never cached across
shell processes. The reference grants no access by itself.

Run setup from one shell at a time for an Account. The reference is an ordinary
CRDT write, not a distributed compare-and-set, so two concurrent first setups
could each create a database.

Installations from before references keep working through the unique tracked
database named `shistory`. The first successful start or finish that finds the
configured host there remembers that root ID, inside the same one-second
deadline. List, search, summary, ticket, and status use the name lookup but do
not migrate it. If no database or several databases have that name, capture
fails without creating or choosing one.

Once a reference exists, a malformed, stale, untracked, inaccessible, or
unavailable target is an error. It never falls back to the name lookup or
creates a replacement database, and setup refuses to select a different root.
Migration changes only the reference, not history, host UUIDs, or host Stores.

To adopt a legacy database explicitly, or finish a setup that created or joined
the database but failed before printing a host UUID, add a host to the existing
root by ID. Setup errors in that state print the root ID to use:

```console
"$SHISTORY_BIN" setup --database bafyr4i... --name "$SHISTORY_HOST_NAME"
```

A failed ticket setup can also be retried with the same ticket.

One window is not repairable this way. Eidetica commits a new database before
the Account records it, so if first setup fails between those two writes (for
example, the daemon connection drops), the new root is untracked, the error
cannot name it, and rerunning setup creates another database. The abandoned
root holds no history and is not shared.

For an isolated populated SQLite comparison of capture latency, build a
baseline `shistory` binary with the same Cargo profile, then run:

```console
SHISTORY_BASELINE_BIN=/absolute/path/to/baseline/shistory \
  cargo test -p shistory --all-features --test service populated_sql_capture_profile -- --ignored --nocapture
```

The ignored exercise seeds 20 unrelated tracked databases and 100 completed
records, then alternates baseline and candidate start/finish runs through a
recording socket proxy. It reports the storage requests and the time before the
history write is submitted, and never logs command text. Pair 0 is a warm-up.
Timings include the proxy and process startup; they are not a latency guarantee.

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
"$SHISTORY_BIN" list --duplicates --limit 50
"$SHISTORY_BIN" summary
"$SHISTORY_BIN" summary --host-id e4e2f3f6-d723-42b8-a0b7-f245624960b4
"$SHISTORY_BIN" status
```

List and search output is tab-separated with a header:

```text
start time    host    exit status    duration (ms)    working directory    command
```

An unfinished command has `incomplete` for its exit status and `-` for its
duration. Control characters are escaped, so each record occupies one row;
stored text is unchanged. Displayed commands, working directories, and host
names are bounded to 80, 40, and 20 characters respectively, with an ellipsis
when truncated. Queries default to 20 unique full commands: filters select
executions first, then the newest occurrence of each exact command text is kept,
then the limit is applied. `--duplicates` displays every matching execution.
Queries accept limits from 1 through 1000.

`summary` scans all stored records, regardless of the list/search limit. It
reports the total, date range, success/failure/incomplete counts, the most
common first command word, the longest completed runtime, and per-machine
counts. It includes all hosts by default; `--host-id` selects the stable UUID
for one machine. Display names are only labels, so two hosts with the same name
remain separate summary rows.

`status` checks the selected socket, daemon connection, passwordless user,
history database, and configured host UUID without writing history, running
setup, or initiating sync. It exits nonzero when a local recording prerequisite
is missing or a read cannot complete, and gives a recovery hint for each failed
check. A socket path alone is not considered healthy: status completes a
bounded service connection and lookups. It does not write a probe record or
inspect the parent shell, so it cannot confirm that the zsh hooks are loaded,
that writes would succeed, or that remote hosts are synchronized. The existing
service login API may idempotently bootstrap missing key or authorization
metadata in a legacy or incompletely initialized user tree; status performs no
other persistent mutation.

## Security and limits

History includes command text, working directories, timestamps, status, and
runtime. It may contain secrets. Eidetica history is replicated and append-only:
there is no redaction, retention policy, import path, or erasure guarantee here.
A leading space is not protection for a secret that must not persist.

Join verifies the supplied response and its ancestry, not global catch-up.
Already-held databases unreadable by the selected key remain refused, including
orphan replicas and unsupported second-user recovery. Missing delegated signing
history remains Unverified until supplied and verified separately; setup does
not acquire it or fix native competing grant/revocation semantics. User-key
approval does not enroll the daemon device: ongoing private background sync
needs separate device authorization, whereas the default global grant permits
both user and daemon keys. See Eidetica's [bootstrap limits](../../docs/src/user_guide/bootstrap.md#current-recovery-limits).

This is a v0 example, not a history service. It has no daemon lifecycle,
embedded-backend fallback, application-owned sync, retention controls, secret
filtering, imports, or interactive search. It supports zsh hooks only.
