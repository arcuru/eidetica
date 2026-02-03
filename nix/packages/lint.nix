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

  # Fast lint checks for CI (clippy + deny + fmt, no udeps)
  lintFast = {
    clippy = lint-clippy;
    deny = lint-deny;
    fmt = lint-fmt;
  };

  # All lint packages
  lintPackages = {
    clippy = lint-clippy;
    deny = lint-deny;
    fmt = lint-fmt;
    udeps = lint-udeps;
  };
in {
  inherit lint-clippy lint-deny lint-fmt lint-udeps lintFast lintPackages;
}
