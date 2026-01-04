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
    enable = mkEnableOption "Eidetica decentralized database server (user service)";

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
      default = "${config.xdg.dataHome}/eidetica";
      defaultText = literalExpression ''"''${config.xdg.dataHome}/eidetica"'';
      description = "Directory for eidetica data storage.";
    };

    postgresUrl = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "PostgreSQL connection URL (required when backend=postgres).";
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

    # Ensure data directory exists
    home.activation.createEideticaDataDir = lib.hm.dag.entryAfter ["writeBoundary"] ''
      mkdir -p "${cfg.dataDir}"
    '';

    # Systemd user service
    systemd.user.services.eidetica = {
      Unit = {
        Description = "Eidetica Decentralized Database Server";
        After = ["network-online.target"];
        Wants = ["network-online.target"];
      };

      Service = {
        Type = "simple";
        WorkingDirectory = cfg.dataDir;
        ExecStart = "${cfg.package}/bin/eidetica";
        Restart = "on-failure";
        RestartSec = "5s";
        Environment =
          [
            "EIDETICA_PORT=${toString cfg.port}"
            "EIDETICA_HOST=${cfg.host}"
            "EIDETICA_BACKEND=${cfg.backend}"
            "EIDETICA_DATA_DIR=${cfg.dataDir}"
          ]
          ++ optional (cfg.postgresUrl != null) "EIDETICA_POSTGRES_URL=${cfg.postgresUrl}"
          ++ mapAttrsToList (name: value: "${name}=${value}") cfg.environment;
      };

      Install = {
        WantedBy = ["default.target"];
      };
    };
  };
}
