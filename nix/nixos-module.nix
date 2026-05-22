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
        bootstraps this user as a PASSWORDLESS admin. Subsequent starts detect
        the existing instance and skip initialisation.

        Security: a passwordless admin means anyone who can reach the service
        can act as admin. This is fine for a trusted/LAN deployment but unsafe
        on a network-exposed bind (`host = "0.0.0.0"`), which emits a warning.
        An operator-supplied-credential bootstrap is planned (P0 follow-up).
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
    ];

    # Loudly surface the passwordless-admin bootstrap at rebuild time. The
    # initial admin (`initialUser`) is created WITHOUT a password, so anyone
    # who can reach the service can act as admin. This is acceptable for a
    # trusted/LAN deployment but unsafe on a network-exposed (0.0.0.0) bind.
    #
    # FIXME(security, P0): replace the unconditional passwordless bootstrap
    # with an operator-supplied credential (e.g. an `initialPasswordFile`
    # read via systemd LoadCredential), and fail closed when neither a
    # password source nor an explicit passwordless opt-in is configured.
    # Tracked in ../private_docs/service-deployment-followups.md.
    warnings = optional (cfg.host != "127.0.0.1") ''
      services.eidetica bootstraps a PASSWORDLESS admin user ("${cfg.initialUser}")
      on first start, but is bound to ${cfg.host} (not loopback). Any host that
      can reach it can act as admin. Restrict access (firewall / reverse proxy
      with auth) or wait for the operator-supplied-credential bootstrap.
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

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        WorkingDirectory = cfg.dataDir;

        # Bootstrap the initial admin user on first start. `eidetica info`
        # exits non-zero only when the backend is not yet initialised, so this
        # is idempotent: it inits once, then no-ops on every subsequent start.
        ExecStartPre = let
          initIfNeeded = pkgs.writeShellScript "eidetica-init-if-needed" ''
            if ! ${cfg.package}/bin/eidetica info >/dev/null 2>&1; then
              ${cfg.package}/bin/eidetica daemon init \
                --username ${escapeShellArg cfg.initialUser} \
                --passwordless
            fi
          '';
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
      };
    };

    # Firewall configuration
    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [cfg.port];
  };
}
