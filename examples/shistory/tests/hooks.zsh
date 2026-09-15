#!/usr/bin/env zsh
set -eu

example_dir=${0:A:h:h}
tmp=${TMPDIR:-/tmp}/shistory-hooks-$$
mkdir "$tmp"
typeset -ga SHISTORY_TEST_PIDS=()
cleanup() {
  (( ${#SHISTORY_TEST_PIDS} )) && kill $SHISTORY_TEST_PIDS 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

print -r -- "#!${commands[zsh]}" > "$tmp/shistory"
cat >> "$tmp/shistory" <<'STUB'
local input=
if [[ $1 == start ]]; then
  IFS= read -r -d '' input || true
fi
print -r -- "$*|$input" >> "$SHISTORY_HOOK_LOG"
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

# A normal command records start metadata and finish metadata while preserving its status.
_shistory_preexec 'false synthetic-secret'
set +e
false
_shistory_precmd
command_status=$?
set -e
[[ $command_status == 1 ]]
grep -Eq '^start --session [^ ]+ --cwd .+ --started-at [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z\|false synthetic-secret$' "$SHISTORY_HOOK_LOG"
grep -q '^finish synthetic-id .*--exit-status 1|$' "$SHISTORY_HOOK_LOG"
started_at=$(sed -n 's/.*--started-at \([^|]*\)|.*/\1/p' "$SHISTORY_HOOK_LOG")
python3 - "$started_at" <<'PY'
from datetime import datetime
import sys

datetime.fromisoformat(sys.argv[1].replace("Z", "+00:00"))
PY

# Leading-space commands never reach the binary.
: > "$SHISTORY_HOOK_LOG"
_shistory_preexec ' hidden synthetic-secret'
[[ ! -s "$SHISTORY_HOOK_LOG" ]]

# Start and finish failures are silent and preserve status.
export SHISTORY_STUB_FAIL=start
_shistory_preexec 'echo unavailable' 2>"$tmp/error"
unset SHISTORY_STUB_FAIL
[[ ! -s "$tmp/error" ]] || { cat "$tmp/error"; false; }
[[ -z $SHISTORY_RECORD_ID ]]
grep -q 'echo unavailable$' "$SHISTORY_HOOK_LOG"
export SHISTORY_STUB_FAIL=finish
SHISTORY_RECORD_ID=synthetic-id
SHISTORY_STARTED_AT=$EPOCHREALTIME
set +e
(exit 23)
_shistory_precmd 2>"$tmp/error"
command_status=$?
set -e
unset SHISTORY_STUB_FAIL
[[ $command_status == 23 && ! -s "$tmp/error" ]]

# An accepting but unresponsive daemon cannot stall either hook indefinitely.
print -r -- '#!/usr/bin/env python3' > "$tmp/stalled-server.py"
cat >> "$tmp/stalled-server.py" <<'PY'
import socket
import sys

server = socket.socket(socket.AF_UNIX)
server.bind(sys.argv[1])
server.listen()
print("ready", flush=True)
while True:
    connection, _ = server.accept()
    print("accepted", flush=True)
PY
SHISTORY_BIN=${SHISTORY_REAL_BIN:?}
export EIDETICA_SOCKET="$tmp/stalled.sock"
python3 "$tmp/stalled-server.py" "$EIDETICA_SOCKET" > "$tmp/stalled.log" &
SHISTORY_TEST_PIDS+=($!)
while ! grep -q '^ready$' "$tmp/stalled.log" 2>/dev/null; do sleep 0.01; done

integer started_at_ms=$(( EPOCHREALTIME * 1000 ))
_shistory_preexec 'echo stalled'
integer start_elapsed_ms=$(( EPOCHREALTIME * 1000 - started_at_ms ))
[[ $start_elapsed_ms -ge 800 && $start_elapsed_ms -lt 2500 && -z $SHISTORY_RECORD_ID ]]

SHISTORY_RECORD_ID=uncertain-id
SHISTORY_STARTED_AT=$EPOCHREALTIME
started_at_ms=$(( EPOCHREALTIME * 1000 ))
_shistory_precmd
integer finish_elapsed_ms=$(( EPOCHREALTIME * 1000 - started_at_ms ))
[[ $finish_elapsed_ms -ge 800 && $finish_elapsed_ms -lt 2500 ]]
for _ in {1..100}; do
  [[ $(grep -c '^accepted$' "$tmp/stalled.log") -eq 2 ]] && break
  sleep 0.01
done
[[ $(grep -c '^accepted$' "$tmp/stalled.log") -eq 2 ]]

print 'hook checks: 13 passed; 0 failed'
