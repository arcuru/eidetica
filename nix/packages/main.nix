# Core eidetica package (binary)
{
  craneLib,
  releaseArgs,
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
in {
  inherit eidetica-bin;
}
