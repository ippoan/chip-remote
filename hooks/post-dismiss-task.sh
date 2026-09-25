#!/usr/bin/env bash
# chip-remote: PostToolUse hook for mcp__ccd_session__dismiss_task.
#
# Reads the hook JSON from stdin and withdraws the chip on the Worker:
#   DELETE $CHIP_REMOTE_URL/v1/chips/<tool_input.task_id>
#
# Must never block or disturb Claude: always exits 0, prints nothing on
# stdout, and appends every problem to ~/.cache/chip-remote/hook.log.
#
# Config: ${XDG_CONFIG_HOME:-$HOME/.config}/chip-remote/env (sourced)

set -u

LOG_DIR="${HOME}/.cache/chip-remote"
LOG_FILE="${LOG_DIR}/hook.log"

log() {
  mkdir -p "$LOG_DIR" 2>/dev/null || return 0
  printf '%s post-dismiss-task: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG_FILE" 2>/dev/null
  return 0
}

main() {
  local conf input task_id url code

  conf="${CHIP_REMOTE_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/chip-remote/env}"
  if [ -f "$conf" ]; then
    # shellcheck source=/dev/null
    . "$conf"
  fi
  if [ -z "${CHIP_REMOTE_URL:-}" ] || [ -z "${CHIP_REMOTE_TOKEN:-}" ]; then
    log "CHIP_REMOTE_URL / CHIP_REMOTE_TOKEN not set (config: $conf)"
    return 0
  fi
  if ! command -v jq >/dev/null 2>&1; then
    log "jq not found"
    return 0
  fi
  if ! command -v curl >/dev/null 2>&1; then
    log "curl not found"
    return 0
  fi

  input="$(cat)"
  task_id="$(printf '%s' "$input" | jq -r '.tool_input.task_id // empty' 2>/dev/null)"
  # Only a well-formed id may end up in the URL path.
  if ! printf '%s' "$task_id" | grep -qE '^task_[0-9a-f]+$'; then
    log "invalid or missing tool_input.task_id: '$(printf '%s' "$task_id" | head -c 100)'"
    return 0
  fi

  url="${CHIP_REMOTE_URL%/}/v1/chips/${task_id}"
  code="$(printf 'header = "Authorization: Bearer %s"\n' "$CHIP_REMOTE_TOKEN" |
    curl -sS --max-time 5 --config - \
      -o /dev/null -w '%{http_code}' \
      -X DELETE \
      "$url" 2>>"$LOG_FILE")"
  case "$code" in
    2??) ;;
    *) log "DELETE $url failed (http_code=${code:-none})" ;;
  esac
  return 0
}

mkdir -p "$LOG_DIR" 2>/dev/null
exec 1>/dev/null
main 2>>"$LOG_FILE" || true
exit 0
