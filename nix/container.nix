# OCI container image configuration
{
  pkgs,
  eidetica-bin,
}: let
  # Non-root user setup for container security
  # Creates passwd/group/shadow files for the eidetica user (UID/GID 1000)
  nonRootUserSetup = let
    user = "eidetica";
    uid = "1000";
    gid = "1000";
  in [
    (pkgs.writeTextDir "etc/passwd" ''
      root:x:0:0:root:/root:/bin/false
      ${user}:x:${uid}:${gid}::/${user}:
    '')
    (pkgs.writeTextDir "etc/group" ''
      root:x:0:
      ${user}:x:${gid}:
    '')
    (pkgs.writeTextDir "etc/shadow" ''
      root:!x:::::::
      ${user}:!:::::::
    '')
  ];

  # License file for container compliance
  licenseFile = pkgs.runCommand "license" {} ''
    mkdir -p $out
    cp ${../LICENSE.txt} $out/LICENSE
  '';

  # Entrypoint: on first start, bootstrap the initial admin user from an
  # operator-supplied credential, then exec the daemon. `eidetica info` exits
  # non-zero only when the backend is not yet initialised, so this inits once
  # and no-ops on every subsequent start.
  #
  # Fails closed: when no credential source is configured the entrypoint
  # exits 1 with an actionable error rather than silently creating a
  # passwordless admin.
  #
  # Credential sources, in priority order:
  #   1. File at /run/secrets/admin_password (Docker / Compose / Kubernetes
  #      secret convention; preferred — keeps the password off the process
  #      table and out of `docker inspect`).
  #   2. EIDETICA_ADMIN_PASSWORD environment variable.
  #   3. EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1 — explicit opt-in to a
  #      passwordless admin (INSECURE; trusted/LAN or local dev only).
  #
  # Before any open, refuse a legacy store still mounted at /config unless the
  # operator explicitly selects that directory: otherwise a credential could
  # bootstrap a fresh /data instance that hides the existing history.
  entrypoint = pkgs.writeShellScriptBin "eidetica-entrypoint" ''
        set -eu
        # Data-bearing files of the selected file backend; PostgreSQL has none.
        has_store() {
          case "''${EIDETICA_BACKEND:-sqlite}" in
            sqlite) names="eidetica.db eidetica.db-wal eidetica.db-journal" ;;
            inmemory) names="eidetica.json" ;;
            *) return 1 ;;
          esac
          for name in $names; do
            if [ -e "$1/$name" ] || [ -L "$1/$name" ]; then return 0; fi
          done
          return 1
        }
        data_dir="''${EIDETICA_DATA_DIR:-.}"
        if ! [ /config -ef "$data_dir" ] && has_store /config; then
          if has_store "$data_dir"; then
            echo "ERROR: found Eidetica stores in both /config and $data_dir; refusing to choose." >&2
            echo "Detach whichever mount is not this instance's store (do not merge or delete" >&2
            echo "files), or select the legacy store explicitly with EIDETICA_DATA_DIR=/config." >&2
            exit 1
          fi
          echo "ERROR: found an existing store in the legacy /config location, but primary" >&2
          echo "storage is $data_dir, which has none. Refusing to start so that a fresh" >&2
          echo "instance cannot hide the existing data. Choose ONE:" >&2
          echo "  1) Mount the same volume/directory at /data instead of /config" >&2
          echo "     (nothing moves; the database keeps its sidecars):" >&2
          echo "       -v eidetica-data:/data" >&2
          echo "  2) Keep the legacy layout explicitly:" >&2
          echo "       -e EIDETICA_DATA_DIR=/config" >&2
          exit 1
        fi

        if ${eidetica-bin}/bin/eidetica info >/dev/null 2>&1; then
          exec ${eidetica-bin}/bin/eidetica "$@"
        fi

        if [ -e /run/secrets/admin_password ] || [ -L /run/secrets/admin_password ]; then
          # Check acquisition separately: a failed substitution in an inline
          # environment assignment would otherwise be masked by successful init.
          if ! admin_password="$(${pkgs.coreutils}/bin/cat /run/secrets/admin_password 2>/dev/null)"; then
            echo "ERROR: failed to read admin password file; refusing to initialise." >&2
            exit 1
          fi
          EIDETICA_ADMIN_PASSWORD="$admin_password" \
            ${eidetica-bin}/bin/eidetica daemon init --username admin
        elif [ -n "''${EIDETICA_ADMIN_PASSWORD:-}" ]; then
          ${eidetica-bin}/bin/eidetica daemon init --username admin
        elif [ "''${EIDETICA_ALLOW_PASSWORDLESS_ADMIN:-}" = "1" ]; then
          echo "WARNING: bootstrapping a PASSWORDLESS admin user 'admin'." >&2
          echo "         Anyone who can reach this service can act as admin." >&2
          ${eidetica-bin}/bin/eidetica daemon init --username admin --passwordless
        else
          ${pkgs.coreutils}/bin/cat >&2 <<'EOF'
    ERROR: refusing to bootstrap admin user without credentials.

    The eidetica container needs to initialise an admin user on first start.
    Provide ONE of:

      1) Mount an admin password file at /run/secrets/admin_password
         (preferred — keeps the password off the process table)
           -v ./admin-password.txt:/run/secrets/admin_password:ro

      2) Set EIDETICA_ADMIN_PASSWORD
           -e EIDETICA_ADMIN_PASSWORD=...
         (visible in `docker inspect`; use a secret store when possible)

      3) Opt in to a PASSWORDLESS admin (INSECURE; anyone reaching the
         service is admin) — only for local/dev:
           -e EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1
    EOF
          exit 1
        fi

        exec ${eidetica-bin}/bin/eidetica "$@"
  '';

  # OCI container image
  eidetica-image = pkgs.dockerTools.buildImage {
    name = "eidetica";
    tag = "dev";
    created = "now";

    copyToRoot = pkgs.buildEnv {
      name = "image-root";
      paths = [eidetica-bin licenseFile entrypoint] ++ nonRootUserSetup;
      pathsToLink = ["/bin" "/etc" "/"];
    };

    # Image-owned defaults are writable by the packaged UID/GID: /config for
    # configuration, /data for primary storage, /cache for disposable records.
    # Bind mounts retain their administrator-provisioned permissions; startup
    # never changes them.
    runAsRoot = ''
      #!${pkgs.runtimeShell}
      mkdir -p /config /data /cache
      chown 1000:1000 /config /data /cache
    '';

    config = {
      Cmd = ["${entrypoint}/bin/eidetica-entrypoint"];
      User = "1000:1000";
      WorkingDir = "/data";
      ExposedPorts = {
        "5942/tcp" = {};
      };
      Env = [
        "EIDETICA_DATA_DIR=/data"
        "EIDETICA_CACHE_DIR=/cache"
        "EIDETICA_HOST=0.0.0.0"
        "HOME=/tmp" # should be unused, set to /tmp for safety
      ];
      Healthcheck = {
        Test = ["CMD" "${eidetica-bin}/bin/eidetica" "health"];
        # These need to be in nanoseconds, written here as multiplications for clarity
        Interval = 30 * 1000 * 1000 * 1000; # 30s
        Timeout = 5 * 1000 * 1000 * 1000; # 5s
        StartPeriod = 5 * 1000 * 1000 * 1000; # 5s
        Retries = 3;
      };
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/arcuru/eidetica";
        "org.opencontainers.image.description" = "Eidetica: Remember Everything - Decentralized Database";
        "org.opencontainers.image.licenses" = "AGPL-3.0-or-later";
      };
    };
  };
in {
  # Export as 'image' for cleaner naming (wired to eidetica.image in flake)
  image = eidetica-image;
}
