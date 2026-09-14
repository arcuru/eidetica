# Source this file from .zshrc after setting SHISTORY_HOST.
autoload -Uz add-zsh-hook
zmodload zsh/datetime

typeset -g SHISTORY_BIN=${SHISTORY_BIN:-shistory}
typeset -g SHISTORY_SESSION=${SHISTORY_SESSION:-"$$-${EPOCHREALTIME//./}"}
typeset -g SHISTORY_RECORD_ID=
typeset -gF SHISTORY_STARTED_AT=0

_shistory_preexec() {
  local command=$1
  SHISTORY_RECORD_ID=

  [[ -n ${SHISTORY_CAPTURE_DISABLED:-} || $command == ' '* ]] && return 0

  SHISTORY_STARTED_AT=$EPOCHREALTIME
  SHISTORY_RECORD_ID=$("$SHISTORY_BIN" start \
    --session "$SHISTORY_SESSION" \
    --cwd "$PWD" \
    -- "$command" 2>/dev/null) || SHISTORY_RECORD_ID=
  return 0
}

_shistory_precmd() {
  local command_status=$?
  local record_id=$SHISTORY_RECORD_ID
  local -F elapsed_ms=$(( (EPOCHREALTIME - SHISTORY_STARTED_AT) * 1000 ))
  SHISTORY_RECORD_ID=

  if [[ -n $record_id ]]; then
    "$SHISTORY_BIN" finish "$record_id" \
      --duration-ms "${elapsed_ms%.*}" \
      --exit-status "$command_status" >/dev/null 2>&1 || true
  fi
  return "$command_status"
}

add-zsh-hook preexec _shistory_preexec
add-zsh-hook precmd _shistory_precmd
