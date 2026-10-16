# Core eidetica package (binary)
{
  craneLib,
  releaseArgs,
  releaseCargoExtraArgs,
  binDebugArgs,
}: let
  eidetica-bin = craneLib.buildPackage (releaseArgs
    // {
      pname = "eidetica-bin";
      cargoExtraArgs = releaseCargoExtraArgs;
      doCheck = false; # Tests run separately with nextest
      meta = {
        description = "Eidetica binary";
        mainProgram = "eidetica";
      };
    });

  # Debug build for CI checks, with feature-matched dependencies
  eidetica-bin-debug = craneLib.buildPackage (binDebugArgs
    // {
      pname = "eidetica-bin-debug";
      doCheck = false;
    });
in {
  inherit eidetica-bin eidetica-bin-debug;
}
