# Core eidetica packages (library and binary)
{
  craneLib,
  releaseArgs,
}: let
  # Library crate build
  eidetica-lib = craneLib.buildPackage (releaseArgs
    // {
      pname = "eidetica";
      cargoExtraArgs = "-p eidetica --all-features";
      doCheck = false; # Tests run separately with nextest
      meta = {
        description = "Eidetica library - A P2P decentralized database";
      };
    });

  # Binary crate build
  eidetica-bin = craneLib.buildPackage (releaseArgs
    // {
      pname = "eidetica-bin";
      cargoExtraArgs = "-p eidetica-bin";
      doCheck = false; # Tests run separately with nextest
      meta = {
        description = "Eidetica binary";
        mainProgram = "eidetica";
      };
    });

  # Main package alias
  eidetica = eidetica-bin;
in {
  inherit eidetica eidetica-lib eidetica-bin;
}
