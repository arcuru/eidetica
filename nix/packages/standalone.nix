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

  # Interactive benchmark runner (for nix run .#bench)
  # Runs benchmarks from cached artifacts in the current directory
  bench-runner = pkgs.writeShellApplication {
    name = "bench-runner";
    runtimeInputs = [];
    text = ''
      # Determine workspace directory for benchmarks
      if [[ -f "./Cargo.toml" ]]; then
        echo "Running benchmarks in $(pwd)..."
        # Copy cached artifacts to local target directory for cargo to find
        mkdir -p target
        if [[ -d "${bench-artifacts}/target" ]]; then
          cp -rT "${bench-artifacts}/target" target/ 2>/dev/null || true
        fi
        exec cargo bench --workspace --all-features "$@"
      else
        echo "Error: Must be run from a project directory with Cargo.toml" >&2
        exit 1
      fi
    '';
  };

  # CI check derivation for benchmarks (hermetic)
  bench-check = craneLib.mkCargoDerivation (benchArgs
    // {
      pname = "bench-check";
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
  # Nested bench structure
  bench = {
    artifacts = bench-artifacts;
    runner = bench-runner;
    check = bench-check;
  };

  # Minimum version compatibility check (stays flat)
  inherit min-versions;
}
