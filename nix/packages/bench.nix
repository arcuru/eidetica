# Benchmark packages
{
  craneLib,
  benchArgs,
  eidLib,
}: let
  # Build bench artifacts (cached in Nix store)
  # Uses cargo bench --no-run to compile without executing
  bench-artifacts = craneLib.mkCargoDerivation (benchArgs
    // {
      pname = "bench-artifacts";
      buildPhaseCargoCommand = "cargo bench --no-run --workspace --all-features";
      doInstallCargoArtifacts = true;
    });

  # Benchmark derivation (hermetic)
  bench = craneLib.mkCargoDerivation (benchArgs
    // {
      pname = "bench";
      cargoArtifacts = bench-artifacts;
      buildPhaseCargoCommand = "cargo bench --workspace --all-features";
      doCheck = false;
      meta = {
        description = "Eidetica benchmark execution";
      };
    });

  # Interactive benchmark runner
  bench-runner = eidLib.mkCargoRunner {
    name = "bench-runner";
    command = "cargo bench --workspace --all-features";
  };
in {
  builds = {
    default = bench;
  };

  runners = {
    default = bench-runner;
  };

  artifacts = bench-artifacts;
}
