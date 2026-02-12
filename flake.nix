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
        # Import toolchain setup
        toolchain = import ./nix/toolchain.nix {inherit inputs system pkgs;};
        inherit
          (toolchain)
          fenixStable
          fenixNightly
          rustSrc
          craneLib
          craneLibNightly
          baseArgs
          baseArgsNightly
          releaseArgs
          benchArgs
          debugArgs
          debugArgsNightly
          asanArgs
          lsanArgs
          ;

        # Shared helpers for package definitions
        eidLib = import ./nix/lib.nix {
          inherit pkgs lib;
          defaultToolchain = fenixStable.toolchain;
        };

        # Import package groups
        mainPkgs = import ./nix/packages/main.nix {inherit craneLib releaseArgs;};

        testPkgs = import ./nix/packages/test.nix {inherit craneLib debugArgs baseArgs pkgs lib;};
        coveragePkgs = import ./nix/packages/coverage.nix {inherit craneLibNightly baseArgsNightly fenixNightly eidLib pkgs lib;};
        sanitizePkgs = import ./nix/packages/sanitize.nix {inherit craneLibNightly debugArgsNightly asanArgs lsanArgs fenixNightly pkgs lib;};
        docPkgs = import ./nix/packages/doc.nix {inherit craneLib debugArgs pkgs lib;};
        lintPkgs = import ./nix/packages/lint.nix {
          inherit craneLib craneLibNightly baseArgs baseArgsNightly debugArgs eidLib pkgs lib;
          treefmtWrapper = config.treefmt.build.wrapper;
        };
        benchPkgs = import ./nix/packages/bench.nix {inherit craneLib benchArgs eidLib;};

        # Import other modules
        containerPkgs = import ./nix/container.nix {
          inherit pkgs;
          inherit (mainPkgs) eidetica-bin;
        };

        nixTests = import ./nix/tests.nix {
          inherit pkgs lib;
          inherit (mainPkgs) eidetica-bin;
          eidetica-image = containerPkgs.image;
          nixosModule = import ./nix/nixos-module.nix;
          homeManagerModule = import ./nix/home-manager.nix;
        };

        # Helper to create aggregate packages
        mkAggregate = name: packages:
          pkgs.symlinkJoin {
            inherit name;
            paths = builtins.attrValues packages;
          };
        mkAll = name: mkAggregate "${name}-all";
      in {
        # Hierarchical package structure via legacyPackages
        # Pattern: nix build .#<group>.<target> for specific targets
        #          nix build .#<group>.default for sensible default
        #          nix build .#<group>.all for all targets in group
        #          nix run .#<group>.<target> for interactive runners (via apps)
        legacyPackages = {
          # Test packages - nested structure with .default and .all aggregates
          # nix build .#test.default (sqlite), .#test.sqlite, .#test.all
          test =
            testPkgs.builds
            // {
              default = testPkgs.builds.sqlite;
              all = mkAll "test" testPkgs.builds;
              inherit (testPkgs) artifacts;
            };

          # Bench package - nix build .#bench runs hermetic benchmarks
          # nix run .#bench for interactive benchmarks
          bench =
            benchPkgs.builds.default
            // {
              inherit (benchPkgs) artifacts;
            };

          # Coverage group - nix build .#coverage.default (sqlite), .#coverage.sqlite, .#coverage.all
          coverage =
            coveragePkgs.builds
            // {
              default = mkAll "coverage" coveragePkgs.defaults;
              all = mkAll "coverage" coveragePkgs.builds;
              inherit (coveragePkgs) artifacts;
            };

          # Sanitizer group - nix build .#sanitize.default (asan+lsan), .#sanitize.asan, .#sanitize.all
          sanitize =
            sanitizePkgs.builds
            // lib.optionalAttrs (sanitizePkgs.builds != {}) {
              default = mkAll "sanitize" sanitizePkgs.defaults;
              all = mkAll "sanitize" sanitizePkgs.builds;
            };

          # Documentation group - nix build .#doc.default (fast), .#doc.api, .#doc.book, .#doc.booktest
          doc =
            docPkgs.builds
            // {
              default = mkAll "doc" docPkgs.defaults;
              all = mkAll "doc" docPkgs.builds;
            };

          # Lint group - nix build .#lint.default (fast), .#lint.clippy, .#lint.all
          lint =
            lintPkgs.builds
            // {
              default = mkAll "lint" lintPkgs.defaults;
              all = mkAll "lint" lintPkgs.builds;
            };

          # Main eidetica packages - nix build .#eidetica.{bin,image}
          eidetica = {
            bin = mainPkgs.eidetica-bin;
            inherit (containerPkgs) image;
          };

          # Default package (eidetica binary)
          default = mainPkgs.eidetica-bin;

          # Integration tests (Linux only) - nix build .#integration.default (all), .#integration.nixos
          integration = lib.optionalAttrs pkgs.stdenv.isLinux (
            nixTests.integration
            // {
              default = mkAll "integration" nixTests.integration;
              all = mkAll "integration" nixTests.integration;
            }
          );

          # Eval tests - nix build .#eval.default (all), .#eval.nixos, .#eval.hm
          eval =
            nixTests.eval
            // {
              default = mkAll "eval" nixTests.eval;
              all = mkAll "eval" nixTests.eval;
            };
        };

        # Standard packages output for `nix build` (bare) and `nix build .#eidetica-bin`
        packages = {
          default = mainPkgs.eidetica-bin;
          inherit (mainPkgs) eidetica-bin;
        };

        # CI checks - packages that run during `nix flake check`
        # Each check builds the corresponding .default from legacyPackages
        # Excluded for performance: coverage, bench, integration, sanitize
        checks = {
          eidetica = mainPkgs.eidetica-bin;
          test = testPkgs.builds.sqlite;
          lint = mkAggregate "lint" lintPkgs.defaults;
          doc = mkAggregate "doc" docPkgs.defaults;
          eval = mkAggregate "eval" nixTests.eval;
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

            # Spell checking
            typos = {
              enable = true;
              configFile = ".config/typos.toml";
            };
          };
        };

        # Application definitions (for nix run)
        # These provide interactive runners that accept arguments
        # Note: apps must be flat - nested structures go through legacyPackages
        apps = let
          mkApp = program: description: {
            type = "app";
            inherit program;
            meta = {inherit description;};
          };
        in
          {
            default = mkApp "${mainPkgs.eidetica-bin}/bin/eidetica" "Run the Eidetica binary";
            eidetica = mkApp "${mainPkgs.eidetica-bin}/bin/eidetica" "Run the Eidetica database";
            fix = mkApp "${lintPkgs.runners.fix}/bin/eidetica-fix" "Run auto-fixes and format code";
            bench = mkApp "${benchPkgs.runners.default}/bin/bench-runner" "Run benchmarks interactively";
            coverage = mkApp "${coveragePkgs.runners.default}/bin/coverage-runner" "Run coverage interactively";

            # Test runners - flat names (nested access via legacyPackages)
            # nix run .#test (default: no backend set), nix run .#test-sqlite, etc.
            test = mkApp "${testPkgs.runners.default}/bin/test-runner-default" "Run tests (override backend with TEST_BACKEND)";
            test-inmemory = mkApp "${testPkgs.runners.inmemory}/bin/test-runner-inmemory" "Run tests with inmemory backend";
            test-sqlite = mkApp "${testPkgs.runners.sqlite}/bin/test-runner-sqlite" "Run tests with SQLite backend";
            test-all = mkApp "${testPkgs.runners.all}/bin/test-runner-all" "Run tests with all backends";
          }
          // lib.optionalAttrs pkgs.stdenv.isLinux {
            test-postgres = mkApp "${testPkgs.runners.postgres}/bin/test-runner-postgres" "Run tests with PostgreSQL backend";
          };

        # Overlay attributes for easy access to packages
        overlayAttrs = {
          inherit (config.legacyPackages) eidetica;
        };

        # Development shell configuration
        devShells.default = import ./nix/dev-shell.nix {
          inherit pkgs lib rustSrc fenixNightly;
          # Pass the full list of packages so the devshell can pickup the dependencies
          devPackages =
            {inherit (mainPkgs) eidetica-bin;}
            // testPkgs.builds
            // lintPkgs.builds
            // docPkgs.builds
            // coveragePkgs.builds
            // sanitizePkgs.builds;
        };
      };
    };
}
