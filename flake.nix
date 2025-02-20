{
  description = "Eidetica - Remember Everything";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

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
      }: {
        # This also sets up `nix fmt` to run all formatters
        treefmt = {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            prettier.enable = true;
            shfmt.enable = true;
          };
        };

        devShells.default = pkgs.mkShell {
          name = "eidetica";

          # Include the packages from the defined checks and packages
          # Installs the full cargo toolchain and the extra tools, e.g. cargo-tarpaulin.
          inputsFrom =
            (builtins.attrValues self.checks.${system})
            ++ (builtins.attrValues self.packages.${system});

          # Extra inputs can be added here
          packages = with pkgs; [
            act # For running Github Actions locally

            # Python
            uv

            # Nix code analysis
            deadnix
            statix

            # Formattiing
            alejandra
            nodePackages.prettier

            # Releasing
            git-cliff
          ];

          # Database location for local development
          DATABASE_URL = "sqlite:///.cache/eidetica.db";

          # Postgres location for testing
          POSTGRES_URL = "postgresql://postgres:postgres@localhost:55556";
        };
      };
    };
}
