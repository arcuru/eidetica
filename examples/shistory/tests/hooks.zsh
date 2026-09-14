#!/usr/bin/env zsh
set -eu

example_dir=${0:A:h:h}
tmp=${TMPDIR:-/tmp}/shistory-hooks-$$
mkdir "$tmp"
trap 'rm -rf "$tmp"' EXIT

print -r -- "#!${commands[zsh]}" > "$tmp/shistory"
cat >> "$tmp/shistory" <<'STUB'
print -r -- "$*" >> "$SHISTORY_HOOK_LOG"
if [[ $1 == start ]]; then
  [[ ${SHISTORY_STUB_FAIL:-} == start ]] && exit 1
  print -r -- synthetic-id
elif [[ $1 == finish ]]; then
  [[ ${SHISTORY_STUB_FAIL:-} == finish ]] && exit 1
fi
STUB
chmod +x "$tmp/shistory"

export SHISTORY_BIN="$tmp/shistory"
export SHISTORY_HOOK_LOG="$tmp/calls"
export SHISTORY_HOST=test-host
source "$example_dir/shistory.zsh"
add-zsh-hook -d preexec _shistory_preexec
add-zsh-hook -d precmd _shistory_precmd
: > "$SHISTORY_HOOK_LOG"

# A normal command records start and finish while preserving its status.
_shistory_preexec 'false synthetic-secret'
set +e
false
_shistory_precmd
command_status=$?
set -e
[[ $command_status == 1 ]]
grep -q '^start .*-- false synthetic-secret$' "$SHISTORY_HOOK_LOG"
grep -q '^finish synthetic-id .*--exit-status 1$' "$SHISTORY_HOOK_LOG"

# Leading-space and disabled commands never reach the binary.
: > "$SHISTORY_HOOK_LOG"
_shistory_preexec ' hidden synthetic-secret'
[[ ! -s "$SHISTORY_HOOK_LOG" ]]
SHISTORY_CAPTURE_DISABLED=1 _shistory_preexec 'echo disabled'
[[ ! -s "$SHISTORY_HOOK_LOG" ]]

# Start and finish failures are silent and preserve status.
SHISTORY_STUB_FAIL=start _shistory_preexec 'echo unavailable' 2>"$tmp/error"
[[ $? == 0 && ! -s "$tmp/error" && -z $SHISTORY_RECORD_ID ]]
SHISTORY_STUB_FAIL=finish
SHISTORY_RECORD_ID=synthetic-id
SHISTORY_STARTED_AT=$EPOCHREALTIME
set +e
(exit 23)
_shistory_precmd 2>"$tmp/error"
command_status=$?
set -e
[[ $command_status == 23 && ! -s "$tmp/error" ]]

print 'hook checks: 8 passed; 0 failed'
