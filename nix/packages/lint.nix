# Linting and code quality packages
{
  craneLib,
  baseArgs,
  debugArgs,
  pkgs,
}: let
  # Tools that compile code - benefit from cached deps
  lint-clippy = craneLib.cargoClippy (debugArgs
    // {
      cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
    });

  # Tools that don't compile - use baseArgs (no cached deps needed)
  lint-deny = craneLib.cargoDeny (baseArgs
    // {
      cargoDenyExtraArgs = "--workspace --all-features";
      cargoDenyChecks = "bans licenses sources";
    });

  # cargo-udeps: find unused dependencies
  # Note: outputs report to $out/result, does not fail on findings
  # Uses debugArgs - udeps needs to compile code to detect unused deps
  lint-udeps = craneLib.mkCargoDerivation (debugArgs
    // {
      pname = "udeps";
      buildPhaseCargoCommand = "cargo udeps --workspace --all-targets --all-features 2>&1 | tee udeps-report.txt || true";
      nativeBuildInputs = debugArgs.nativeBuildInputs ++ [pkgs.cargo-udeps];
      doInstallCargoArtifacts = false;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        cp udeps-report.txt $out/result
        runHook postInstall
      '';
    });

  # Nix linting with statix
  # Note: These lint checks output directories (mkdir $out) for symlinkJoin compatibility
  lint-statix =
    pkgs.runCommand "lint-statix" {
      nativeBuildInputs = [pkgs.statix];
      inherit (baseArgs) src;
    } ''
      cd $src
      statix check .
      mkdir -p $out
    '';

  # Find dead Nix code with deadnix
  lint-deadnix =
    pkgs.runCommand "lint-deadnix" {
      nativeBuildInputs = [pkgs.deadnix];
      inherit (baseArgs) src;
    } ''
      cd $src
      deadnix --fail .
      mkdir -p $out
    '';

  # Shell script linting with shellcheck
  lint-shellcheck =
    pkgs.runCommand "lint-shellcheck" {
      nativeBuildInputs = [pkgs.shellcheck pkgs.findutils];
      inherit (baseArgs) src;
    } ''
      cd $src
      find . -name "*.sh" -type f -exec shellcheck {} +
      mkdir -p $out
    '';

  # YAML linting with yamllint
  lint-yamllint =
    pkgs.runCommand "lint-yamllint" {
      nativeBuildInputs = [pkgs.yamllint pkgs.findutils];
      inherit (baseArgs) src;
    } ''
      cd $src
      find . \( -name "*.yml" -o -name "*.yaml" \) -type f -exec yamllint -c .config/yamllint.yaml {} +
      mkdir -p $out
    '';
in {
  packages = {
    clippy = lint-clippy;
    deny = lint-deny;
    udeps = lint-udeps;
    statix = lint-statix;
    deadnix = lint-deadnix;
    shellcheck = lint-shellcheck;
    yamllint = lint-yamllint;
  };

  # Fast lint checks for CI (excludes udeps)
  packagesFast = {
    clippy = lint-clippy;
    deny = lint-deny;
    statix = lint-statix;
    deadnix = lint-deadnix;
    shellcheck = lint-shellcheck;
    yamllint = lint-yamllint;
  };
}
