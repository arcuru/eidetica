# Standalone packages (bench, min-versions)
{
  craneLib,
  releaseArgs,
  baseArgs,
  pkgs,
}: let
  # Benchmark execution
  bench = craneLib.mkCargoDerivation (releaseArgs
    // {
      pname = "eidetica-bench";
      buildPhaseCargoCommand = "cargo bench --workspace --all-features";
      doCheck = false;
      meta = {
        description = "Eidetica benchmark suite";
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
  inherit bench min-versions;
}
