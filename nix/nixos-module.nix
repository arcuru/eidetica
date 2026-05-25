{
  config,
  lib,
  pkgs,
  ...
}:
with lib; let
  cfg = config.services.eidetica;
in {
  options.services.eidetica = {
    enable = mkEnableOption "Eidetica decentralized database server";

    package = mkOption {
      type = types.package;
      default = pkgs.eidetica;
      defaultText = literalExpression "pkgs.eidetica";
      description = "The eidetica package to use.";
    };

    port = mkOption {
      type = types.port;
      default = 3000;
      description = "Port for the eidetica server to listen on.";
    };

    host = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = "Bind address. Use 0.0.0.0 for all interfaces.";
    };

    backend = mkOption {
      type = types.enum ["sqlite" "postgres" "inmemory"];
      default = "sqlite";
      description = "Storage backend to use.";
    };

    dataDir = mkOption {
      type = types.path;
      default = "/var/lib/eidetica";
      description = "Directory for eidetica data storage.";
    };

    postgresUrl = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "PostgreSQL connection URL (required when backend=postgres).";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Whether to open the firewall port for eidetica.";
    };

    user = mkOption {
      type = types.str;
      default = "eidetica";
      description = "User account under which eidetica runs.";
    };

    group = mkOption {
      type = types.str;
      default = "eidetica";
      description = "Group under which eidetica runs.";
    };

    initialUser = mkOption {
      type = types.str;
      default = "admin";
      description = ''
        Username of the initial admin user created on first start.

        The service requires an initialised backend with an admin user. On
        first start (when the backend is not yet initialised) the service
        bootstraps this user; subsequent starts detect the existing instance
        and skip initialisation. The bootstrap credential is controlled by
        `initialPasswordFile` (recommended) or `allowPasswordlessAdmin` —
        exactly one of those must be set, or the module refuses to evaluate.
      '';
    };

    initialPasswordFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/run/agenix/eidetica-admin-password";
      description = ''
        Path to a file containing the initial admin user's password. The
        file is read by systemd via `LoadCredential` (so it can live on a
        root-owned secret store outside the service user's reach) and
        passed to `eidetica daemon init` via the `EIDETICA_ADMIN_PASSWORD`
        environment variable on first start.

        Exactly one of `initialPasswordFile` or `allowPasswordlessAdmin`
        must be set; without either, the module fails closed at evaluation
        time rather than silently creating a passwordless admin.
      '';
    };

    allowPasswordlessAdmin = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Opt in to bootstrapping the initial admin user **without a
        password**. A passwordless admin means anyone who can reach the
        service can act as admin, so only set this for trusted/LAN
        deployments or local development. Use `initialPasswordFile`
        instead for any deployment that exposes the service beyond
        loopback.
      '';
    };

    environment = mkOption {
      type = types.attrsOf types.str;
      default = {};
      description = "Additional environment variables for the service.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.backend != "postgres" || cfg.postgresUrl != null;
        message = "services.eidetica.postgresUrl is required when backend is postgres";
      }
      {
        # XOR: exactly one of initialPasswordFile / allowPasswordlessAdmin.
        # Both unset → fail closed (no silent passwordless bootstrap).
        # Both set → ambiguous; reject.
        assertion = (cfg.initialPasswordFile != null) != cfg.allowPasswordlessAdmin;
        message = ''
          services.eidetica requires exactly one of the following to bootstrap the initial admin user on first start:

            services.eidetica.initialPasswordFile = "/path/to/password-file";
              (recommended; the file is read via systemd LoadCredential)

            services.eidetica.allowPasswordlessAdmin = true;
              (INSECURE — anyone reaching the service becomes admin; trusted/LAN only)

          Without one of these the service would either crash-loop on a fresh backend or silently create a passwordless admin, so the module refuses to evaluate.
        '';
      }
    ];

    # Warn loudly when the operator opted into a passwordless admin AND the
    # service is reachable beyond loopback. The assertion above already
    # ensures the opt-in is explicit; this just makes the consequence visible
    # at rebuild time for the dangerous combination.
    warnings = optional (cfg.allowPasswordlessAdmin && cfg.host != "127.0.0.1") ''
      services.eidetica.allowPasswordlessAdmin is true and host is ${cfg.host} (not loopback).
      Anyone who can reach the service can act as admin. Restrict access (firewall /
      reverse proxy with auth), or switch to services.eidetica.initialPasswordFile.
    '';

    # Create user and group
    users.users.${cfg.user} = {
      isSystemUser = true;
      inherit (cfg) group;
      home = cfg.dataDir;
      createHome = true;
      description = "Eidetica service user";
    };

    users.groups.${cfg.group} = {};

    # Systemd service
    systemd.services.eidetica = {
      description = "Eidetica Decentralized Database Server";
      after = ["network.target"];
      wantedBy = ["multi-user.target"];

      environment =
        {
          EIDETICA_PORT = toString cfg.port;
          EIDETICA_HOST = cfg.host;
          EIDETICA_BACKEND = cfg.backend;
          EIDETICA_DATA_DIR = cfg.dataDir;
        }
        // optionalAttrs (cfg.postgresUrl != null) {
          EIDETICA_POSTGRES_URL = cfg.postgresUrl;
        }
        // cfg.environment;

      serviceConfig =
        {
          Type = "simple";
          User = cfg.user;
          Group = cfg.group;
          WorkingDirectory = cfg.dataDir;

          # Bootstrap the initial admin user on first start. `eidetica info`
          # exits non-zero only when the backend is not yet initialised, so this
          # is idempotent: it inits once, then no-ops on every subsequent start.
          # The credential source is decided at module-evaluation time by the
          # initialPasswordFile / allowPasswordlessAdmin assertion above.
          ExecStartPre = let
            initIfNeeded = pkgs.writeShellScript "eidetica-init-if-needed" (
              ''
                set -eu
                if ${cfg.package}/bin/eidetica info >/dev/null 2>&1; then
                  exit 0
                fi
              ''
              + (
                if cfg.initialPasswordFile != null
                then ''
                  EIDETICA_ADMIN_PASSWORD="$(cat "$CREDENTIALS_DIRECTORY/admin-password")" \
                    ${cfg.package}/bin/eidetica daemon init \
                      --username ${escapeShellArg cfg.initialUser}
                ''
                else ''
                  echo "WARNING: bootstrapping a PASSWORDLESS admin user (${escapeShellArg cfg.initialUser})." >&2
                  echo "         Anyone who can reach the service can act as admin." >&2
                  ${cfg.package}/bin/eidetica daemon init \
                    --username ${escapeShellArg cfg.initialUser} \
                    --passwordless
                ''
              )
            );
          in "${initIfNeeded}";

          ExecStart = "${cfg.package}/bin/eidetica";
          Restart = "on-failure";
          RestartSec = "5s";

          # Security hardening
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          ReadWritePaths = [cfg.dataDir];
        }
        // optionalAttrs (cfg.initialPasswordFile != null) {
          # systemd reads the file as root and exposes it to the service at
          # $CREDENTIALS_DIRECTORY/admin-password, so the unprivileged
          # service user never needs read access to the original file.
          LoadCredential = ["admin-password:${cfg.initialPasswordFile}"];
        };
    };

    # Firewall configuration
    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [cfg.port];
  };
}
