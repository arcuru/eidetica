# Standalone packages (bench, min-versions)
{
  craneLib,
  releaseArgs,
  benchArgs,
  baseArgs,
  pkgs,
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

  # Minimum version compatibility check
  min-versions = craneLib.mkCargoDerivation (releaseArgs
    // {
      pname = "min-versions";
      nativeBuildInputs = baseArgs.nativeBuildInputs ++ [pkgs.cargo-nextest];
      buildPhaseCargoCommand = ''
        cargo update -Z minimal-versions
        cargo build --workspace --all-targets --all-features --quiet
      '';
      doCheck = true;
      checkPhase = ''
        runHook preCheck
        cargo nextest run --workspace --all-features --status-level fail --show-progress=none
        runHook postCheck
      '';
      doInstallCargoArtifacts = false;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        echo "Minimum version compatibility verified" > $out/result
        runHook postInstall
      '';
    });
in {
  # Bench: .#bench runs benchmarks, .#bench.artifacts for intermediate compilation artifacts
  bench =
    bench
    // {
      artifacts = bench-artifacts;
    };

  # Minimum version compatibility check (stays flat)
  inherit min-versions;
}
