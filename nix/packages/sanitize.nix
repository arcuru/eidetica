# Sanitizer packages (miri, asan, lsan)
# All sanitizers require nightly toolchain for -Z flags and miri component
{
  craneLibNightly,
  debugArgsNightly,
  asanArgs,
  lsanArgs,
  fenixNightly,
  pkgs,
  lib,
}: let
  # Miri: undefined behavior detection via MIR interpretation
  sanitize-miri = craneLibNightly.mkCargoDerivation (debugArgsNightly
    // {
      pname = "sanitize-miri";
      buildPhaseCargoCommand = "cargo miri test --workspace --all-features";
      nativeBuildInputs =
        debugArgsNightly.nativeBuildInputs
        ++ [
          (fenixNightly.withComponents [
            "miri"
            "rust-src"
          ])
        ];
      doInstallCargoArtifacts = false;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        echo "Miri tests passed" > $out/result
        runHook postInstall
      '';
    });

  # AddressSanitizer: memory errors, use-after-free, buffer overflows (Linux only)
  # Uses asanArgs which includes pre-built deps with sanitizer flags
  sanitize-asan = craneLibNightly.mkCargoDerivation (asanArgs
    // {
      pname = "sanitize-asan";
      buildPhaseCargoCommand = "cargo test --workspace --all-features --lib --bins --tests --examples --target x86_64-unknown-linux-gnu";
      doInstallCargoArtifacts = false;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        echo "AddressSanitizer tests passed" > $out/result
        runHook postInstall
      '';
    });

  # LeakSanitizer: memory leak detection (Linux only)
  # Uses lsanArgs which includes pre-built deps with sanitizer flags
  sanitize-lsan = craneLibNightly.mkCargoDerivation (lsanArgs
    // {
      pname = "sanitize-lsan";
      buildPhaseCargoCommand = "cargo test --workspace --all-features --lib --bins --tests --examples --target x86_64-unknown-linux-gnu";
      doInstallCargoArtifacts = false;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        echo "LeakSanitizer tests passed" > $out/result
        runHook postInstall
      '';
    });
in {
  builds = lib.optionalAttrs (pkgs.stdenv.isLinux && pkgs.stdenv.isx86_64) {
    miri = sanitize-miri;
    asan = sanitize-asan;
    lsan = sanitize-lsan;
  };

  # Fast sanitizers only (excludes miri due to 12+ hour runtime)
  defaults = lib.optionalAttrs (pkgs.stdenv.isLinux && pkgs.stdenv.isx86_64) {
    asan = sanitize-asan;
    lsan = sanitize-lsan;
  };
}
