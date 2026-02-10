# Shared helpers for Nix package definitions
#
# Provides mkCargoRunner for creating interactive cargo-based runners
# as writeShellApplication wrappers with proper build dependencies.
{
  pkgs,
  lib,
  defaultToolchain,
}: {
  inherit defaultToolchain;

  # Create an interactive runner for cargo-based commands
  # These are writeShellApplication wrappers that set up the build environment
  # (toolchain, pkg-config, openssl) and forward arguments to the user.
  #
  # Usage:
  #   mkCargoRunner {
  #     name = "bench-runner";
  #     command = "cargo bench --workspace --all-features";
  #   }
  mkCargoRunner = {
    name,
    toolchain ? defaultToolchain,
    extraInputs ? [],
    command,
  }:
    pkgs.writeShellApplication {
      inherit name;
      runtimeInputs = [toolchain pkgs.pkg-config pkgs.openssl] ++ extraInputs;
      text = ''
        export PKG_CONFIG_PATH="${lib.makeSearchPath "lib/pkgconfig" [pkgs.openssl.dev]}''${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
        ${command} "$@"
      '';
    };
}
