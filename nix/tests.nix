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
  testPkgs,
  eidetica-bin,
  eidetica-image,
  nixosModule,
  homeManagerModule,
}: let
  # Evaluate NixOS module with service disabled
  nixosEvalDisabled = lib.nixosSystem {
    inherit (pkgs) system;
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
    inherit (pkgs) system;
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
  eval = {
    # Fast module evaluation test for NixOS module
    # Evaluates the module at flake eval time and writes results
    nixos = pkgs.runCommand "eval-nixos" {} ''
      mkdir -p $out

      echo "NixOS module evaluation test"

      # Test 1: Service disabled (default)
      echo "Service disabled: ${lib.boolToString nixosDisabledResult}" > $out/disabled.txt
      ${
        if !nixosDisabledResult
        then "echo '✓ Module evaluates correctly with service disabled'"
        else "echo '✗ Service should be disabled by default' && exit 1"
      }

      # Test 2: Service enabled with custom config
      echo "Service enabled: ${lib.boolToString nixosEnabledResult.enable}" > $out/enabled.txt
      echo "Port: ${toString nixosEnabledResult.port}" >> $out/enabled.txt
      echo "Backend: ${nixosEnabledResult.backend}" >> $out/enabled.txt
      echo "Host: ${nixosEnabledResult.host}" >> $out/enabled.txt
      ${
        if nixosEnabledResult.enable && nixosEnabledResult.port == 8080 && nixosEnabledResult.backend == "sqlite"
        then "echo '✓ Module evaluates correctly with service enabled'"
        else "echo '✗ Service configuration mismatch' && exit 1"
      }

      echo "All NixOS module evaluation tests passed" > $out/result
    '';

    # Fast module evaluation test for Home Manager module
    hm = pkgs.runCommand "eval-hm" {} ''
      mkdir -p $out

      echo "Home Manager module evaluation test"

      # Test 1: Service disabled (default)
      echo "Service disabled: ${lib.boolToString hmDisabledResult}" > $out/disabled.txt
      ${
        if !hmDisabledResult
        then "echo '✓ Module evaluates correctly with service disabled'"
        else "echo '✗ Service should be disabled by default' && exit 1"
      }

      # Test 2: Service enabled with custom config
      echo "Service enabled: ${lib.boolToString hmEnabledResult.enable}" > $out/enabled.txt
      echo "Port: ${toString hmEnabledResult.port}" >> $out/enabled.txt
      echo "Backend: ${hmEnabledResult.backend}" >> $out/enabled.txt
      ${
        if hmEnabledResult.enable && hmEnabledResult.port == 9000 && hmEnabledResult.backend == "sqlite"
        then "echo '✓ Module evaluates correctly with service enabled'"
        else "echo '✗ Service configuration mismatch' && exit 1"
      }

      echo "All Home Manager module evaluation tests passed" > $out/result
    '';
  };

  integration = {
    # Full NixOS VM integration test
    # Boots a VM, starts the service, and verifies it responds
    nixos = pkgs.testers.nixosTest {
      name = "eidetica-nixos-service";

      nodes.machine = _: {
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

        # Verify the service responds (follow redirects since / redirects to /login)
        result = machine.succeed("curl -fL http://localhost:3000/")
        machine.log(f"HTTP response: {result}")

        # Verify service is running as correct user
        machine.succeed("pgrep -u eidetica eidetica")

        # Verify data directory exists
        machine.succeed("test -d /var/lib/eidetica")

        machine.log("NixOS service integration test passed!")
      '';
    };

    # Service daemon integration test
    # Starts the real eidetica daemon binary, then runs a smoke test against it.
    #
    # Why a smoke test instead of the full suite?
    # The daemon hosts a single shared Instance/backend. Every test that calls
    # test_backend() gets a RemoteBackend connected to that same Instance, so
    # state (users, databases) leaks between tests. Most tests assume a clean
    # backend (e.g. they all create_user("test_user")), so running them all
    # against one daemon causes widespread collisions. The full test suite with
    # per-test backend isolation is already covered by `nix build .#test.service`.
    #
    # This test validates what test.service cannot: that the actual compiled
    # `eidetica daemon` binary starts, binds a socket, and serves requests
    # correctly over the real Unix socket protocol.
    service =
      pkgs.runCommand "integration-service" {
        nativeBuildInputs = [eidetica-bin pkgs.cargo-nextest];
      } ''
        SOCKET="$TMPDIR/test.sock"

        # Start the daemon with inmemory backend
        eidetica daemon --backend inmemory --socket "$SOCKET" &
        DAEMON_PID=$!
        trap 'kill "$DAEMON_PID" 2>/dev/null || true; wait "$DAEMON_PID" 2>/dev/null || true' EXIT

        # Wait for the socket to appear
        for i in $(seq 1 50); do
          if [ -S "$SOCKET" ]; then
            break
          fi
          if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
            echo "Daemon exited prematurely"
            exit 1
          fi
          sleep 0.1
        done

        if [ ! -S "$SOCKET" ]; then
          echo "Timed out waiting for daemon socket"
          exit 1
        fi

        echo "Daemon started (pid=$DAEMON_PID, socket=$SOCKET)"

        # Copy source to a writable location (nextest creates target/nextest/ in the workspace)
        cp -r ${testPkgs.src} "$TMPDIR/src"
        chmod -R u+w "$TMPDIR/src"

        # Run a focused smoke test against the external daemon.
        # All tests share one daemon so we run a single representative test that
        # exercises user creation, login, key generation, database creation, and
        # store operations -- validating the full protocol stack end-to-end.
        export TEST_BACKEND=service
        export EIDETICA_SOCKET="$SOCKET"
        cargo-nextest nextest run \
          --archive-file ${testPkgs.archive}/archive.tar.zst \
          --workspace-remap "$TMPDIR/src" \
          --show-progress=none \
          -E 'test(=user::user_lifecycle_tests::test_complete_lifecycle_passwordless)'

        echo "Service integration test passed"
        mkdir -p $out
        echo "passed" > $out/result
      '';

    # OCI container integration test
    # Loads the container image and verifies it starts and responds
    container = pkgs.testers.nixosTest {
      name = "eidetica-oci-container";

      nodes.machine = _: {
        virtualisation.podman.enable = true;
      };

      testScript = ''
        machine.start()
        machine.wait_for_unit("multi-user.target")

        # Load the image (this triggers podman socket activation)
        machine.succeed("podman load < ${eidetica-image}")

        # Create data directory with correct ownership for container user (1000:1000)
        # Use /var/lib instead of /tmp to avoid tmpfs issues with SQLite WAL mode
        machine.succeed("mkdir -p /var/lib/eidetica-data")
        machine.succeed("chown 1000:1000 /var/lib/eidetica-data")
        machine.succeed(
          "podman run -d --name eidetica-test -p 3000:3000 -v /var/lib/eidetica-data:/data eidetica:dev"
        )

        # Wait for container to start
        import time
        time.sleep(3)

        # Check container is running
        machine.succeed("podman ps | grep eidetica-test")

        # Verify the service responds (follow redirects since / redirects to /login)
        machine.wait_until_succeeds("curl -fL http://localhost:3000/", timeout=30)

        # Check container logs
        logs = machine.succeed("podman logs eidetica-test")
        machine.log(f"Container logs: {logs}")

        # Stop and cleanup
        machine.succeed("podman stop eidetica-test")
        machine.succeed("podman rm eidetica-test")

        machine.log("OCI container integration test passed!")
      '';
    };
  };
}
