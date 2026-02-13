# Development shell configuration
{
  pkgs,
  lib,
  rustSrc,
  toolChainNightly,
  devPackages,
}: let
  # Wrapper to run cargo with nightly toolchain (for udeps, miri, sanitizers, etc.)
  # Prepends full nightly toolchain to PATH so rustc, cargo, etc. are all nightly
  cargo-nightly = pkgs.writeShellScriptBin "cargo-nightly" ''
    export PATH="${toolChainNightly}/bin:$PATH"
    exec cargo "$@"
  '';
in
  pkgs.mkShell {
    name = "eidetica";
    shellHook = ''
      echo "Eidetica Development Shell"
      echo ""
      echo "Run 'just' to see available commands"
      echo ""
    '';

    # Inherit build environments from all dev packages
    # This ensures all build dependencies are available
    inputsFrom = builtins.attrValues devPackages;

    # Additional development tools
    packages =
      [
        # Nightly toolchain wrapper (for udeps, miri, sanitizers, etc.)
        cargo-nightly
      ]
      ++ (with pkgs; [
        # CI/CD tools
        act # Run GitHub Actions locally
        just # Task runner
        nix-fast-build # Fast parallel Nix builds

        # Linting tools
        deadnix # Find dead Nix code
        statix # Lint Nix code
        shellcheck # Lint shell scripts
        yamllint # Lint YAML files
        actionlint # Lint GitHub Actions workflows
        hadolint # Lint Dockerfiles
        markdownlint-cli # Lint Markdown files
        gitleaks # Detect secrets in code

        # Code formatting and quality
        alejandra # Nix formatter
        nodePackages.prettier # General formatter
        typos # Spell checker

        # Release management
        release-plz # Automated releases
        git-cliff # Changelog generation

        # Performance analysis
        cargo-flamegraph # Profiling

        # Code coverage
        lcov # Merge coverage reports

        # Documentation
        mdbook # Book generation
        mdbook-mermaid # Mermaid diagrams
        lychee # Link validation

        # Memory safety analysis
        cargo-careful # Run with extra runtime checks
      ]);

    # Environment variables

    # Rust standard library sources for tools like rust-analyzer
    RUST_SRC_PATH = "${rustSrc}/lib/rustlib/src/rust/library";

    # Enable debug symbols in release builds for better profiling
    CARGO_PROFILE_RELEASE_DEBUG = true;

    # Default logging level for development
    RUST_LOG = "eidetica=debug";
  }
  // lib.optionalAttrs pkgs.stdenv.isLinux {
    RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
  }
