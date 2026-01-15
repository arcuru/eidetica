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
        # Root source for packages that need full source tree
        rootSrc = ./.;

        # Import toolchain setup
        toolchain = import ./nix/toolchain.nix {inherit inputs system pkgs;};
        inherit
          (toolchain)
          fenixStable
          rustSrc
          craneLib
          baseArgs
          cargoArtifacts
          cargoArtifactsDebug
          cargoArtifactsAsan
          cargoArtifactsLsan
          releaseArgs
          debugArgs
          asanArgs
          lsanArgs
          noDepsArgs
          ;

        # Import package groups
        mainPkgs = import ./nix/packages/main.nix {inherit craneLib releaseArgs;};
        testPkgs = import ./nix/packages/test.nix {inherit craneLib debugArgs baseArgs pkgs lib;};
        coveragePkgs = import ./nix/packages/coverage.nix {inherit craneLib baseArgs fenixStable pkgs lib rootSrc;};
        sanitizePkgs = import ./nix/packages/sanitize.nix {inherit craneLib noDepsArgs asanArgs lsanArgs fenixStable pkgs lib;};
        docPkgs = import ./nix/packages/doc.nix {inherit craneLib debugArgs noDepsArgs pkgs rootSrc;};
        lintPkgs = import ./nix/packages/lint.nix {inherit craneLib debugArgs noDepsArgs pkgs;};
        standalonePkgs = import ./nix/packages/standalone.nix {inherit craneLib releaseArgs baseArgs pkgs;};

        # Import other modules
        containerPkgs = import ./nix/container.nix {
          inherit pkgs;
          eidetica-bin = mainPkgs.eidetica-bin;
        };

        nixTests = import ./nix/tests.nix {
          inherit pkgs lib;
          eidetica-bin = mainPkgs.eidetica-bin;
          eidetica-image = containerPkgs.eidetica-image;
          nixosModule = import ./nix/nixos-module.nix;
          homeManagerModule = import ./nix/home-manager.nix;
        };

        # Helper to create "all" aggregate package
        mkAll = name: packages:
          pkgs.symlinkJoin {
            name = "${name}-all";
            paths = builtins.attrValues packages;
          };
      in {
        # Hierarchical package structure via legacyPackages
        # Pattern: .#<group> runs fast/CI subset, .#<group>.full runs all, .#<group>.<name> runs specific
        legacyPackages = {
          # Test group - .#test runs CI tests, .#test.full runs all backends
          test =
            mkAll "test" testPkgs.testFast
            // testPkgs.testPackages
            // {full = mkAll "test-full" testPkgs.testPackages;};

          # Coverage group - .#coverage runs inmemory, .#coverage.full runs all backends
          coverage =
            mkAll "coverage" coveragePkgs.coverageFast
            // coveragePkgs.coveragePackages
            // {full = mkAll "coverage-full" coveragePkgs.coveragePackages;};

          # Sanitizer group - .#sanitize runs fast (asan/lsan), .#sanitize.full includes miri
          sanitize =
            mkAll "sanitize" sanitizePkgs.sanitizeFast
            // sanitizePkgs.sanitizePackages
            // {full = mkAll "sanitize-full" sanitizePkgs.sanitizePackages;};

          # Documentation group - .#doc runs CI checks, .#doc.full includes slow builds
          doc =
            mkAll "doc" docPkgs.docFast
            // docPkgs.docPackages
            // {full = mkAll "doc-full" docPkgs.docPackages;};

          # Lint group - .#lint runs CI checks, .#lint.full includes udeps
          lint =
            mkAll "lint" lintPkgs.lintFast
            // lintPkgs.lintPackages
            // {full = mkAll "lint-full" lintPkgs.lintPackages;};

          # Top-level packages
          default = mainPkgs.eidetica;
          inherit (mainPkgs) eidetica eidetica-lib eidetica-bin;
          inherit (containerPkgs) eidetica-image;

          # Standalone packages
          inherit (standalonePkgs) bench min-versions;

          # Integration tests (Linux only)
          integration = lib.optionalAttrs pkgs.stdenv.isLinux {
            nixos = nixTests.integration-nixos;
            container = nixTests.integration-container;
          };
        };

        # CI checks - packages that run during `nix flake check`
        # Composed from fast groups: lint, test, doc, plus main packages and module eval tests
        # Excluded for performance: coverage, bench, integration, sanitize
        checks =
          {inherit (mainPkgs) eidetica eidetica-lib;}
          // lintPkgs.lintFast
          // testPkgs.testFast
          // docPkgs.docFast
          // {inherit (nixTests) eval-nixos eval-hm;};

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
          default = eidetica-app;
          eidetica-app = {
            type = "app";
            program = config.legacyPackages.eidetica;
            meta.description = "Run the Eidetica database";
          };
        };

        # Overlay attributes for easy access to packages
        overlayAttrs = {
          inherit (config.legacyPackages) eidetica;
        };

        # Development shell configuration
        devShells.default = import ./nix/dev-shell.nix {
          inherit pkgs rustSrc;
          checks = config.checks;
        };
      };
    };
}
