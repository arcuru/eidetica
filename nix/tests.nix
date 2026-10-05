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
          # Opt in explicitly so the bootstrap assertion passes. The legacy
          # serve listener also surfaces the passwordless web warning.
          allowPasswordlessAdmin = true;
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
  nixosCombined =
    (nixosEvalEnabled.extendModules {
      modules = [
        {
          services.eidetica = {
            daemon = true;
            dashboard = true;
            host = "0.0.0.0";
            openFirewall = true;
          };
        }
      ];
    }).config.systemd.services.eidetica.serviceConfig.ExecStart;
  nixosDashboardFirewall =
    (nixosEvalEnabled.extendModules {
      modules = [
        {
          services.eidetica = {
            daemon = true;
            dashboard = true;
            openFirewall = true;
          };
        }
      ];
    }).config.networking.firewall.allowedTCPPorts;
  hmCombined =
    (hmEvalEnabled.extendModules {
      modules = [
        {
          services.eidetica = {
            daemon = true;
            dashboard = true;
            host = "0.0.0.0";
          };
        }
      ];
    }).config.systemd.user.services.eidetica.Service.ExecStart;
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

      ${
        if
          lib.hasSuffix "/bin/eidetica serve" nixosEvalEnabled.config.systemd.services.eidetica.serviceConfig.ExecStart
          && lib.hasSuffix "/bin/eidetica daemon --dashboard --dashboard-host 0.0.0.0 --dashboard-port 8080" nixosCombined
          && lib.elem 8080 nixosDashboardFirewall
        then "echo '✓ Legacy serve and combined daemon commands evaluate correctly'"
        else "echo '✗ NixOS service command mismatch' && exit 1"
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

      ${
        if
          lib.hasSuffix "/bin/eidetica serve" hmEvalEnabled.config.systemd.user.services.eidetica.Service.ExecStart
          && lib.hasSuffix "/bin/eidetica daemon --dashboard --dashboard-host 0.0.0.0 --dashboard-port 9000" hmCombined
        then "echo '✓ Legacy serve and combined daemon commands evaluate correctly'"
        else "echo '✗ Home Manager service command mismatch' && exit 1"
      }

      echo "All Home Manager module evaluation tests passed" > $out/result
    '';
  };

  integration = {
    # Multicast needs a real interface/route, which the Nix build sandbox lacks.
    # Run the same-host sync regression in a LAN-only VM, with no public DNS.
    mdns = pkgs.testers.nixosTest {
      name = "eidetica-mdns-sync";

      nodes.machine = _: {
        # The debug nextest archive expands to about 3 GiB; use disk, not /tmp's tmpfs.
        virtualisation.diskSize = 4096;
        networking = {
          dhcpcd.enable = false;
          nameservers = ["127.0.0.1"];
          firewall.allowedUDPPorts = [5353];
          interfaces.eth1.ipv4.routes = [
            {
              address = "224.0.0.0";
              prefixLength = 4;
            }
          ];
        };
        environment.systemPackages = [pkgs.cargo-nextest];
      };

      testScript = ''
        machine.start()
        machine.wait_for_unit("multi-user.target")
        machine.succeed("mkdir -p /var/lib/nextest && cp -r ${testPkgs.src} /tmp/src && chmod -R u+w /tmp/src")
        result = machine.succeed(
          "cargo-nextest nextest run "
          "--archive-file ${testPkgs.archive}/archive.tar.zst "
          "--extract-to /var/lib/nextest --workspace-remap /tmp/src "
          "--show-progress=none --run-ignored only --no-tests fail "
          "-E 'test(=sync::iroh_e2e_test::test_iroh_mdns_same_host_sync)'",
          timeout=60,
        )
        machine.log(result)
        machine.log("mDNS-only same-host sync integration test passed!")
      '';
    };

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
          # VM test runs against a fresh backend with no operator-supplied
          # credential; opt in to the passwordless bootstrap so the
          # ExecStartPre check succeeds.
          allowPasswordlessAdmin = true;
        };

        # Ensure networking is available
        networking.firewall.allowedTCPPorts = [5942];
      };

      testScript = ''
        machine.start()
        machine.wait_for_unit("eidetica.service")
        machine.wait_for_open_port(5942)

        # Verify the service responds (follow redirects since / redirects to /login)
        result = machine.succeed("curl -fL http://localhost:5942/")
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
        nativeBuildInputs = [eidetica-bin pkgs.cargo-nextest pkgs.curl];
      } ''
        SOCKET="$TMPDIR/test.sock"
        DATA="$TMPDIR/data"

        # The daemon refuses to run an uninitialised backend, so initialise a
        # fresh sqlite instance first, then start the daemon against the same
        # data dir. (inmemory can't be used here: init and daemon are separate
        # processes and an in-memory backend wouldn't persist between them.)
        eidetica daemon --backend sqlite --data-dir "$DATA" init \
          --username admin --passwordless
        DAEMON_LOG="$TMPDIR/daemon.log"
        eidetica daemon --backend sqlite --data-dir "$DATA" --socket "$SOCKET" \
          --dashboard --dashboard-port 0 \
          >"$DAEMON_LOG" 2>&1 &
        DAEMON_PID=$!
        trap 'if [ -n "$DAEMON_PID" ]; then kill "$DAEMON_PID" 2>/dev/null || true; wait "$DAEMON_PID" 2>/dev/null || true; fi' EXIT

        # Wait for both the service socket and unconditional sync startup.
        for i in $(seq 1 50); do
          if [ -S "$SOCKET" ] && grep -q "Daemon sync listener started" "$DAEMON_LOG"; then
            break
          fi
          if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
            echo "Daemon exited prematurely"
            cat "$DAEMON_LOG"
            exit 1
          fi
          sleep 0.1
        done

        if [ ! -S "$SOCKET" ] || ! grep -q "Daemon sync listener started" "$DAEMON_LOG"; then
          echo "Timed out waiting for daemon startup"
          cat "$DAEMON_LOG"
          exit 1
        fi

        # Only the dashboard routes are exposed; no trusted service RPC on HTTP.
        PORT=$(sed -n 's/.*Dashboard listening on http:\/\/127.0.0.1:\([0-9]*\).*/\1/p' "$DAEMON_LOG" | head -1)
        test -n "$PORT"
        curl -fsS "http://127.0.0.1:$PORT/health" | grep -q '"backend":"sqlite"'
        test "$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/v0")" = 404
        test "$(curl -s -o /dev/null -w '%{http_code}' -X POST -d 'username=admin' "http://127.0.0.1:$PORT/login")" = 403
        curl -fsS -D "$TMPDIR/login-headers" -c "$TMPDIR/admin-cookies" -o /dev/null -X POST \
          -H "Origin: http://127.0.0.1:$PORT" -d 'username=admin' "http://127.0.0.1:$PORT/login"
        grep -qi 'samesite=strict' "$TMPDIR/login-headers"
        grep -qi 'httponly' "$TMPDIR/login-headers"
        # No Secure flag on direct HTTP; verify same-origin writes and CSRF denials.
        if grep -qi '; secure' "$TMPDIR/login-headers"; then
          echo "Direct HTTP login set an unusable Secure cookie"
          exit 1
        fi
        test "$(curl -s -o /dev/null -w '%{http_code}' -X POST \
          -d 'username=attacker' "http://127.0.0.1:$PORT/register")" = 403
        test "$(curl -s -o /dev/null -w '%{http_code}' -b "$TMPDIR/admin-cookies" -X POST \
          "http://127.0.0.1:$PORT/logout")" = 403
        test "$(curl -s -o /dev/null -w '%{http_code}' -b "$TMPDIR/admin-cookies" -X POST \
          -H "Origin: http://127.0.0.1:$PORT" -d 'ticket=invalid&permission=read' \
          "http://127.0.0.1:$PORT/dashboard/track")" = 400

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

        # A separate process writes through the *external* daemon socket.
        export EIDETICA_EXTERNAL_SOCKET="$SOCKET"
        export EIDETICA_EXTERNAL_DB_ID_FILE="$TMPDIR/external-db-id"
        cargo-nextest nextest run \
          --archive-file ${testPkgs.archive}/archive.tar.zst \
          --workspace-remap "$TMPDIR/src" --show-progress=none \
          -E 'test(=socket_write_visible_to_dashboard)'
        curl -fsS -c "$TMPDIR/admin-cookies" -o /dev/null -X POST \
          -H "Origin: http://127.0.0.1:$PORT" -d 'username=admin' "http://127.0.0.1:$PORT/login"
        curl -fsS -b "$TMPDIR/admin-cookies" "http://127.0.0.1:$PORT/dashboard" > "$TMPDIR/dashboard.html"
        test -s "$EIDETICA_EXTERNAL_DB_ID_FILE"
        if ! grep -q "$(cat "$EIDETICA_EXTERNAL_DB_ID_FILE")" "$TMPDIR/dashboard.html"; then
          echo "Dashboard did not show database written over external socket"
          cat "$TMPDIR/dashboard.html"
          cat "$DAEMON_LOG"
          exit 1
        fi
        test "$(curl -s -o /dev/null -w '%{http_code}' -b "$TMPDIR/admin-cookies" -X POST \
          -H 'Origin: http://attacker.invalid' -d 'ticket=bad&permission=read' \
          "http://127.0.0.1:$PORT/dashboard/track")" = 403

        kill -TERM "$DAEMON_PID"
        wait "$DAEMON_PID"
        DAEMON_PID=""
        grep -q "All sync servers stopped" "$DAEMON_LOG"
        grep -q "Daemon shut down" "$DAEMON_LOG"

        # The dashboard listens exactly where configured. A wildcard bind accepts
        # whatever Host the browser uses; form writes still need a matching
        # Origin. The same socket and persisted instance survive a restart.
        DAEMON_LOG="$TMPDIR/daemon-public.log"
        eidetica daemon --backend sqlite --data-dir "$DATA" --socket "$SOCKET" \
          --dashboard --dashboard-host 0.0.0.0 --dashboard-port "$PORT" >"$DAEMON_LOG" 2>&1 &
        DAEMON_PID=$!
        for i in $(seq 1 50); do
          if curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
            break
          fi
          if ! kill -0 "$DAEMON_PID" 2>/dev/null; then cat "$DAEMON_LOG"; exit 1; fi
          sleep 0.1
        done
        grep -q "Dashboard listening on http://0.0.0.0:$PORT" "$DAEMON_LOG"
        # /proc/net/tcp records the kernel listener address in hex.
        awk -v port="$(printf '%04X' "$PORT")" \
          '$2 == "00000000:" port && $4 == "0A" { found=1 } END { exit !found }' /proc/net/tcp
        curl -fsS -H "Host: db.example:$PORT" "http://127.0.0.1:$PORT/health" | grep -q '"backend":"sqlite"'
        test "$(curl -s -o /dev/null -w '%{http_code}' -H "Host: db.example:$PORT" \
          -H "Origin: http://attacker.invalid" -d 'username=admin' "http://127.0.0.1:$PORT/login")" = 403
        curl -fsS -D "$TMPDIR/public-login-headers" -c "$TMPDIR/public-cookies" -o /dev/null \
          -H "Host: db.example" -H "Origin: https://db.example" -d 'username=admin' \
          "http://127.0.0.1:$PORT/login"
        # Behind a TLS proxy the browser origin is HTTPS, so the cookie is Secure.
        grep -qi '; secure' "$TMPDIR/public-login-headers"
        curl -fsS -c "$TMPDIR/public-cookies" -o /dev/null -H "Host: db.example:$PORT" \
          -H "Origin: http://db.example:$PORT" -d 'username=admin' "http://127.0.0.1:$PORT/login"
        curl -fsS -b "$TMPDIR/public-cookies" -H "Host: db.example:$PORT" \
          "http://127.0.0.1:$PORT/dashboard" | grep -q "$(cat "$EIDETICA_EXTERNAL_DB_ID_FILE")"
        kill -TERM "$DAEMON_PID"
        wait "$DAEMON_PID"
        DAEMON_PID=""

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
        # Opt in to the passwordless-admin bootstrap explicitly; the
        # entrypoint now fails closed without a credential source.
        machine.succeed(
          "podman run -d --name eidetica-test -p 5942:5942 "
          "-e EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1 "
          "-v /var/lib/eidetica-data:/data eidetica:dev"
        )

        # Wait for container to start
        import time
        time.sleep(3)

        # Check container is running
        machine.succeed("podman ps | grep eidetica-test")

        # Verify the service responds (follow redirects since / redirects to /login)
        machine.wait_until_succeeds("curl -fL http://localhost:5942/", timeout=30)

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
