{
  description = "Eidetica: Remember Everything";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";

    fenix = {
      # Needed because rust-overlay, normally used by crane, doesn't have llvm-tools for coverage
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    advisory-db = {
      # Rust dependency security advisories
      url = "github:rustsec/advisory-db";
      flake = false;
    };

    # Flake helper for better organization with modules.
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

  outputs = inputs @ {
    self,
    flake-parts,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      imports = [
        flake-parts.flakeModules.easyOverlay
        inputs.treefmt-nix.flakeModule
      ];

      perSystem = {
        config,
        system,
        pkgs,
        ...
      }: let
        # Use the stable rust tools from fenix
        fenixStable = inputs.fenix.packages.${system}.stable;
        rustSrc = fenixStable.rust-src;
        toolChain = fenixStable.completeToolchain;

        # Use the toolchain with the crane helper functions
        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolChain;

        # Common arguments for mkCargoDerivation, a helper for the crane functions
        # Arguments can be included here even if they aren't used, but we only
        # place them here if they would otherwise show up in multiple places
        commonArgs = {
          inherit cargoArtifacts;
          # Clean the src to only have the Rust-relevant files
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
          buildInputs = with pkgs; [
            openssl
          ];
        };

        # Build only the cargo dependencies so we can cache them all when running in CI
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual crate itself, reusing the cargoArtifacts
        eidetica = craneLib.buildPackage (commonArgs
          // {
            doCheck = false; # Tests are run as a separate build with nextest
            meta.mainProgram = "eidetica";
          });
      in {
        packages = {
          default = eidetica;
          eidetica = eidetica;

          # Check code coverage with tarpaulin
          # This is currently broken because the tests require the running database
          coverage = craneLib.cargoTarpaulin (commonArgs
            // {
              # Use lcov output as thats far more widely supported
              cargoTarpaulinExtraArgs = "--skip-clean --include-tests --output-dir $out --out lcov";
            });

          # Run clippy (and deny all warnings) on the crate source
          clippy = craneLib.cargoClippy (commonArgs
            // {
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });

          # Check docs build successfully
          doc = craneLib.cargoDoc commonArgs;

          # Check formatting
          fmt = craneLib.cargoFmt commonArgs;

          # Run tests with cargo-nextest
          test = craneLib.cargoNextest commonArgs;

          # Audit dependencies
          # This only runs when Cargo.lock files change
          audit = craneLib.cargoAudit (commonArgs
            // {
              inherit (inputs) advisory-db;
            });
        };

        checks = {
          inherit eidetica;
          # Build almost every package in checks, with exceptions:
          # - coverage: It requires a full rebuild, and only needs to be run occasionally
          # - audit: Requires remote access
          # - test: Requires a running postgres db
          inherit (self.packages.${system}) clippy doc fmt;
        };

        # This also sets up `nix fmt` to run all formatters
        treefmt = {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            prettier = {
              enable = true;
              excludes = [
                "docs/mermaid.min.js"
                "docs/book/\\.html"
              ];
            };
            rustfmt = {
              enable = true;
              package = toolChain;
            };
            shfmt.enable = true;
          };
        };

        apps = rec {
          default = eidetica;
          eidetica.program = self.packages.${system}.eidetica;
        };

        overlayAttrs = {
          inherit (config.packages) eidetica;
        };

        devShells.default = pkgs.mkShell {
          name = "eidetica";
          shellHook = ''
            echo ---------------------
            task --list
            echo ---------------------
          '';

          # Include the packages from the defined checks and packages
          # Installs the full cargo toolchain and the extra tools, e.g. cargo-tarpaulin.
          inputsFrom =
            (builtins.attrValues self.checks.${system})
            ++ (builtins.attrValues self.packages.${system});

          # Extra inputs can be added here
          packages = with pkgs; [
            act # For running Github Actions locally
            go-task # Taskfile

            # Nix code analysis
            deadnix
            statix

            # Formattiing
            alejandra
            nodePackages.prettier

            # Releasing
            release-plz
            git-cliff

            # Profiling
            cargo-flamegraph

            # Documentation
            mdbook
            mdbook-mermaid
          ];

          # Many tools read this to find the sources for rust stdlib
          RUST_SRC_PATH = "${rustSrc}/lib/rustlib/src/rust/library";

          # Enable debug symbols in release builds
          CARGO_PROFILE_RELEASE_DEBUG = true;

          # Set the debug level for this crate while developing
          RUST_LOG = "eidetica=debug";
        };
      };
    };
}
