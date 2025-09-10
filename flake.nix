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
        lib,
        ...
      }: let
        inherit (pkgs) stdenv;

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
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [
            pkg-config # Required for OpenSSL linking
          ];
          buildInputs = with pkgs;
            [
              openssl
            ]
            ++ lib.optionals stdenv.isDarwin [
              # Darwin-specific frameworks required for networking and crypto
              # Not actually tested
              darwin.apple_sdk.frameworks.Security
              darwin.apple_sdk.frameworks.SystemConfiguration
            ];
        };

        # Build cargo dependencies once for caching (release profile by default)
        # This creates a build cache that can be reused by all release builds
        cargoArtifacts = craneLib.buildDepsOnly baseArgs;

        # Build cargo dependencies for debug profile (used by book-test)
        # Separate debug cache needed because debug/release profiles are incompatible
        cargoArtifactsDebug = craneLib.buildDepsOnly (baseArgs
          // {
            pname = "debug-deps";
            CARGO_PROFILE = "dev";
          });

        # Common arguments for cargo derivations that need cargoArtifacts
        # Most builds use the release artifacts for better performance
        commonArgs =
          baseArgs
          // {
            inherit cargoArtifacts;
          };

        # Library crate build
        eidetica-lib = craneLib.buildPackage (commonArgs
          // {
            pname = "eidetica";
            cargoExtraArgs = "-p eidetica --all-features";
            doCheck = false; # Tests run separately with nextest
            meta = {
              description = "Eidetica library - A P2P decentralized database";
            };
          });

        # Binary crate build
        eidetica-bin = craneLib.buildPackage (commonArgs
          // {
            pname = "eidetica-bin";
            cargoExtraArgs = "-p eidetica-bin";
            doCheck = false; # Tests run separately with nextest
            meta = {
              description = "Eidetica binary";
              mainProgram = "eidetica";
            };
          });

        # Main package
        eidetica = eidetica-bin;
      in {
        # Package definitions
        packages = {
          default = eidetica;
          eidetica = eidetica;
          eidetica-lib = eidetica-lib;
          eidetica-bin = eidetica-bin;

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
          # Only docs for this workspace, not the deps
          doc = craneLib.cargoDoc (commonArgs
            // {
              cargoDocExtraArgs = "--workspace --all-features --no-deps";
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

          # Benchmark execution
          bench = craneLib.mkCargoDerivation (commonArgs
            // {
              pname = "eidetica-bench";
              buildPhaseCargoCommand = "cargo bench --workspace --all-features";
              doCheck = false;
              meta = {
                description = "Eidetica benchmark suite";
              };
            });

          # Security audit of dependencies
          audit = craneLib.cargoAudit (commonArgs
            // {
              inherit (inputs) advisory-db;
            });

          # Documentation examples testing
          # Uses debug cargoArtifacts for better build cache reuse
          book-test = craneLib.mkCargoDerivation (baseArgs
            // {
              pname = "book-test";
              src = ./.; # Needs the docs directory
              cargoArtifacts = cargoArtifactsDebug; # Reuse debug dependencies
              nativeBuildInputs = baseArgs.nativeBuildInputs ++ [pkgs.mdbook];

              # Force debug build to match cargoArtifactsDebug
              CARGO_PROFILE = "dev";
              buildPhaseCargoCommand = "cargoWithProfile build -p eidetica --features full";

              doCheck = true;
              checkPhase = ''
                runHook preCheck
                RUST_LOG=warn mdbook test docs -L target/debug/deps
                runHook postCheck
              '';

              installPhase = ''
                runHook preInstall
                mkdir -p $out
                echo "Documentation examples tested successfully" > $out/result
                runHook postInstall
              '';
            });
        };

        # CI checks - packages that run during `nix flake check`
        checks = {
          inherit eidetica eidetica-lib;

          inherit
            (config.packages)
            audit # Security vulnerabilities
            clippy # Linting and code quality
            doc # Documentation builds
            deny # License compliance
            fmt # Code formatting
            test # Test Suite
            ;

          # Note: Excluded from CI for performance reasons:
          # - coverage: expensive tarpaulin run
          # - book-test: rebuilds dependencies for mdbook compatibility
          # - bench: benchmarks are run separately
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
            echo "Eidetica Development Shell"
            echo ""
            echo "Available commands:"
            echo "  task --list       - Show all task commands"
            echo "  cargo nextest run - Run tests with nextest"
            echo "  nix flake check   - Run all CI checks"
            echo ""
            task --list
            echo ""
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
