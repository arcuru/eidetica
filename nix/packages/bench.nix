# Benchmark packages
{
  craneLib,
  benchArgs,
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
  # Note: Unlike tests, there's no interactive runner because cargo bench
  # doesn't support nextest-style archive/remap. Use `cargo bench` locally.
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
in {
  # Bench: .#bench runs benchmarks, .#bench.artifacts for intermediate compilation artifacts
  bench =
    bench
    // {
      artifacts = bench-artifacts;
    };
}
