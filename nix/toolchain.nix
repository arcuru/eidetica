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
  # Use .complete as a component source but NOT .complete.toolchain — that combines ALL
  # components and breaks when any component isn't published for a given nightly date.
  # Instead, explicitly list only the components needed via .withComponents.
  fenixNightly = inputs.fenix.packages.${system}.complete;
  toolChainNightly = fenixNightly.withComponents [
    "cargo"
    "rustc"
    "rust-std"
    "rust-src"
    "clippy"
    "rustfmt"
  ];
  craneLibNightly = (inputs.crane.mkLib pkgs).overrideToolchain toolChainNightly;

  # Default to stable for main builds
  craneLib = craneLibStable;
  rustSrc = fenixStable.rust-src;

  inherit (pkgs) lib;

  # Mold linker for faster link times (Linux only)
  moldBuildInputs = lib.optionals pkgs.stdenv.isLinux [pkgs.mold];
  moldFlags = lib.optionalString pkgs.stdenv.isLinux " -C link-arg=-fuse-ld=mold";

  # Base arguments for cargo derivations
  # These are shared by all builds
  baseArgs =
    {
      # Clean source to include only Rust-relevant files
      src = craneLib.cleanCargoSource ../.;
      strictDeps = true;
      nativeBuildInputs =
        [pkgs.pkg-config]
        ++ moldBuildInputs;
      buildInputs = with pkgs; [
        openssl
      ];
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
    };

  # Workspace-wide artifact caches per unique build configuration
  # One artifact per profile is sufficient - package-specific flags are added at build time

  # Release deps (workspace-wide, used by lib, bin, and min-versions)
  cargoArtifactsRelease = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "release";
      CARGO_PROFILE = "release";
      cargoExtraArgs = "--workspace --all-features";
    });

  # Bench deps - uses buildDepsOnly with bench stubs injected via dummy hook
  # The dummy source from crane doesn't include bench files, so we add them
  cargoArtifactsBench = craneLib.buildDepsOnly (baseArgs
    // {
      pname = "bench";
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
  baseArgsNightly =
    {
      src = craneLibNightly.cleanCargoSource ../.;
      strictDeps = true;
      nativeBuildInputs =
        [pkgs.pkg-config]
        ++ moldBuildInputs;
      buildInputs = with pkgs; [
        openssl
      ];
    }
    // lib.optionalAttrs pkgs.stdenv.isLinux {
      RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
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
      RUSTFLAGS = "-Zsanitizer=address${moldFlags}";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    });

  # Build cargo dependencies with LeakSanitizer (Linux only, nightly)
  # Uses dev profile to match test builds
  cargoArtifactsLsan = craneLibNightly.buildDepsOnly (baseArgsNightly
    // {
      pname = "lsan";
      CARGO_PROFILE = "dev";
      RUSTFLAGS = "-Zsanitizer=leak${moldFlags}";
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
      RUSTFLAGS = "-Zsanitizer=address${moldFlags}";
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
    };

  # Arguments for LeakSanitizer builds (Linux only, nightly)
  lsanArgs =
    baseArgsNightly
    // {
      cargoArtifacts = cargoArtifactsLsan;
      RUSTFLAGS = "-Zsanitizer=leak${moldFlags}";
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
    toolChainNightly
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
