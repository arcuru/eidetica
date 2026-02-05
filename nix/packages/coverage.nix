# Code coverage packages (requires nightly for llvm-tools-preview)
{
  craneLibNightly,
  baseArgsNightly,
  fenixNightly,
  pkgs,
  lib,
}: let
  # Dummy artifacts for coverage builds (which rebuild everything anyway)
  dummyArtifacts = craneLibNightly.mkDummySrc {inherit (baseArgsNightly) src;};

  coverage-inmemory = craneLibNightly.cargoTarpaulin (baseArgsNightly
    // {
      cargoArtifacts = dummyArtifacts;
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      nativeBuildInputs =
        baseArgsNightly.nativeBuildInputs
        ++ [
          (fenixNightly.withComponents [
            "llvm-tools-preview"
          ])
        ];
    });

  coverage-sqlite = craneLibNightly.cargoTarpaulin (baseArgsNightly
    // {
      pname = "coverage-sqlite";
      cargoArtifacts = dummyArtifacts;
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      TEST_BACKEND = "sqlite";
      nativeBuildInputs =
        baseArgsNightly.nativeBuildInputs
        ++ [
          (fenixNightly.withComponents [
            "llvm-tools-preview"
          ])
        ];
    });

  # PostgreSQL coverage (Linux only)
  coverage-postgres = craneLibNightly.cargoTarpaulin (baseArgsNightly
    // {
      pname = "coverage-postgres";
      TEST_BACKEND = "postgres";
      cargoArtifacts = dummyArtifacts;
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      nativeBuildInputs =
        baseArgsNightly.nativeBuildInputs
        ++ [
          pkgs.postgresql
          (fenixNightly.withComponents [
            "llvm-tools-preview"
          ])
        ];
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
