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

  lint-fmt = craneLib.cargoFmt (baseArgs
    // {
      cargoExtraArgs = "--all";
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
  lint-statix =
    pkgs.runCommand "lint-statix" {
      nativeBuildInputs = [pkgs.statix];
      inherit (baseArgs) src;
    } ''
      cd $src
      statix check .
      touch $out
    '';

  # Find dead Nix code with deadnix
  lint-deadnix =
    pkgs.runCommand "lint-deadnix" {
      nativeBuildInputs = [pkgs.deadnix];
      inherit (baseArgs) src;
    } ''
      cd $src
      deadnix --fail .
      touch $out
    '';

  # Fast lint checks for CI (clippy + deny + fmt + nix linters, no udeps)
  lintFast = {
    clippy = lint-clippy;
    deny = lint-deny;
    fmt = lint-fmt;
    statix = lint-statix;
    deadnix = lint-deadnix;
  };

  # All lint packages
  lintPackages = {
    clippy = lint-clippy;
    deny = lint-deny;
    fmt = lint-fmt;
    udeps = lint-udeps;
    statix = lint-statix;
    deadnix = lint-deadnix;
  };
in {
  inherit lint-clippy lint-deny lint-fmt lint-udeps lint-statix lint-deadnix lintFast lintPackages;
}
