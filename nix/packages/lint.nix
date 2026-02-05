# Linting and code quality packages
#
# This module provides:
# - Source filtering helpers for non-Rust linters
# - A declarative linter builder (mkSimpleLinter)
# - All lint packages organized by type
{
  craneLib,
  craneLibNightly,
  baseArgs,
  baseArgsNightly,
  debugArgs,
  pkgs,
  lib,
}: let
  # =============================================================================
  # Library: Source filtering helpers
  # =============================================================================
  # Base source with gitignore applied
  cleanSrc = lib.cleanSource ../..;

  # Create a source filtered to specific file extensions
  # Usage: sourceWithExts ["nix"] or sourceWithExts ["yml" "yaml"]
  sourceWithExts = exts:
    lib.cleanSourceWith {
      src = cleanSrc;
      filter = path: type:
        (type == "directory")
        || (lib.any (ext: lib.hasSuffix ".${ext}" path) exts);
    };

  # Pre-defined filtered sources for common file types
  sources = {
    nix = sourceWithExts ["nix"];
    shell = sourceWithExts ["sh"];
    yaml = sourceWithExts ["yml" "yaml"];
    all = cleanSrc;
  };

  # =============================================================================
  # Library: Linter builders
  # =============================================================================

  # Build a simple linter that runs a command on filtered source
  # Usage:
  #   mkSimpleLinter {
  #     name = "deadnix";
  #     packages = [ pkgs.deadnix ];
  #     src = sources.nix;
  #     command = "deadnix --fail .";
  #   }
  mkSimpleLinter = {
    name,
    packages ? [],
    src ? cleanSrc,
    command,
  }:
    pkgs.runCommand "lint-${name}" {
      nativeBuildInputs = packages;
      inherit src;
    } ''
      cd $src
      ${command}
      mkdir -p $out
    '';

  # =============================================================================
  # Linter definitions: Simple (non-Rust) linters
  # =============================================================================

  simpleLinters = {
    statix = mkSimpleLinter {
      name = "statix";
      packages = [pkgs.statix];
      src = sources.nix;
      command = "statix check .";
    };

    deadnix = mkSimpleLinter {
      name = "deadnix";
      packages = [pkgs.deadnix];
      src = sources.nix;
      command = "deadnix --fail .";
    };

    shellcheck = mkSimpleLinter {
      name = "shellcheck";
      packages = [pkgs.shellcheck pkgs.findutils];
      src = sources.shell;
      command = ''find . -name "*.sh" -type f -exec shellcheck {} +'';
    };

    yamllint = mkSimpleLinter {
      name = "yamllint";
      packages = [pkgs.yamllint pkgs.findutils];
      src = sources.yaml;
      command = ''find . \( -name "*.yml" -o -name "*.yaml" \) -type f -exec yamllint -c .config/yamllint.yaml {} +'';
    };

    typos = mkSimpleLinter {
      name = "typos";
      packages = [pkgs.typos];
      src = sources.all;
      command = "typos --config .config/typos.toml";
    };
  };

  # =============================================================================
  # Linter definitions: Rust linters (use crane)
  # =============================================================================

  rustLinters = {
    clippy = craneLib.cargoClippy (debugArgs
      // {
        cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
      });

    deny = craneLib.cargoDeny (baseArgs
      // {
        cargoDenyExtraArgs = "--workspace --all-features";
        cargoDenyChecks = "bans licenses sources";
      });

    # cargo-udeps: find unused dependencies
    # Requires nightly toolchain for -Z flags
    # Note: No cached artifacts - must perform its own instrumented build
    udeps = craneLibNightly.mkCargoDerivation (baseArgsNightly
      // {
        pname = "udeps";
        cargoArtifacts = null;
        buildPhaseCargoCommand = "cargo udeps --workspace --all-targets --all-features";
        nativeBuildInputs = baseArgsNightly.nativeBuildInputs ++ [pkgs.cargo-udeps];
        doInstallCargoArtifacts = false;
        installPhase = ''
          runHook preInstall
          mkdir -p $out
          echo "No unused dependencies found" > $out/result
          runHook postInstall
        '';
      });

    # Minimum version compatibility check
    # Validates that minimum versions in Cargo.toml actually work
    minversions = craneLibNightly.mkCargoDerivation (baseArgsNightly
      // {
        pname = "minversions";
        cargoArtifacts = null;
        nativeBuildInputs = baseArgsNightly.nativeBuildInputs ++ [pkgs.cargo-nextest];
        buildPhaseCargoCommand = ''
          cargo update -Z minimal-versions
          cargo build --workspace --all-targets --all-features --quiet
        '';
        doCheck = true;
        checkPhase = ''
          runHook preCheck
          cargo nextest run --workspace --all-features --status-level fail --show-progress=none
          runHook postCheck
        '';
        doInstallCargoArtifacts = false;
        installPhase = ''
          runHook preInstall
          mkdir -p $out
          echo "Minimum version compatibility verified" > $out/result
          runHook postInstall
        '';
      });
  };

  # =============================================================================
  # Package groups
  # =============================================================================

  allLinters = simpleLinters // rustLinters;

  # Fast linters for CI (excludes slow: udeps, minversions)
  fastLinters = builtins.removeAttrs allLinters ["udeps" "minversions"];
in {
  # Exported packages
  packages = allLinters;
  packagesFast = fastLinters;

  # Export library for potential reuse
  lib = {
    inherit sourceWithExts mkSimpleLinter sources cleanSrc;
  };
}
