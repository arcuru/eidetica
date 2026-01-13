# Rust toolchain and Crane build configuration
{
  inputs,
  system,
  pkgs,
}: let
  # Rust toolchain configuration using fenix
  # Using fenix instead of rust-overlay for better llvm-tools support (needed for coverage)
  fenixStable = inputs.fenix.packages.${system}.complete;
  rustSrc = fenixStable.rust-src;
  toolChain = fenixStable.toolchain;

  # Crane library with custom Rust toolchain
  # Crane provides efficient Rust builds with Nix
  craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolChain;

  # Base arguments for cargo derivations
  # These are shared by all builds
  baseArgs = {
    # Clean source to include only Rust-relevant files
    src = craneLib.cleanCargoSource ../.;
    strictDeps = true;
    nativeBuildInputs = with pkgs; [
      pkg-config # Required for OpenSSL linking
    ];
    buildInputs = with pkgs; [
      openssl
    ];
  };

  # Build cargo dependencies for release builds
  # This creates a build cache that can be reused by all release builds
  cargoArtifacts = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "release";
      CARGO_PROFILE = "release";
    });

  # Build cargo dependencies for debug profile (used by book-test)
  # Separate debug cache needed because debug/release profiles are incompatible
  cargoArtifactsDebug = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "debug";
      CARGO_PROFILE = "dev";
    });

  # Build cargo dependencies with AddressSanitizer (Linux only)
  # Uses dev profile to match test builds
  cargoArtifactsAsan = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "asan";
      CARGO_PROFILE = "dev";
      RUSTFLAGS = "-Zsanitizer=address";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    });

  # Build cargo dependencies with LeakSanitizer (Linux only)
  # Uses dev profile to match test builds
  cargoArtifactsLsan = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "lsan";
      CARGO_PROFILE = "dev";
      RUSTFLAGS = "-Zsanitizer=leak";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    });

  # Common arguments for release builds (tests, benchmarks, main packages)
  releaseArgs =
    baseArgs
    // {
      inherit cargoArtifacts;
      CARGO_PROFILE = "release";
    };

  # Common arguments for debug builds (analysis tools that don't need runtime performance)
  debugArgs =
    baseArgs
    // {
      cargoArtifacts = cargoArtifactsDebug;
      CARGO_PROFILE = "dev";
    };

  # Arguments for AddressSanitizer builds (Linux only)
  asanArgs =
    baseArgs
    // {
      cargoArtifacts = cargoArtifactsAsan;
      RUSTFLAGS = "-Zsanitizer=address";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    };

  # Arguments for LeakSanitizer builds (Linux only)
  lsanArgs =
    baseArgs
    // {
      cargoArtifacts = cargoArtifactsLsan;
      RUSTFLAGS = "-Zsanitizer=leak";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    };

  # Arguments for tools that rebuild everything (sanitizers, coverage, etc.)
  # Uses dummy artifacts so mkCargoDerivation doesn't require pre-built deps
  noDepsArgs =
    baseArgs
    // {
      cargoArtifacts = craneLib.mkDummySrc {src = baseArgs.src;};
    };
in {
  inherit
    fenixStable
    rustSrc
    craneLib
    baseArgs
    cargoArtifacts
    cargoArtifactsDebug
    cargoArtifactsAsan
    cargoArtifactsLsan
    releaseArgs
    debugArgs
    asanArgs
    lsanArgs
    noDepsArgs
    ;
}
