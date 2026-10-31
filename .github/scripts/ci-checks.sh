#!/usr/bin/env bash
# Build every check in one flake attribute set as a single Nix graph, streaming
# fresh build logs, then replay each leaf check's stored log under its platform
# and check name. Replaying a stored log does not rerun a check, and a missing
# or unreadable log never changes the build's exit status.
#
# Usage: ci-checks.sh <flake attribute set>, e.g. legacyPackages.aarch64-linux.ci-pr
set -euo pipefail

set_attr=${1:?usage: ci-checks.sh <flake attribute set>}

# Evaluation failure fails the job before anything is built.
names=$(nix eval --raw ".#$set_attr" --apply 's: builtins.concatStringsSep "\n" (builtins.attrNames s)')
targets=()
while IFS= read -r name; do
  if [[ -n $name ]]; then
    targets+=(".#$set_attr.$name")
  fi
done <<<"$names"
if ((${#targets[@]} == 0)); then
  echo "No checks found in .#$set_attr" >&2
  exit 1
fi

build_status=0
nix build -L --no-link --keep-going "${targets[@]}" || build_status=$?

# Aggregates expose their constituents; every other check is its own leaf.
# Lines are "<system> <check> <derivation>"; derivation paths still name the
# logs of checks that failed or never started.
# shellcheck disable=SC2016 # Nix, not shell, interpolates this expression.
leaves='s: let
  leaves = label: d:
    if d ? constituents
    then builtins.concatLists (builtins.attrValues (builtins.mapAttrs (n: leaves "${label}.${n}") d.constituents))
    else ["${d.system} ${label} ${d.drvPath}"];
in builtins.concatStringsSep "\n" (builtins.concatLists (builtins.attrValues (builtins.mapAttrs leaves s)))'
if ! logs=$(nix eval --raw ".#$set_attr" --apply "$leaves"); then
  echo "::warning::Could not list stored check logs for .#$set_attr; no test result inferred"
  exit "$build_status"
fi
while read -r system check drv; do
  if [[ -z $drv ]]; then
    continue
  fi
  echo "::group::$system $check — replaying stored derivation log; check is not being rerun"
  if ! nix log "$drv" </dev/null; then
    echo "::warning::Stored log unavailable for $system $check (local store or configured binary caches); no test result inferred"
  fi
  echo '::endgroup::'
done <<<"$logs"
exit "$build_status"
