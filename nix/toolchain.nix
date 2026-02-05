# Rust toolchain and Crane build configuration
{
  inputs,
  system,
  pkgs,
}: let
  # Stable Rust toolchain for main builds (tests, linting, docs)
  fenixStable = inputs.fenix.packages.${system}.stable;
  toolChainStable = fenixStable.toolchain;
  craneLibStable = (inputs.crane.mkLib pkgs).overrideToolchain toolChainStable;

  # Nightly Rust toolchain for coverage (llvm-tools-preview) and sanitizers (miri, -Z flags)
  fenixNightly = inputs.fenix.packages.${system}.complete;
  toolChainNightly = fenixNightly.toolchain;
  craneLibNightly = (inputs.crane.mkLib pkgs).overrideToolchain toolChainNightly;

  # Default to stable for main builds
  craneLib = craneLibStable;
  rustSrc = fenixStable.rust-src;

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

  # Workspace-wide artifact caches per unique build configuration
  # One artifact per profile is sufficient - package-specific flags are added at build time

  # Release deps (workspace-wide, used by lib, bin, and min-versions)
  cargoArtifactsRelease = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "release-workspace";
      CARGO_PROFILE = "release";
      cargoExtraArgs = "--workspace --all-features";
    });

  # Bench deps - uses buildDepsOnly with bench stubs injected via dummy hook
  # The dummy source from crane doesn't include bench files, so we add them
  cargoArtifactsBench = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "bench-deps";
      cargoBuildCommand = "cargo bench --no-run --workspace --all-features";
      cargoCheckCommand = "true"; # skip check phase
      # Inject dummy bench files before build (crane's dummy source lacks bench targets)
      # Dynamically creates stubs for all [[bench]] targets found in Cargo.toml
      preBuild = ''
        echo "=== Creating bench stubs ==="
        mkdir -p crates/lib/benches

        # Create criterion stub template
        stub='use criterion::{criterion_group, criterion_main, Criterion};
        fn bench(_c: &mut Criterion) {}
        criterion_group!(benches, bench);
        criterion_main!(benches);'

        # Find all [[bench]] targets and create dummy files for each
        for name in $(grep -A1 '^\[\[bench\]\]' crates/lib/Cargo.toml | grep 'name = ' | sed 's/.*name = "\([^"]*\)".*/\1/'); do
          echo "Creating stub for: $name"
          echo "$stub" > "crates/lib/benches/$name.rs"
        done
        echo "=== Done creating bench stubs ==="
        ls -la crates/lib/benches/
      '';
    });

  # Debug deps for tests, lints, docs (workspace-wide)
  cargoArtifactsDebug = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "debug";
      CARGO_PROFILE = "dev";
      cargoExtraArgs = "--workspace --all-features";
    });

  # Nightly base args for sanitizer builds (need -Z flags)
  baseArgsNightly = {
    src = craneLibNightly.cleanCargoSource ../.;
    strictDeps = true;
    nativeBuildInputs = with pkgs; [
      pkg-config
    ];
    buildInputs = with pkgs; [
      openssl
    ];
  };

  # Debug deps for nightly builds (miri needs nightly debug artifacts)
  cargoArtifactsDebugNightly = craneLibNightly.buildDepsOnly (baseArgsNightly
    // {
      pname = "debug-nightly";
      CARGO_PROFILE = "dev";
      cargoExtraArgs = "--workspace --all-features";
    });

  # Build cargo dependencies with AddressSanitizer (Linux only, nightly)
  # Uses dev profile to match test builds
  cargoArtifactsAsan = craneLibNightly.buildDepsOnly (baseArgsNightly
    // {
      pname = "asan";
      CARGO_PROFILE = "dev";
      RUSTFLAGS = "-Zsanitizer=address";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    });

  # Build cargo dependencies with LeakSanitizer (Linux only, nightly)
  # Uses dev profile to match test builds
  cargoArtifactsLsan = craneLibNightly.buildDepsOnly (baseArgsNightly
    // {
      pname = "lsan";
      CARGO_PROFILE = "dev";
      RUSTFLAGS = "-Zsanitizer=leak";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    });

  # Release arguments for all release builds (lib, bin, min-versions)
  releaseArgs =
    baseArgs
    // {
      cargoArtifacts = cargoArtifactsRelease;
      CARGO_PROFILE = "release";
    };

  # Bench arguments (uses bench profile artifacts)
  benchArgs =
    baseArgs
    // {
      cargoArtifacts = cargoArtifactsBench;
    };

  # Common arguments for debug builds (analysis tools that don't need runtime performance)
  debugArgs =
    baseArgs
    // {
      cargoArtifacts = cargoArtifactsDebug;
      CARGO_PROFILE = "dev";
    };

  # Arguments for AddressSanitizer builds (Linux only, nightly)
  asanArgs =
    baseArgsNightly
    // {
      cargoArtifacts = cargoArtifactsAsan;
      RUSTFLAGS = "-Zsanitizer=address";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    };

  # Arguments for LeakSanitizer builds (Linux only, nightly)
  lsanArgs =
    baseArgsNightly
    // {
      cargoArtifacts = cargoArtifactsLsan;
      RUSTFLAGS = "-Zsanitizer=leak";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    };

  # Debug arguments for nightly builds (miri)
  debugArgsNightly =
    baseArgsNightly
    // {
      cargoArtifacts = cargoArtifactsDebugNightly;
      CARGO_PROFILE = "dev";
    };
in {
  inherit
    fenixStable
    fenixNightly
    rustSrc
    craneLib
    craneLibNightly
    baseArgs
    baseArgsNightly
    cargoArtifactsRelease
    cargoArtifactsBench
    cargoArtifactsDebug
    cargoArtifactsDebugNightly
    cargoArtifactsAsan
    cargoArtifactsLsan
    releaseArgs
    benchArgs
    debugArgs
    debugArgsNightly
    asanArgs
    lsanArgs
    ;
}
