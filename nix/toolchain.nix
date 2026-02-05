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
in {
  inherit
    fenixStable
    rustSrc
    craneLib
    baseArgs
    cargoArtifactsRelease
    cargoArtifactsBench
    cargoArtifactsDebug
    cargoArtifactsAsan
    cargoArtifactsLsan
    releaseArgs
    benchArgs
    debugArgs
    asanArgs
    lsanArgs
    ;
}
