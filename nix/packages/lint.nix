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
  eidLib,
  treefmtWrapper,
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
    markdown = lib.cleanSourceWith {
      src = cleanSrc;
      filter = path: type:
        (type == "directory")
        || (lib.hasSuffix ".md" path)
        || (builtins.baseNameOf path == "markdownlint.yaml");
    };
    github-actions = lib.cleanSourceWith {
      src = cleanSrc;
      filter = path: type:
        (type == "directory")
        || (lib.hasSuffix ".yml" path && lib.hasInfix ".github/workflows" path);
    };
    dockerfile = lib.cleanSourceWith {
      src = cleanSrc;
      filter = path: type:
        (type == "directory")
        || (builtins.baseNameOf path == "Dockerfile");
    };
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
    fixCommand ? null,
  }: let
    check =
      pkgs.runCommand "lint-${name}" {
        nativeBuildInputs = packages;
        inherit src;
      } ''
        cd $src
        ${command}
        mkdir -p $out
      '';
    runner = pkgs.writeShellApplication {
      name = "fix-${name}";
      runtimeInputs = packages;
      text = fixCommand;
    };
  in
    if fixCommand == null
    then check
    else
      check.overrideAttrs (old: {
        passthru =
          (old.passthru or {})
          // {
            fixRunner = runner;
          };
      });

  # =============================================================================
  # Linter definitions: Simple (non-Rust) linters
  # =============================================================================

  simpleLinters = {
    statix = mkSimpleLinter {
      name = "statix";
      packages = [pkgs.statix];
      src = sources.nix;
      command = "statix check .";
      fixCommand = "statix fix .";
    };

    deadnix = mkSimpleLinter {
      name = "deadnix";
      packages = [pkgs.deadnix];
      src = sources.nix;
      command = "deadnix --fail .";
      fixCommand = "deadnix --edit .";
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

    actionlint = mkSimpleLinter {
      name = "actionlint";
      packages = [pkgs.actionlint pkgs.shellcheck pkgs.findutils];
      src = sources.github-actions;
      command = ''find .github/workflows -name "*.yml" -exec actionlint {} +'';
    };

    hadolint = mkSimpleLinter {
      name = "hadolint";
      packages = [pkgs.hadolint];
      src = sources.dockerfile;
      command = "hadolint Dockerfile";
    };

    markdownlint = mkSimpleLinter {
      name = "markdownlint";
      packages = [pkgs.markdownlint-cli pkgs.findutils];
      src = sources.markdown;
      command = ''find . -name "*.md" -type f -exec markdownlint --config .config/markdownlint.yaml {} +'';
      fixCommand = ''find . -name "*.md" -not -path "./target/*" -type f -exec markdownlint --fix --config .config/markdownlint.yaml {} +'';
    };

    gitleaks = mkSimpleLinter {
      name = "gitleaks";
      packages = [pkgs.gitleaks pkgs.git];
      src = sources.all;
      command = "gitleaks detect --source . --no-git --verbose --config .config/gitleaks.toml";
    };
  };

  # =============================================================================
  # Linter definitions: Rust linters (use crane)
  # =============================================================================

  rustLinters = {
    clippy = (craneLib.cargoClippy (debugArgs
      // {
        cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
      })).overrideAttrs (old: {
      passthru =
        (old.passthru or {})
        // {
          fixRunner = eidLib.mkCargoRunner {
            name = "fix-clippy";
            command = "cargo clippy --workspace --fix --allow-dirty --all-targets --all-features --allow-no-vcs -- -D warnings";
          };
        };
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

  # Collect individual fix runners from linters that support auto-fixing
  individualFixRunners = lib.pipe allLinters [
    (lib.filterAttrs (_: drv: (drv.passthru or {}) ? fixRunner))
    (lib.mapAttrs (_: drv: drv.passthru.fixRunner))
  ];

  # Aggregate fix runner: calls each individual fix runner, then treefmt
  fixRunner = let
    fixScript = lib.concatStringsSep "\n" (lib.mapAttrsToList (name: runner: ''
        echo "=== Fixing: ${name} ==="
        ${runner}/bin/fix-${name}
      '')
      individualFixRunners);
  in
    pkgs.writeShellApplication {
      name = "eidetica-fix";
      runtimeInputs = [treefmtWrapper];
      text = ''
        ${fixScript}

        echo "=== Running treefmt ==="
        treefmt
      '';
    };
in {
  # All lint build targets
  builds = allLinters;

  # Fast subset for CI / nix flake check
  defaults = fastLinters;

  # Interactive runners
  runners = {
    fix = fixRunner;
  };

  # Export library for potential reuse
  lib = {
    inherit sourceWithExts mkSimpleLinter sources cleanSrc;
  };
}
