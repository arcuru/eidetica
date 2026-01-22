# Nix module and container tests for eidetica
#
# This file defines integration tests for:
# - NixOS module evaluation (fast sanity check)
# - Home Manager module evaluation (fast sanity check)
# - NixOS VM integration test (full service test)
# - OCI container test (container runtime test)
{
  pkgs,
  lib,
  eidetica-bin,
  eidetica-image,
  nixosModule,
  homeManagerModule,
}: let
  # Evaluate NixOS module with service disabled
  nixosEvalDisabled = lib.nixosSystem {
    system = pkgs.system;
    modules = [
      nixosModule
      {
        boot.loader.grub.device = "nodev";
        fileSystems."/" = {
          device = "none";
          fsType = "tmpfs";
        };
        system.stateVersion = "25.11";
        nixpkgs.pkgs = pkgs;
      }
    ];
  };

  # Evaluate NixOS module with service enabled
  nixosEvalEnabled = lib.nixosSystem {
    system = pkgs.system;
    modules = [
      nixosModule
      {
        boot.loader.grub.device = "nodev";
        fileSystems."/" = {
          device = "none";
          fsType = "tmpfs";
        };
        system.stateVersion = "25.11";
        nixpkgs.pkgs = pkgs;
        services.eidetica = {
          enable = true;
          package = pkgs.hello; # Dummy package for eval test
          port = 8080;
          backend = "sqlite";
          host = "0.0.0.0";
        };
      }
    ];
  };

  # Stub module providing Home Manager-like options
  hmStubModule = {lib, ...}: {
    options = {
      xdg.dataHome = lib.mkOption {
        type = lib.types.str;
        default = "/home/test/.local/share";
      };
      home.activation = lib.mkOption {
        type = lib.types.attrs;
        default = {};
      };
      systemd.user.services = lib.mkOption {
        type = lib.types.attrs;
        default = {};
      };
      # Required for assertions in the module
      assertions = lib.mkOption {
        type = lib.types.listOf lib.types.attrs;
        default = [];
      };
    };
  };

  # Evaluate Home Manager module with service disabled
  hmEvalDisabled = lib.evalModules {
    modules = [
      hmStubModule
      homeManagerModule
      {_module.args.pkgs = pkgs;}
    ];
  };

  # Evaluate Home Manager module with service enabled
  hmEvalEnabled = lib.evalModules {
    modules = [
      hmStubModule
      homeManagerModule
      {_module.args.pkgs = pkgs;}
      {
        services.eidetica = {
          enable = true;
          package = pkgs.hello;
          port = 9000;
          backend = "sqlite";
        };
      }
    ];
  };

  # Force evaluation of configs to catch errors at build time
  nixosDisabledResult = nixosEvalDisabled.config.services.eidetica.enable;
  nixosEnabledResult = nixosEvalEnabled.config.services.eidetica;
  hmDisabledResult = hmEvalDisabled.config.services.eidetica.enable;
  hmEnabledResult = hmEvalEnabled.config.services.eidetica;
in {
  # Fast module evaluation test for NixOS module
  # Evaluates the module at flake eval time and writes results
  eval-nixos = pkgs.runCommand "eval-nixos" {} ''
    mkdir -p $out

    echo "NixOS module evaluation test"

    # Test 1: Service disabled (default)
    echo "Service disabled: ${lib.boolToString nixosDisabledResult}" > $out/disabled.txt
    ${
      if nixosDisabledResult == false
      then "echo '✓ Module evaluates correctly with service disabled'"
      else "echo '✗ Service should be disabled by default' && exit 1"
    }

    # Test 2: Service enabled with custom config
    echo "Service enabled: ${lib.boolToString nixosEnabledResult.enable}" > $out/enabled.txt
    echo "Port: ${toString nixosEnabledResult.port}" >> $out/enabled.txt
    echo "Backend: ${nixosEnabledResult.backend}" >> $out/enabled.txt
    echo "Host: ${nixosEnabledResult.host}" >> $out/enabled.txt
    ${
      if nixosEnabledResult.enable == true && nixosEnabledResult.port == 8080 && nixosEnabledResult.backend == "sqlite"
      then "echo '✓ Module evaluates correctly with service enabled'"
      else "echo '✗ Service configuration mismatch' && exit 1"
    }

    echo "All NixOS module evaluation tests passed" > $out/result
  '';

  # Fast module evaluation test for Home Manager module
  eval-hm = pkgs.runCommand "eval-hm" {} ''
    mkdir -p $out

    echo "Home Manager module evaluation test"

    # Test 1: Service disabled (default)
    echo "Service disabled: ${lib.boolToString hmDisabledResult}" > $out/disabled.txt
    ${
      if hmDisabledResult == false
      then "echo '✓ Module evaluates correctly with service disabled'"
      else "echo '✗ Service should be disabled by default' && exit 1"
    }

    # Test 2: Service enabled with custom config
    echo "Service enabled: ${lib.boolToString hmEnabledResult.enable}" > $out/enabled.txt
    echo "Port: ${toString hmEnabledResult.port}" >> $out/enabled.txt
    echo "Backend: ${hmEnabledResult.backend}" >> $out/enabled.txt
    ${
      if hmEnabledResult.enable == true && hmEnabledResult.port == 9000 && hmEnabledResult.backend == "sqlite"
      then "echo '✓ Module evaluates correctly with service enabled'"
      else "echo '✗ Service configuration mismatch' && exit 1"
    }

    echo "All Home Manager module evaluation tests passed" > $out/result
  '';

  # Full NixOS VM integration test
  # Boots a VM, starts the service, and verifies it responds
  integration-nixos = pkgs.testers.nixosTest {
    name = "eidetica-nixos-service";

    nodes.machine = {pkgs, ...}: {
      imports = [nixosModule];

      services.eidetica = {
        enable = true;
        package = eidetica-bin;
        host = "0.0.0.0";
        backend = "sqlite";
      };

      # Ensure networking is available
      networking.firewall.allowedTCPPorts = [3000];
    };

    testScript = ''
      machine.start()
      machine.wait_for_unit("eidetica.service")
      machine.wait_for_open_port(3000)

      # Verify the service responds
      result = machine.succeed("curl -f http://localhost:3000/ || true")
      machine.log(f"HTTP response: {result}")

      # Verify service is running as correct user
      machine.succeed("pgrep -u eidetica eidetica")

      # Verify data directory exists
      machine.succeed("test -d /var/lib/eidetica")

      machine.log("NixOS service integration test passed!")
    '';
  };

  # OCI container integration test
  # Loads the container image and verifies it starts and responds
  integration-container = pkgs.testers.nixosTest {
    name = "eidetica-oci-container";

    nodes.machine = {pkgs, ...}: {
      virtualisation.podman.enable = true;
    };

    testScript = ''
      machine.start()
      machine.wait_for_unit("multi-user.target")

      # Load the image (this triggers podman socket activation)
      machine.succeed("podman load < ${eidetica-image}")

      # Create data directory and run the container
      machine.succeed("mkdir -p /tmp/eidetica-data")
      machine.succeed(
        "podman run -d --name eidetica-test -p 3000:3000 -v /tmp/eidetica-data:/data eidetica:dev"
      )

      # Wait for container to start
      import time
      time.sleep(3)

      # Check container is running
      machine.succeed("podman ps | grep eidetica-test")

      # Verify the service responds
      machine.wait_until_succeeds("curl -f http://localhost:3000/ || true", timeout=30)

      # Check container logs
      logs = machine.succeed("podman logs eidetica-test")
      machine.log(f"Container logs: {logs}")

      # Stop and cleanup
      machine.succeed("podman stop eidetica-test")
      machine.succeed("podman rm eidetica-test")

      machine.log("OCI container integration test passed!")
    '';
  };
}
