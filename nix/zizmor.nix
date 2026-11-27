# Backport self-repository workflow parsing without updating the Rust toolchain
# or unrelated flake inputs. Remove when the locked nixpkgs has zizmor >= 1.30.1.
{pkgs}:
pkgs.zizmor.overrideAttrs (finalAttrs: _: {
  version = "1.30.1";
  src = pkgs.fetchFromGitHub {
    owner = "zizmorcore";
    repo = "zizmor";
    tag = "v${finalAttrs.version}";
    hash = "sha256-Oay7x5bS85w7bszaUHRyh/PuMA1d4hd7l//HqsV8Z3A=";
  };
  cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
    inherit (finalAttrs) pname version src;
    hash = "sha256-0cDYV7e9S6r4xF4UTqH3DAKGmwOkJ/DrOJd8Ohr9LwM=";
  };
})
