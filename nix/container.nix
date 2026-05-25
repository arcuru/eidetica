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
  entrypoint = pkgs.writeShellScriptBin "eidetica-entrypoint" ''
    set -eu
    if ${eidetica-bin}/bin/eidetica info >/dev/null 2>&1; then
      exec ${eidetica-bin}/bin/eidetica "$@"
    fi

    if [ -r /run/secrets/admin_password ]; then
      EIDETICA_ADMIN_PASSWORD="$(cat /run/secrets/admin_password)" \
        ${eidetica-bin}/bin/eidetica daemon init --username admin
    elif [ -n "''${EIDETICA_ADMIN_PASSWORD:-}" ]; then
      ${eidetica-bin}/bin/eidetica daemon init --username admin
    elif [ "''${EIDETICA_ALLOW_PASSWORDLESS_ADMIN:-}" = "1" ]; then
      echo "WARNING: bootstrapping a PASSWORDLESS admin user 'admin'." >&2
      echo "         Anyone who can reach this service can act as admin." >&2
      ${eidetica-bin}/bin/eidetica daemon init --username admin --passwordless
    else
      cat >&2 <<'EOF'
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

    config = {
      Cmd = ["${entrypoint}/bin/eidetica-entrypoint"];
      User = "1000:1000";
      WorkingDir = "/config";
      ExposedPorts = {
        "3000/tcp" = {};
      };
      Env = [
        "EIDETICA_DATA_DIR=/config"
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
