# Core eidetica package (binary)
{
  craneLib,
  releaseArgs,
  debugArgs,
}: let
  eidetica-bin = craneLib.buildPackage (releaseArgs
    // {
      pname = "eidetica-bin";
      cargoExtraArgs = "-p eidetica-bin --all-features";
      doCheck = false; # Tests run separately with nextest
      meta = {
        description = "Eidetica binary";
        mainProgram = "eidetica";
      };
    });

  # Debug build for CI checks (fast: reuses cargoArtifactsDebug)
  eidetica-bin-debug = craneLib.buildPackage (debugArgs
    // {
      pname = "eidetica-bin-debug";
      cargoExtraArgs = "-p eidetica-bin --all-features";
      doCheck = false;
    });
in {
  inherit eidetica-bin eidetica-bin-debug;
}
