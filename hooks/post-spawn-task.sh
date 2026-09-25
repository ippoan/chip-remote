#!/usr/bin/env bash
# chip-remote: PostToolUse hook for mcp__ccd_session__spawn_task.
#
# Reads the hook JSON from stdin and reports the new chip to the Worker:
#   POST $CHIP_REMOTE_URL/v1/chips
#   {task_id, title, tldr, cwd, host, session_id}
#
# Must never block or disturb Claude: always exits 0, prints nothing on
# stdout, and appends every problem to ~/.cache/chip-remote/hook.log.
#
# Config: ${XDG_CONFIG_HOME:-$HOME/.config}/chip-remote/env (sourced)
#   CHIP_REMOTE_URL=https://chip-remote.ippoan.org
#   CHIP_REMOTE_TOKEN=...
#   CHIP_REMOTE_HOST=mini-ryzen   # optional, defaults to hostname

set -u

LOG_DIR="${HOME}/.cache/chip-remote"
LOG_FILE="${LOG_DIR}/hook.log"

log() {
  mkdir -p "$LOG_DIR" 2>/dev/null || return 0
  printf '%s post-spawn-task: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG_FILE" 2>/dev/null
  return 0
}

main() {
  local conf input task_id host url body code

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
  if ! printf '%s' "$input" | jq -e . >/dev/null 2>&1; then
    log "stdin is not valid JSON"
    return 0
  fi

  # tool_response shape varies (plain string, {content:[{type,text}]},
  # array of content blocks). Stringify whatever it is and grep the id.
  task_id="$(printf '%s' "$input" | jq -r '.tool_response | tostring' 2>/dev/null |
    grep -oE 'task_[0-9a-f]+' | head -n 1)"
  if [ -z "$task_id" ]; then
    log "task_id not found in tool_response: $(printf '%s' "$input" | jq -c '.tool_response' 2>/dev/null | head -c 300)"
    return 0
  fi

  host="${CHIP_REMOTE_HOST:-$(hostname 2>/dev/null || uname -n)}"

  body="$(printf '%s' "$input" | jq -c \
    --arg task_id "$task_id" \
    --arg host "$host" \
    '{
      task_id: $task_id,
      title: (.tool_input.title // ""),
      tldr: (.tool_input.tldr // ""),
      cwd: (.tool_input.cwd // .cwd // null),
      host: $host,
      session_id: (.session_id // null)
    }')"
  if [ -z "$body" ]; then
    log "failed to build body for $task_id"
    return 0
  fi

  url="${CHIP_REMOTE_URL%/}/v1/chips"
  # The token goes through --config on stdin so it never shows up in ps.
  code="$(printf 'header = "Authorization: Bearer %s"\n' "$CHIP_REMOTE_TOKEN" |
    curl -sS --max-time 5 --config - \
      -o /dev/null -w '%{http_code}' \
      -X POST \
      -H 'Content-Type: application/json; charset=utf-8' \
      --data-binary "$body" \
      "$url" 2>>"$LOG_FILE")"
  case "$code" in
    2??) ;;
    *) log "POST $url for $task_id failed (http_code=${code:-none})" ;;
  esac
  return 0
}

mkdir -p "$LOG_DIR" 2>/dev/null
exec 1>/dev/null
main 2>>"$LOG_FILE" || true
exit 0
