{
  description = "Eidetica: Remember Everything";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";

    # Needed because rust-overlay, normally used by crane, doesn't have llvm-tools for coverage
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    # Rust dependency security advisories
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };

    # Flake helper for better organization with modules
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    # For creating a universal `nix fmt`
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      # Import flakes that have a flake-parts module
      imports = [
        inputs.flake-parts.flakeModules.easyOverlay
        inputs.treefmt-nix.flakeModule
      ];

      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      perSystem = {
        config,
        system,
        pkgs,
        ...
      }: let
        # Rust toolchain configuration using fenix
        fenixStable = inputs.fenix.packages.${system}.complete;
        rustSrc = fenixStable.rust-src;
        toolChain = fenixStable.toolchain;

        # Crane library with custom Rust toolchain
        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolChain;

        # Common arguments for all cargo derivations
        # These arguments are shared across build phases to maintain consistency
        commonArgs = {
          inherit cargoArtifacts;
          # Clean source to include only Rust-relevant files
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
          buildInputs = with pkgs; [
            openssl
          ];
        };

        # Build cargo dependencies separately for better caching
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Main package build
        eidetica = craneLib.buildPackage (commonArgs
          // {
            cargoExtraArgs = "--workspace --all-targets --all-features";
            doCheck = false; # Tests run separately with nextest
            meta = {
              description = "A P2P decentralized database";
              mainProgram = "eidetica";
            };
          });
      in {
        # Package definitions
        packages = {
          default = eidetica;
          eidetica = eidetica;

          # Development and Quality Assurance packages

          # Check code coverage with tarpaulin
          coverage = craneLib.cargoTarpaulin (commonArgs
            // {
              # Use lcov output format for wider tool support
              cargoTarpaulinExtraArgs = "--skip-clean --include-tests --output-dir $out --out lcov";
            });

          # Run clippy with strict warnings
          clippy = craneLib.cargoClippy (commonArgs
            // {
              cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
            });

          # License compliance checking
          deny = craneLib.cargoDeny commonArgs;

          # Documentation generation
          doc = craneLib.cargoDoc (commonArgs
            // {
              cargoDocExtraArgs = "--workspace --all-features";
            });

          # Code formatting check
          fmt = craneLib.cargoFmt (commonArgs
            // {
              cargoExtraArgs = "--all";
            });

          # Test execution with nextest
          test = craneLib.cargoNextest (commonArgs
            // {
              cargoNextestExtraArgs = "--workspace --all-features --no-fail-fast";
            });

          # Security audit of dependencies
          audit = craneLib.cargoAudit (commonArgs
            // {
              inherit (inputs) advisory-db;
            });
        };

        # CI checks - packages that run during `nix flake check`
        checks = {
          inherit eidetica;
          # Include most packages in CI checks
          # Excluded: coverage (expensive)
          inherit (config.packages) audit clippy doc deny fmt test;
        };

        # Formatting configuration via treefmt
        treefmt = {
          projectRootFile = "flake.nix";
          programs = {
            # Nix formatting
            alejandra.enable = true;

            # Markdown, JSON, YAML formatting
            prettier = {
              enable = true;
              excludes = [
                "docs/book/\\.html"
              ];
            };

            # Rust formatting
            rustfmt.enable = true;

            # Shell script formatting
            shfmt.enable = true;
          };
        };

        # Application definitions
        apps = rec {
          default = eidetica;
          eidetica = {
            type = "app";
            program = config.packages.eidetica;
            meta.description = "Run the Eidetica database";
          };
        };

        # Overlay attributes for easy access to packages
        overlayAttrs = {
          inherit (config.packages) eidetica;
        };

        # Development shell configuration
        devShells.default = pkgs.mkShell {
          name = "eidetica";
          shellHook = ''
            echo ---------------------
            task --list
            echo ---------------------
          '';

          # Inherit build environments from all packages and checks
          # This ensures all build dependencies are available in the dev shell
          inputsFrom =
            (builtins.attrValues config.checks)
            ++ (builtins.attrValues config.packages);

          # Additional development tools
          packages = with pkgs; [
            # CI/CD tools
            act # Run GitHub Actions locally
            go-task # Task runner

            # Nix development tools
            deadnix # Find dead Nix code
            statix # Lint Nix code

            # Code formatting
            alejandra # Nix formatter
            nodePackages.prettier # General formatter

            # Release management
            release-plz # Automated releases
            git-cliff # Changelog generation

            # Performance analysis
            cargo-flamegraph # Profiling

            # Documentation
            mdbook # Book generation
            mdbook-mermaid # Mermaid diagrams
          ];

          # Environment variables

          # Rust standard library sources for tools like rust-analyzer
          RUST_SRC_PATH = "${rustSrc}/lib/rustlib/src/rust/library";

          # Enable debug symbols in release builds for better profiling
          CARGO_PROFILE_RELEASE_DEBUG = true;

          # Default logging level for development
          RUST_LOG = "eidetica=debug";
        };
      };
    };
}
