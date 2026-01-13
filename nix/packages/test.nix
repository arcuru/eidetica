# Test packages for different backends
{
  craneLib,
  debugArgs,
  baseArgs,
  pkgs,
  lib,
}: let
  # Common nextest args for cleaner log output (no progress bar)
  nextestBaseArgs = "--show-progress=none --no-fail-fast";

  # Build test binaries once, reuse across all backend tests
  # Uses dev profile for faster compilation (deps already optimized via Cargo.toml)
  testArtifacts = craneLib.mkCargoDerivation (debugArgs
    // {
      pname = "test-artifacts";
      buildPhaseCargoCommand = "cargo nextest run --no-run --workspace --all-features";
      nativeBuildInputs = baseArgs.nativeBuildInputs ++ [pkgs.cargo-nextest];
      doInstallCargoArtifacts = true;
    });

  # Args for test runners that reuse pre-built test binaries
  testRunnerArgs =
    debugArgs
    // {
      cargoArtifacts = testArtifacts;
    };

  test-inmemory = craneLib.cargoNextest (testRunnerArgs
    // {
      cargoNextestExtraArgs = "--workspace --all-features ${nextestBaseArgs}";
    });

  test-sqlite = craneLib.cargoNextest (testRunnerArgs
    // {
      pname = "test-sqlite";
      TEST_BACKEND = "sqlite";
      cargoNextestExtraArgs = "--workspace --all-features ${nextestBaseArgs}";
    });

  test-minimal = craneLib.cargoNextest (debugArgs
    // {
      pname = "eidetica-minimal";
      cargoNextestExtraArgs = "-p eidetica --no-default-features --features testing ${nextestBaseArgs}";
    });

  # PostgreSQL backend tests (Linux only)
  test-postgres = craneLib.cargoNextest (testRunnerArgs
    // {
      pname = "test-postgres";
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
      cargoNextestExtraArgs = "--workspace --all-features ${nextestBaseArgs}";
    });

  # Fast tests for CI (inmemory + minimal)
  testFast = {
    inmemory = test-inmemory;
    sqlite = test-sqlite;
    minimal = test-minimal;
  };

  # All test packages
  testPackages =
    {
      inmemory = test-inmemory;
      sqlite = test-sqlite;
      minimal = test-minimal;
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      postgres = test-postgres;
    };
in {
  inherit test-inmemory test-sqlite test-minimal test-postgres testFast testPackages;
}
