# Test packages for different backends
{
  craneLib,
  debugArgs,
  baseArgs,
  pkgs,
  lib,
}: let
  # Args for CI checks (no progress bar for cleaner logs)
  nextestCheckArgs = "--no-fail-fast --show-progress=none";
  # Args for interactive runners (progress bar, only show failures)
  nextestRunnerArgs = "--no-fail-fast --show-progress=bar --status-level fail";

  # Build test artifacts (cached in Nix store)
  # Creates a nextest archive containing test binaries + metadata
  # This archive can be used to run tests without recompilation
  test-artifacts = craneLib.mkCargoDerivation (debugArgs
    // {
      pname = "test-artifacts";
      nativeBuildInputs = baseArgs.nativeBuildInputs ++ [pkgs.cargo-nextest];
      buildPhaseCargoCommand = ''
        cargo nextest archive --archive-file archive.tar.zst --workspace --all-features
      '';
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        cp archive.tar.zst $out/
        runHook postInstall
      '';
    });

  # Helper to create backend-specific test runners
  # These are lightweight shell scripts that run tests from the cached archive
  mkTestRunner = {
    name,
    backend ? null,
    extraEnv ? "",
    extraDeps ? [],
    preRun ? "",
    postRun ? "",
  }:
    pkgs.writeShellApplication {
      inherit name;
      runtimeInputs = [pkgs.cargo-nextest] ++ extraDeps;
      text = ''
        ${lib.optionalString (backend != null) "export TEST_BACKEND=${backend}"}
        ${extraEnv}
        ${preRun}
        # Determine workspace directory for tests
        # Use current directory if it has Cargo.toml (project checkout)
        # Otherwise fall back to nix source (for pure nix builds)
        if [[ -f "./Cargo.toml" ]]; then
          NEXTEST_WORKSPACE="$(pwd)"
        else
          echo "Warning: Not in a project directory. Tests requiring source files may fail." >&2
          NEXTEST_WORKSPACE="${baseArgs.src}"
        fi
        cargo nextest run \
          --archive-file ${test-artifacts}/archive.tar.zst \
          --workspace-remap "$NEXTEST_WORKSPACE" \
          ${nextestRunnerArgs} \
          "$@"
        ${postRun}
      '';
    };

  # Interactive test runners (for nix run .#test-runner-*)
  test-runner-inmemory = mkTestRunner {name = "test-runner-inmemory";};

  test-runner-sqlite = mkTestRunner {
    name = "test-runner-sqlite";
    backend = "sqlite";
  };

  test-runner-postgres = mkTestRunner {
    name = "test-runner-postgres";
    backend = "postgres";
    extraDeps = [pkgs.postgresql];
    preRun = ''
      TMPDIR="''${TMPDIR:-/tmp}"
      export PGDATA="$TMPDIR/pgdata-$$"
      export PGHOST="$TMPDIR"
      export PGDATABASE="eidetica_test"

      # Suppress postgres output
      initdb --no-locale --encoding=UTF8 --auth=trust > /dev/null 2>&1
      pg_ctl start -o "-k $TMPDIR -h '''" -l "$PGDATA/postgres.log" > /dev/null 2>&1
      createdb "$PGDATABASE" > /dev/null 2>&1

      export TEST_POSTGRES_URL="postgres:///$PGDATABASE?host=$TMPDIR"
    '';
    postRun = ''
      pg_ctl stop > /dev/null 2>&1 || true
    '';
  };

  # Runner that executes all backends sequentially
  test-runner-all = pkgs.writeShellApplication {
    name = "test-runner-all";
    runtimeInputs = [pkgs.cargo-nextest pkgs.postgresql];
    text = ''
      echo "=== Running inmemory backend tests ==="
      ${test-runner-inmemory}/bin/test-runner-inmemory "$@"

      echo ""
      echo "=== Running sqlite backend tests ==="
      ${test-runner-sqlite}/bin/test-runner-sqlite "$@"

      ${lib.optionalString pkgs.stdenv.isLinux ''
        echo ""
        echo "=== Running postgres backend tests ==="
        ${test-runner-postgres}/bin/test-runner-postgres "$@"
      ''}

      echo ""
      echo "=== All backend tests completed ==="
    '';
  };

  # Build test binaries for CI checks (reuses debug artifacts)
  testCheckArtifacts = craneLib.mkCargoDerivation (debugArgs
    // {
      pname = "test-check-artifacts";
      buildPhaseCargoCommand = "cargo nextest run --no-run --workspace --all-features";
      nativeBuildInputs = baseArgs.nativeBuildInputs ++ [pkgs.cargo-nextest];
      doInstallCargoArtifacts = true;
    });

  # Args for CI check runners that reuse pre-built test binaries
  testCheckArgs =
    debugArgs
    // {
      cargoArtifacts = testCheckArtifacts;
    };

  # CI check derivations (hermetic, for nix flake check)
  test-check-inmemory = craneLib.cargoNextest (testCheckArgs
    // {
      pname = "test-check-inmemory";
      cargoNextestExtraArgs = "--workspace --all-features ${nextestCheckArgs}";
    });

  test-check-sqlite = craneLib.cargoNextest (testCheckArgs
    // {
      pname = "test-check-sqlite";
      TEST_BACKEND = "sqlite";
      cargoNextestExtraArgs = "--workspace --all-features ${nextestCheckArgs}";
    });

  test-check-minimal = craneLib.cargoNextest (debugArgs
    // {
      pname = "test-check-minimal";
      cargoNextestExtraArgs = "-p eidetica --no-default-features --features testing ${nextestCheckArgs}";
    });

  # PostgreSQL backend check (Linux only)
  test-check-postgres = craneLib.cargoNextest (testCheckArgs
    // {
      pname = "test-check-postgres";
      TEST_BACKEND = "postgres";
      nativeBuildInputs =
        baseArgs.nativeBuildInputs
        ++ [
          pkgs.postgresql
        ];
      preCheck = ''
        export PGDATA="$TMPDIR/pgdata"
        export PGHOST="$TMPDIR"
        export PGDATABASE="eidetica_test"

        initdb --no-locale --encoding=UTF8 --auth=trust
        pg_ctl start -o "-k $TMPDIR -h '''"
        createdb $PGDATABASE

        export TEST_POSTGRES_URL="postgres:///$PGDATABASE?host=$TMPDIR"
      '';
      postCheck = ''
        pg_ctl stop || true
      '';
      cargoNextestExtraArgs = "--workspace --all-features ${nextestCheckArgs}";
    });
in {
  # Build artifacts for caching
  artifacts = test-artifacts;

  # CI check derivations (for nix build .#test.<backend>)
  checks =
    {
      inmemory = test-check-inmemory;
      sqlite = test-check-sqlite;
      minimal = test-check-minimal;
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      postgres = test-check-postgres;
    };

  # Interactive runners (for nix run .#test-<backend>)
  runners =
    {
      inmemory = test-runner-inmemory;
      sqlite = test-runner-sqlite;
      all = test-runner-all;
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      postgres = test-runner-postgres;
    };
}
