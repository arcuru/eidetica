# Development shell configuration
{
  pkgs,
  rustSrc,
  checks,
  extraPackages ? {},
}:
pkgs.mkShell {
  name = "eidetica";
  shellHook = ''
    echo "Eidetica Development Shell"
    echo ""
    echo "Run 'just' to see available commands"
    echo ""
  '';

  # Inherit build environments from checks and additional packages
  # This ensures all build dependencies are available in the dev shell
  # extraPackages includes coverage/sanitize packages not in checks
  inputsFrom = builtins.attrValues checks ++ builtins.attrValues extraPackages;

  # Additional development tools
  packages = with pkgs; [
    # CI/CD tools
    act # Run GitHub Actions locally
    go-task # Task runner
    just # Task runner
    nix-fast-build # Fast parallel Nix builds

    # Nix development tools
    deadnix # Find dead Nix code
    statix # Lint Nix code

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
  ];

  # Environment variables

  # Rust standard library sources for tools like rust-analyzer
  RUST_SRC_PATH = "${rustSrc}/lib/rustlib/src/rust/library";

  # Enable debug symbols in release builds for better profiling
  CARGO_PROFILE_RELEASE_DEBUG = true;

  # Default logging level for development
  RUST_LOG = "eidetica=debug";
}
