# Sanitizer packages (miri, asan, lsan)
# - miri: uses debugArgs (benefits from cached deps for setup)
# - asan/lsan: use dedicated deps caches with sanitizer flags
{
  craneLib,
  debugArgs,
  asanArgs,
  lsanArgs,
  fenixStable,
  pkgs,
  lib,
}: let
  # Miri: undefined behavior detection via MIR interpretation
  sanitize-miri = craneLib.mkCargoDerivation (debugArgs
    // {
      pname = "sanitize-miri";
      buildPhaseCargoCommand = "cargo miri test --workspace --all-features";
      nativeBuildInputs =
        debugArgs.nativeBuildInputs
        ++ [
          (fenixStable.withComponents [
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
  sanitize-asan = craneLib.mkCargoDerivation (asanArgs
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
  sanitize-lsan = craneLib.mkCargoDerivation (lsanArgs
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

  # All sanitizer packages (miri excluded from 'all' due to 12+ hour runtime)
  # Note: all sanitizers only available on x86_64-linux
  sanitizePackages = lib.optionalAttrs (pkgs.stdenv.isLinux && pkgs.stdenv.isx86_64) {
    miri = sanitize-miri;
    asan = sanitize-asan;
    lsan = sanitize-lsan;
  };

  # Fast sanitizers only (for .all aggregate)
  sanitizeFast = lib.optionalAttrs (pkgs.stdenv.isLinux && pkgs.stdenv.isx86_64) {
    asan = sanitize-asan;
    lsan = sanitize-lsan;
  };
in {
  inherit sanitize-miri sanitize-asan sanitize-lsan sanitizePackages sanitizeFast;
}
