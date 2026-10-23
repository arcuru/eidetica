#!/usr/bin/env bash
# Sign and upload the outputs this protected main job already checked.
set -euo pipefail

# Defense in depth behind the protected job's exact main/event guard.
[[ $GITHUB_REPOSITORY == arcuru/eidetica && $GITHUB_REPOSITORY_OWNER == arcuru &&
  $GITHUB_REF == refs/heads/main &&
  ($GITHUB_EVENT_NAME == push || $GITHUB_EVENT_NAME == workflow_dispatch) ]] || {
  echo 'Cache publication refused for this repository/event/ref' >&2
  exit 1
}

# The native path listing comes only from this job's successful checks.
list="$RUNNER_TEMP/eidetica-publish-paths"
[[ -s $list ]] || {
  echo 'No checked outputs to publish' >&2
  exit 1
}
mapfile -t paths < <(sort -u "$list")
((${#paths[@]} > 0)) || exit 1
for path in "${paths[@]}"; do
  [[ $path =~ ^/nix/store/[a-z0-9]{32}-[a-zA-Z0-9+._?=-]+$ ]] || {
    echo 'Invalid publication store path' >&2
    exit 1
  }
done
# Explicit store paths cannot trigger evaluation or another build.
nix path-info "${paths[@]}" >/dev/null
umask 077
key=$(mktemp "$RUNNER_TEMP/eidetica-cache-key.XXXXXX")
trap 'rm -f "$key"' EXIT
printf '%s\n' "${NIX_CACHE_SIGNING_KEY:?missing signing key}" >"$key"
unset NIX_CACHE_SIGNING_KEY
nix store sign --recursive --key-file "$key" "${paths[@]}"
# Cargo artifacts are already compressed; avoid expensive outer XZ.
cache='s3://cache-eidetica-dev?region=auto&endpoint=https://ab01ccd7fce6ab984e4c956734085680.r2.cloudflarestorage.com&compression=zstd'
nix copy --to "$cache" "${paths[@]}"
