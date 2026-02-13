#!/usr/bin/env bash
# Setup Nix configuration with caches
# Usage: ./setup-nix.sh [--attic] [--cachix] [tools...]
#
# Environment variables:
#   ATTIC_SERVER_URL, ATTIC_AUTH_TOKEN - Attic configuration
#   CACHIX_AUTH_TOKEN - Cachix configuration

set -euo pipefail

setup_attic=false
setup_cachix=false
tools=()

while [[ $# -gt 0 ]]; do
  case "$1" in
  --attic)
    setup_attic=true
    shift
    ;;
  --cachix)
    setup_cachix=true
    shift
    ;;
  *)
    tools+=("$1")
    shift
    ;;
  esac
done

# Configure Nix
BUILD_DIR=${NIX_BUILD_DIR:-$HOME/.cache/nix/build}
mkdir -p "$BUILD_DIR"
chmod 700 "$BUILD_DIR"
SUBSTITUTERS="https://cache.nixos.org?priority=40 https://eidetica.cachix.org?priority=50"
TRUSTED_KEYS="cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= eidetica.cachix.org-1:EDr+F/9jkD8aeThjJ4W3+4Yj3MH9fPx6slVLxF1HNSs= eidetica:hm/EK+V7LITUUdJi9AxDNic5j6cB1EhSQy0R+z2uoPU="

if [[ -n ${ATTIC_SERVER_URL:-} ]]; then
  SUBSTITUTERS="$ATTIC_SERVER_URL/eidetica?priority=10 $SUBSTITUTERS"
fi

# Write to system nix.conf (works in container where we're root)
# Falls back to user config if not root
if [[ -w /etc/nix/nix.conf ]] || [[ -w /etc/nix ]]; then
  mkdir -p /etc/nix
  cat >/etc/nix/nix.conf <<EOF
experimental-features = nix-command flakes
sandbox = false
accept-flake-config = true
build-dir = $BUILD_DIR
extra-substituters = $SUBSTITUTERS
extra-trusted-public-keys = $TRUSTED_KEYS
EOF
else
  mkdir -p ~/.config/nix
  cat >~/.config/nix/nix.conf <<EOF
experimental-features = nix-command flakes
sandbox = false
accept-flake-config = true
build-dir = $BUILD_DIR
extra-substituters = $SUBSTITUTERS
extra-trusted-public-keys = $TRUSTED_KEYS
EOF
fi

# Install tools
if [[ ${#tools[@]} -gt 0 ]]; then
  nix profile add "${tools[@]}"
fi

# Configure Attic
if [[ $setup_attic == "true" && -n ${ATTIC_AUTH_TOKEN:-} && -n ${ATTIC_SERVER_URL:-} ]]; then
  nix profile add nixpkgs#attic-client
  attic login eidetica "$ATTIC_SERVER_URL" "$ATTIC_AUTH_TOKEN"
fi

# Configure Cachix
if [[ $setup_cachix == "true" && -n ${CACHIX_AUTH_TOKEN:-} ]]; then
  nix profile add nixpkgs#cachix
  cachix authtoken "$CACHIX_AUTH_TOKEN"
fi
