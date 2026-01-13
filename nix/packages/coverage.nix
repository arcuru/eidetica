# Code coverage packages
{
  craneLib,
  baseArgs,
  fenixStable,
  pkgs,
  lib,
  rootSrc,
}: let
  coverage-inmemory = craneLib.cargoTarpaulin (baseArgs
    // {
      cargoArtifacts = craneLib.mkDummySrc {src = rootSrc;};
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      nativeBuildInputs =
        baseArgs.nativeBuildInputs
        ++ [
          (fenixStable.withComponents [
            "llvm-tools-preview"
          ])
        ];
    });

  coverage-sqlite = craneLib.cargoTarpaulin (baseArgs
    // {
      pname = "coverage-sqlite";
      cargoArtifacts = craneLib.mkDummySrc {src = rootSrc;};
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      TEST_BACKEND = "sqlite";
      nativeBuildInputs =
        baseArgs.nativeBuildInputs
        ++ [
          (fenixStable.withComponents [
            "llvm-tools-preview"
          ])
        ];
    });

  # PostgreSQL coverage (Linux only)
  coverage-postgres = craneLib.cargoTarpaulin (baseArgs
    // {
      pname = "coverage-postgres";
      TEST_BACKEND = "postgres";
      cargoArtifacts = craneLib.mkDummySrc {src = rootSrc;};
      cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
      nativeBuildInputs =
        baseArgs.nativeBuildInputs
        ++ [
          pkgs.postgresql
          (fenixStable.withComponents [
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

  # Fast coverage (inmemory only)
  coverageFast = {
    inmemory = coverage-inmemory;
  };

  # All coverage packages
  coveragePackages =
    {
      inmemory = coverage-inmemory;
      sqlite = coverage-sqlite;
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      postgres = coverage-postgres;
    };
in {
  inherit coverage-inmemory coverage-sqlite coverage-postgres coverageFast coveragePackages;
}
