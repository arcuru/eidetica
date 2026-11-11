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
        # The workspace debug archive expands to about 5 GiB; use disk, not /tmp's tmpfs.
        virtualisation.diskSize = 8192;
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
        machine.succeed(
          "mkdir -p /var/lib/nextest && cp -r ${testPkgs.src} /tmp/src && chmod -R u+w /tmp/src",
          timeout=300,
        )
        # Archive extraction is setup, not part of the sync behavior deadline.
        machine.succeed(
          "tar --use-compress-program=${pkgs.zstd}/bin/zstd "
          "-xf ${testPkgs.archive}/archive.tar.zst -C /var/lib/nextest",
          timeout=300,
        )
        result = machine.succeed(
          "cargo-nextest nextest run "
          "--cargo-metadata /var/lib/nextest/target/nextest/cargo-metadata.json "
          "--binaries-metadata /var/lib/nextest/target/nextest/binaries-metadata.json "
          "--target-dir-remap /var/lib/nextest/target --workspace-remap /tmp/src "
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
        export EIDETICA_CACHE_DIR="$TMPDIR/cache"

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
        curl -fsS "http://127.0.0.1:$PORT/health" | grep '"backend":"sqlite"' >/dev/null
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
          -E 'test(=account::account_lifecycle_tests::test_complete_lifecycle_passwordless)'

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
        curl -fsS -H "Host: db.example:$PORT" "http://127.0.0.1:$PORT/health" | grep '"backend":"sqlite"' >/dev/null
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
          "http://127.0.0.1:$PORT/dashboard" | grep "$(cat "$EIDETICA_EXTERNAL_DB_ID_FILE")" >/dev/null
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
        environment.systemPackages = [pkgs.sqlite];
      };

      testScript = ''
        machine.start()
        machine.wait_for_unit("multi-user.target")

        # Load the image (this triggers podman socket activation)
        machine.succeed("podman load < ${eidetica-image}")

        import json

        entrypoint = json.loads(machine.succeed("podman image inspect eidetica:dev"))[0]["Config"]["Cmd"][0]

        def data(name, persistent_cache=True):
            path = "/var/lib/" + name
            machine.succeed(f"mkdir -p {path}; chown 1000:1000 {path}")
            if not persistent_cache:
                return f"-v {path}:/config"
            cache = path + "-cache"
            machine.succeed(f"mkdir -p {cache}; chown 1000:1000 {cache}")
            return f"-v {path}:/config -v {cache}:/cache"

        def uninitialised(mounts):
            status, output = machine.execute(f"podman run --rm {mounts} eidetica:dev /bin/eidetica info 2>&1")
            assert status == 1 and "not initialised" in output, "storage must remain uninitialised"

        def refused(name, mounts, options, diagnostic):
            status, output = machine.execute(
                f"podman run --name {name} {mounts} {options} eidetica:dev {entrypoint} info 2>&1"
            )
            assert status == 1, f"{name}: expected refusal exit 1, got {status}"
            assert diagnostic in output, f"{name}: missing refusal diagnostic"
            uninitialised(mounts)
            machine.succeed(f"podman rm {name}")

        # Generate synthetic credentials without printing their values or cookies.
        machine.succeed(
            "umask 077; head -c 24 /dev/urandom | base64 | tr -d '\\n' >/var/lib/admin-password; "
            "head -c 24 /dev/urandom | base64 | tr -d '\\n' >/var/lib/wrong-password; "
            "chown 1000:1000 /var/lib/admin-password"
        )
        secret = "-v /var/lib/admin-password:/run/secrets/admin_password:ro"
        mounts = data("secret-data")
        # A file takes priority over both other configured credential sources.
        machine.succeed(
            f"podman run -d --name eidetica-test -p 5942:5942 {mounts} {secret} "
            "-e EIDETICA_ADMIN_PASSWORD=unused-fallback -e EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1 eidetica:dev"
        )
        machine.wait_until_succeeds("curl -fsS http://localhost:5942/health", timeout=30)

        def login(password_file=None, passwordless=False):
            password = f"--data-urlencode password@{password_file}" if password_file else "--data 'password='"
            if passwordless:
                password = ""
            return machine.succeed(
                "curl -sS -o /dev/null -w '%{http_code}' -H 'Origin: http://localhost:5942' "
                f"--data username=admin {password} http://localhost:5942/login"
            ).strip()

        correct, empty, wrong = login("/var/lib/admin-password"), login(), login("/var/lib/wrong-password")
        machine.log(f"Secret-file login status: correct={correct}, empty={empty}, wrong={wrong}")
        assert correct == "303", "secret-file password must authenticate"
        assert empty == "200", "empty password must not authenticate"
        assert wrong == "200", "wrong password must not authenticate"
        machine.succeed("podman stop eidetica-test; podman rm eidetica-test")
        # The packaged binary used distinct durable/cache mounts with no
        # credential/history tables in C. Record both images while stopped.
        machine.succeed("test -s /var/lib/secret-data/eidetica.db")
        cache_files = machine.succeed("find /var/lib/secret-data-cache -name '*.sqlite'").splitlines()
        assert len(cache_files) == 1, "instance-key cache file must be separate from /config"
        cache_path = cache_files[0]
        assert not machine.succeed("find /var/lib/secret-data -name '*.cache' -o -name '*.sqlite'").strip()
        old_names = set(machine.succeed(f"sqlite3 {cache_path} 'SELECT namespace_id FROM store_state_namespaces WHERE status = 1'").splitlines())
        assert old_names, "real login reads must populate Derived records in /cache"
        assert machine.succeed(f"sqlite3 {cache_path} \"SELECT count(*) FROM sqlite_master WHERE name IN ('entries','local_accounts','instance_secrets','verified_tips')\"").strip() == "0"
        # Restart without any credential source: skip bootstrap and preserve admin.
        machine.succeed(f"podman run -d --name eidetica-test -p 5942:5942 {mounts} eidetica:dev")
        machine.wait_until_succeeds("curl -fsS http://localhost:5942/health", timeout=30)
        assert login("/var/lib/admin-password") == "303"
        assert login() == "200"
        machine.succeed("podman stop eidetica-test; podman rm eidetica-test")
        retained_names = set(machine.succeed(f"sqlite3 {cache_path} 'SELECT namespace_id FROM store_state_namespaces WHERE status = 1'").splitlines())
        assert old_names & retained_names, "restart must reuse immutable Snapshot namespaces"
        # Supported offline recreation: remove only this instance's C image
        # and SQLite sidecars, not its stable ownership sidecar or shared root.
        machine.succeed(f"rm -f {cache_path} {cache_path}-wal {cache_path}-shm {cache_path}-journal")
        machine.succeed("cp /var/lib/secret-data/eidetica.db /var/lib/history-before-recreate.db")
        machine.succeed(f"podman run --rm {mounts} eidetica:dev /bin/eidetica info")
        machine.succeed("cmp /var/lib/secret-data/eidetica.db /var/lib/history-before-recreate.db")
        machine.succeed(f"test -s {cache_path}")
        machine.succeed(f"podman run -d --name eidetica-test -p 5942:5942 {mounts} eidetica:dev")
        machine.wait_until_succeeds("curl -fsS http://localhost:5942/health", timeout=30)
        assert login("/var/lib/admin-password") == "303", "cache recreation must preserve native credentials"
        machine.succeed("podman stop eidetica-test; podman rm eidetica-test")

        refused("missing", data("missing-data"), "", "refusing to bootstrap admin user without credentials")
        # cat on a directory fails after the source has been selected. Neither
        # environment credentials nor passwordless opt-in may mask that failure.
        machine.succeed("mkdir /var/lib/broken-secret")
        refused(
            "read-error", data("read-error-data"),
            "-v /var/lib/broken-secret:/run/secrets/admin_password:ro "
            "-e EIDETICA_ADMIN_PASSWORD=unused-fallback -e EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1",
            "failed to read admin password file",
        )

        # An existing but unreadable file is not an absent credential source.
        machine.succeed("touch /var/lib/unreadable-secret; chmod 000 /var/lib/unreadable-secret")
        refused(
            "unreadable", data("unreadable-data"),
            "-v /var/lib/unreadable-secret:/run/secrets/admin_password:ro "
            "-e EIDETICA_ADMIN_PASSWORD=unused-fallback -e EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1",
            "failed to read admin password file",
        )

        # No /cache mount: the image-owned writable directory is disposable.
        mounts = data("env-data", persistent_cache=False)
        machine.succeed(
            f"podman run -d --name eidetica-test -p 5942:5942 {mounts} "
            '-e EIDETICA_ADMIN_PASSWORD="$(cat /var/lib/admin-password)" eidetica:dev'
        )
        machine.wait_until_succeeds("curl -fsS http://localhost:5942/health", timeout=30)
        assert login("/var/lib/admin-password") == "303"
        assert login() == "200"
        machine.succeed("podman stop eidetica-test; podman rm eidetica-test")

        machine.succeed(
            "podman run -d --name eidetica-test -p 5942:5942 " + data("passwordless-data") +
            " -e EIDETICA_ALLOW_PASSWORDLESS_ADMIN=1 eidetica:dev"
        )
        machine.wait_until_succeeds("curl -fsS http://localhost:5942/health", timeout=30)
        assert login(passwordless=True) == "303"
        machine.succeed("podman stop eidetica-test; podman rm eidetica-test")
        machine.log("OCI container integration test passed: separate config/cache mounts, restart, offline cache recreation, credentials, refusal and disposable image cache")
      '';
    };
  };
}
