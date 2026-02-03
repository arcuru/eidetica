# Core eidetica packages (library and binary)
{
  craneLib,
  releaseArgsLib,
  releaseArgsBin,
}: let
  # Library crate build
  # Uses releaseArgsLib with matching -p eidetica --all-features
  eidetica-lib = craneLib.buildPackage (releaseArgsLib
    // {
      pname = "eidetica";
      cargoExtraArgs = "-p eidetica --all-features";
      doCheck = false; # Tests run separately with nextest
      meta = {
        description = "Eidetica library - A P2P decentralized database";
      };
    });

  # Binary crate build
  # Uses releaseArgsBin with matching -p eidetica-bin --all-features
  eidetica-bin = craneLib.buildPackage (releaseArgsBin
    // {
      pname = "eidetica-bin";
      cargoExtraArgs = "-p eidetica-bin --all-features";
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
