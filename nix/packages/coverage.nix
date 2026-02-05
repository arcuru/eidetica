# Code coverage packages (requires nightly for llvm-tools-preview)
{
  craneLibNightly,
  baseArgsNightly,
  fenixNightly,
  pkgs,
  lib,
}: let
  # Common nativeBuildInputs for coverage (tarpaulin + llvm-tools)
  coverageNativeBuildInputs =
    baseArgsNightly.nativeBuildInputs
    ++ [
      pkgs.cargo-tarpaulin
      (fenixNightly.withComponents ["llvm-tools-preview"])
    ];

  # Build instrumented test binaries once (shared by all backends)
  coverageArtifacts = craneLibNightly.mkCargoDerivation (baseArgsNightly
    // {
      pname = "coverage-artifacts";
      cargoArtifacts = null;
      buildPhaseCargoCommand = "cargo tarpaulin --no-run --workspace --all-features --engine llvm";
      nativeBuildInputs = coverageNativeBuildInputs;
      doInstallCargoArtifacts = true;
    });

  # Shared args that reuse instrumented artifacts
  coverageArgs =
    baseArgsNightly
    // {
      cargoArtifacts = coverageArtifacts;
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      nativeBuildInputs = coverageNativeBuildInputs;
    };

  coverage-inmemory = craneLibNightly.cargoTarpaulin (coverageArgs
    // {
      pname = "coverage-inmemory";
      TEST_BACKEND = "inmemory";
    });

  coverage-sqlite = craneLibNightly.cargoTarpaulin (coverageArgs
    // {
      pname = "coverage-sqlite";
      TEST_BACKEND = "sqlite";
    });

  # PostgreSQL coverage (Linux only)
  coverage-postgres = craneLibNightly.cargoTarpaulin (coverageArgs
    // {
      pname = "coverage-postgres";
      TEST_BACKEND = "postgres";
      nativeBuildInputs = coverageNativeBuildInputs ++ [pkgs.postgresql];
      preBuild = ''
        export PGDATA="$TMPDIR/pgdata"
        export PGHOST="$TMPDIR"
        export PGDATABASE="eidetica_test"

        initdb --no-locale --encoding=UTF8 --auth=trust
        pg_ctl start -o "-k $TMPDIR -h '''"
        createdb $PGDATABASE

        export TEST_POSTGRES_URL="postgres:///$PGDATABASE?host=$TMPDIR"
      '';
      postBuild = ''
        pg_ctl stop || true
      '';
    });
in {
  # Shared instrumented artifacts
  artifacts = coverageArtifacts;

  packages =
    {
      inmemory = coverage-inmemory;
      sqlite = coverage-sqlite;
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      postgres = coverage-postgres;
    };

  # Fast coverage (sqlite only, for CI)
  packagesFast = {
    sqlite = coverage-sqlite;
  };
}
