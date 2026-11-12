# Docker

The Nix-built image runs as UID/GID **1000:1000** and includes no curl, wget or
interactive shell tools. Its entrypoint initializes an administrator on first
start, then runs the server. Later starts retain the existing instance and login.

Images are available from `ghcr.io/arcuru/eidetica` and `arcuru/eidetica`.
`dev` follows main; `latest` is the latest stable release. Use a version tag or
digest when pinning a deployment. The directory behavior below applies to images
built with separate SQLite cache support; older images may differ.

## Quick start

Supply an administrator password explicitly. For an interactive shell:

```bash
read -r -s -p 'Initial admin password: ' EIDETICA_ADMIN_PASSWORD; echo
export EIDETICA_ADMIN_PASSWORD
docker run -d --name eidetica -p 127.0.0.1:5942:5942 \
  --mount type=volume,source=eidetica-data,target=/config \
  --mount type=volume,source=eidetica-cache,target=/cache \
  -e EIDETICA_ADMIN_PASSWORD ghcr.io/arcuru/eidetica:dev
unset EIDETICA_ADMIN_PASSWORD
```

Log in as `admin` with that password. Environment credentials are visible in
`docker inspect`; for a deployed service, prefer a secret file readable by UID
1000 mounted read-only at `/run/secrets/admin_password`. The file takes priority
over `EIDETICA_ADMIN_PASSWORD`. Missing/unreadable credentials refuse bootstrap.
`EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1` is an explicit local-development opt-in,
not a production default; anyone who can reach the web listener can then log in.

On restart, keep `/config` and omit the bootstrap password. Do not run a second
direct backend process while the server owns its files. The HTTP listener is
plain HTTP: keep it on a trusted interface or put HTTPS in front of it.

### Execution modes and command override

The bootstrap script above is the image's default command, and the image sets no
separate entrypoint. Any arguments after the image name replace that script
entirely and are passed to the `eidetica` binary: no credential bootstrap runs.
To run the socket-only `daemon` (or another subcommand) in this image, initialize
`/config` first — e.g. start once with a credential as shown above, stop the
container, then run `docker run ... image daemon --socket /config/eidetica.sock`
against the same volumes. The image's built-in health check calls
`eidetica health`, an HTTP probe of `serve`/dashboard listeners; a socket-only
daemon has no HTTP listener, so that check does not apply to it.

## Durable and disposable directories

| Location                       | Contents                                                                                          | Retention                           |
| ------------------------------ | ------------------------------------------------------------------------------------------------- | ----------------------------------- |
| `/config/eidetica.db`          | History, instance secrets/metadata, local credentials, verification and Authoritative Store state | Durable; back up                    |
| `/cache/<instance-key>.sqlite` | Reconstructible Derived records, with SQLite and ownership sidecars                               | Optional; rebuildable while offline |

The cache filename is the hex encoding of the instance's existing prefixed device
public key. Initialization uses an in-memory cache until metadata is complete;
the next open selects its disk file. Independent instances can share a cache root
without colliding. Copies with the same device key need separate cache roots if
run independently.

The image provisions `/config` and `/cache` for UID/GID 1000:1000. New named Docker
volumes inherit the image directory ownership. Bind mounts hide those directories:
the administrator must provision them writable by the packaged user and arrange
read access to a mounted password file. Startup never chmods/chowns supplied
mounts. Preserve any deliberate group ownership/access policy.

A cache may contain sensitive plaintext from unencrypted Stores. Protect it as
well as history. `--cache-bytes` limits live record payload, not disk usage or a
filesystem quota. Both SQLite files retain WAL/NORMAL durability defaults.

### Disposable container cache

Omit the `/cache` mount to keep it in the writable container layer:

```bash
docker run -d --name eidetica -p 127.0.0.1:5942:5942 \
  --mount type=volume,source=eidetica-data,target=/config \
  ghcr.io/arcuru/eidetica:dev
```

This example assumes the durable volume was initialized by the quick start.
Replacing the container loses only its cache; `/config` retains the instance and
credentials. A persistent cache mount instead avoids rebuilding after replacement.

### Backups and offline recreation

Use SQLite's backup API or stop every owner/reader before copying durable history
with any outstanding WAL. Cache backup is optional. To recreate a cache, stop and
remove the container, remove only the selected cache database and its `-wal`,
`-shm` and `-journal` sidecars, then start a new container with the same `/config`.
Keep the stable `.eidetica-owner` sidecar. Never unlink/replace live databases or
locks, remove durable history, or clean an entire shared platform cache root.
See [SQLite storage](concepts/backends.md#disposable-sqlite-cache) for library
configuration, unsupported candidate formats and restore behavior.

## Docker Compose

Create `admin-password.txt` in a secure location readable by the container's UID 1000. Do not commit it. This example keeps both directories in named volumes:

```yaml
services:
  eidetica:
    image: ghcr.io/arcuru/eidetica:dev
    ports:
      - "127.0.0.1:5942:5942"
    volumes:
      - eidetica-data:/config
      - eidetica-cache:/cache
    secrets:
      - source: admin_password
        target: admin_password
    restart: unless-stopped

secrets:
  admin_password:
    file: ./admin-password.txt

volumes:
  eidetica-data:
  eidetica-cache:
```

```bash
docker compose up -d
docker compose logs eidetica
```

Remove the cache mount and its volume declaration for disposable container-local
cache storage. On an already initialized `/config`, the password source is no
longer needed for bootstrap; retain your login credentials separately.

## Configuration

| Variable                            | Description                                            | Image default |
| ----------------------------------- | ------------------------------------------------------ | ------------- |
| `EIDETICA_BACKEND`                  | `sqlite`, `postgres`, `inmemory`                       | `sqlite`      |
| `EIDETICA_DATA_DIR`                 | Durable data directory                                 | `/config`     |
| `EIDETICA_CACHE_DIR`                | Disposable SQLite directory, ignored by other backends | `/cache`      |
| `EIDETICA_HOST`                     | HTTP bind address                                      | `0.0.0.0`     |
| `EIDETICA_PORT`                     | HTTP port                                              | `5942`        |
| `EIDETICA_POSTGRES_URL`             | PostgreSQL connection URL                              | Unset         |
| `EIDETICA_ADMIN_PASSWORD`           | First-start admin credential, if no secret file        | Unset         |
| `EIDETICA_ALLOW_PASSWORDLESS_ADMIN` | Explicit insecure bootstrap opt-in                     | Unset         |

PostgreSQL retains its existing single-pool storage; `/cache` is a SQLite option.
When using PostgreSQL, supply `EIDETICA_BACKEND=postgres`, the connection URL and
the same explicit first-start credential source. Back up the PostgreSQL database,
not a SQLite file. See [CLI reference](cli.md) for cache bounds and other options.

The image's built-in health check calls `eidetica health`. It can also be invoked
manually without opening the backend:

```bash
docker exec eidetica eidetica health
```

## Building locally

```bash
just container nix
```

This builds the packaged Nix image and entrypoint described above. The maintained
Dockerfile also supports `just container docker`, but has a direct binary
entrypoint: initialize its durable volume explicitly with `daemon init --username
admin` and a credential, then run the server. It does not implement the Nix
image's automatic credential bootstrap.
