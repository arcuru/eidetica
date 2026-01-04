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

    # Create user and group
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
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
