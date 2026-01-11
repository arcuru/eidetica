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

  # Cachix binary cache configuration
  nixConfig = {
    extra-substituters = ["https://eidetica.cachix.org"];
    extra-trusted-public-keys = ["eidetica.cachix.org-1:EDr+F/9jkD8aeThjJ4W3+4Yj3MH9fPx6slVLxF1HNSs="];
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      # Import flakes that have a flake-parts module
      imports = [
        inputs.flake-parts.flakeModules.easyOverlay
        inputs.treefmt-nix.flakeModule
      ];

      # System-level outputs (not per-system)
      flake = {
        nixosModules = {
          default = import ./nix/nixos-module.nix;
          eidetica = import ./nix/nixos-module.nix;
        };

        homeManagerModules = {
          default = import ./nix/home-manager.nix;
          eidetica = import ./nix/home-manager.nix;
        };
      };

      systems = [
        "aarch64-linux"
        "x86_64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem = {
        config,
        system,
        pkgs,
        lib,
        ...
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
          src = craneLib.cleanCargoSource ./.;
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

        # Library crate build
        eidetica-lib = craneLib.buildPackage (releaseArgs
          // {
            pname = "eidetica";
            cargoExtraArgs = "-p eidetica --all-features";
            doCheck = false; # Tests run separately with nextest
            meta = {
              description = "Eidetica library - A P2P decentralized database";
            };
          });

        # Binary crate build
        eidetica-bin = craneLib.buildPackage (releaseArgs
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

        # Non-root user setup for container security
        # Creates passwd/group/shadow files for the eidetica user (UID/GID 1000)
        nonRootUserSetup = let
          user = "eidetica";
          uid = "1000";
          gid = "1000";
        in [
          (pkgs.writeTextDir "etc/passwd" ''
            root:x:0:0:root:/root:/bin/false
            ${user}:x:${uid}:${gid}::/${user}:
          '')
          (pkgs.writeTextDir "etc/group" ''
            root:x:0:
            ${user}:x:${gid}:
          '')
          (pkgs.writeTextDir "etc/shadow" ''
            root:!x:::::::
            ${user}:!:::::::
          '')
        ];

        # License file for container compliance
        licenseFile = pkgs.runCommand "license" {} ''
          mkdir -p $out
          cp ${./LICENSE.txt} $out/LICENSE
        '';

        # OCI container image
        eidetica-image = pkgs.dockerTools.buildImage {
          name = "eidetica";
          tag = "dev";
          created = "now";

          copyToRoot = pkgs.buildEnv {
            name = "image-root";
            paths = [eidetica-bin licenseFile] ++ nonRootUserSetup;
            pathsToLink = ["/bin" "/etc" "/"];
          };

          config = {
            Cmd = ["${eidetica-bin}/bin/eidetica"];
            User = "1000:1000";
            WorkingDir = "/data";
            ExposedPorts = {
              "3000/tcp" = {};
            };
            Volumes = {
              "/data" = {};
            };
            Env = [
              "EIDETICA_DATA_DIR=/data"
              "EIDETICA_HOST=0.0.0.0"
              "HOME=/tmp"
            ];
            Labels = {
              "org.opencontainers.image.source" = "https://github.com/arcuru/eidetica";
              "org.opencontainers.image.description" = "Eidetica: Remember Everything - Decentralized Database";
              "org.opencontainers.image.licenses" = "AGPL-3.0-or-later";
            };
          };
        };

        # Integration tests for Nix modules and containers
        nixTests = import ./nix/tests.nix {
          inherit pkgs lib eidetica-bin eidetica-image;
          nixosModule = import ./nix/nixos-module.nix;
          homeManagerModule = import ./nix/home-manager.nix;
        };
      in {
        # Package definitions
        packages =
          {
            default = eidetica;
            eidetica = eidetica;
            eidetica-lib = eidetica-lib;
            eidetica-bin = eidetica-bin;
            eidetica-image = eidetica-image;

            # Check code coverage with tarpaulin (inmemory backend)
            coverage = craneLib.cargoTarpaulin (baseArgs
              // {
                # Use dummy artifacts since tarpaulin rebuilds everything anyway
                cargoArtifacts = craneLib.mkDummySrc {src = ./.;};
                # Use lcov output format for wider tool support and LLVM engine to avoid segfaults
                cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
                # Add llvm-tools-preview for tarpaulin
                nativeBuildInputs =
                  baseArgs.nativeBuildInputs
                  ++ [
                    (fenixStable.withComponents [
                      "llvm-tools-preview"
                    ])
                  ];
              });

            # Check code coverage with tarpaulin (sqlite backend)
            coverage-sqlite = craneLib.cargoTarpaulin (baseArgs
              // {
                pname = "coverage-sqlite";
                cargoArtifacts = craneLib.mkDummySrc {src = ./.;};
                cargoTarpaulinExtraArgs = "--skip-clean --output-dir $out --out lcov --all-features --engine llvm";
                # Set environment variable to use SQLite backend
                TEST_BACKEND = "sqlite";
                nativeBuildInputs =
                  baseArgs.nativeBuildInputs
                  ++ [
                    (fenixStable.withComponents [
                      "llvm-tools-preview"
                    ])
                  ];
              });

            # Run clippy with strict warnings
            clippy = craneLib.cargoClippy (debugArgs
              // {
                cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
              });

            # Compliance checking
            # This runs cargo deny "bans", "licenses", and "sources" only
            # It unfortunately does not run "advisories" because that requires a network connection
            deny = craneLib.cargoDeny (debugArgs
              // {
                cargoDenyExtraArgs = "--workspace --all-features";
                cargoDenyChecks = "bans licenses sources";
              });

            # Documentation generation
            doc = craneLib.cargoDoc (debugArgs
              // {
                # Only docs for this workspace, not the deps
                cargoDocExtraArgs = "--workspace --all-features --no-deps";
              });

            # Code formatting check
            fmt = craneLib.cargoFmt (debugArgs
              // {
                cargoExtraArgs = "--all";
              });

            # Test execution with nextest
            test = craneLib.cargoNextest (releaseArgs
              // {
                cargoNextestExtraArgs = "--workspace --all-features --no-fail-fast";
              });

            # Test library with minimal features (no defaults)
            test-minimal = craneLib.cargoNextest (releaseArgs
              // {
                pname = "eidetica-minimal";
                cargoNextestExtraArgs = "-p eidetica --no-default-features --features testing";
              });

            # Documentation tests
            doc-test = craneLib.cargoTest (releaseArgs
              // {
                cargoTestExtraArgs = "--doc --workspace --all-features";
              });

            # Benchmark execution
            bench = craneLib.mkCargoDerivation (releaseArgs
              // {
                pname = "eidetica-bench";
                buildPhaseCargoCommand = "cargo bench --workspace --all-features";
                doCheck = false;
                meta = {
                  description = "Eidetica benchmark suite";
                };
              });

            # Documentation examples testing
            book-test = craneLib.mkCargoDerivation (debugArgs
              // {
                pname = "book-test";
                src = ./.; # Needs the docs directory (not just cleanCargoSource)
                nativeBuildInputs = baseArgs.nativeBuildInputs ++ [pkgs.mdbook];

                # Use debug profile for faster builds
                # Clean any existing library artifacts to ensure single consistent build
                buildPhaseCargoCommand = ''
                  rm -f target/debug/deps/libeidetica-*.rlib target/debug/deps/libeidetica-*.rmeta
                  cargo build -p eidetica
                '';

                doCheck = true;
                checkPhase = ''
                  runHook preCheck
                  cd docs
                  mdbook test . -L ../target/debug/deps
                  runHook postCheck
                '';

                doInstallCargoArtifacts = false;
                installPhase = ''
                  runHook preInstall
                  mkdir -p $out
                  echo "Documentation examples tested successfully" > $out/result
                  runHook postInstall
                '';
              });
          }
          // lib.optionalAttrs pkgs.stdenv.isLinux {
            # Integration tests (excluded from `nix flake check` for performance)
            # Run manually with: nix build .#integration-nixos
            # VM tests only available on Linux
            integration-nixos = nixTests.integration-nixos;
            integration-container = nixTests.integration-container;
          };

        # CI checks - packages that run during `nix flake check`
        checks = {
          inherit eidetica eidetica-lib;

          inherit
            (config.packages)
            clippy # Linting and code quality
            book-test # Documentation tests in the mdbook
            doc # Documentation builds
            doc-test # Documentation example tests
            deny # License compliance
            fmt # Code formatting
            test # Test Suite
            test-minimal # Test library with no default features
            ;

          # Module evaluation tests (fast, all platforms)
          inherit
            (nixTests)
            eval-nixos
            eval-hm
            ;

          # Note: Excluded from default checks for performance reasons:
          # - coverage, coverage-sqlite: tarpaulin rebuilds everything
          # - bench: benchmarks take significant time
          # - integration-nixos, integration-container: VM tests are slow
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
            echo "Run 'just' to see available commands"
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
            just # Task runner
            nix-fast-build # Fast parallel Nix builds

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
        };
      };
    };
}
