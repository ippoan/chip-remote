#!/usr/bin/env bash
# Tests for hooks/post-spawn-task.sh and hooks/post-dismiss-task.sh.
#
# A fake `curl` is put first on PATH; it records its argv / stdin and
# prints a canned http_code. No network is used.
#
# jq is required (both by the hooks and by the assertions). When it is
# missing the tests are skipped locally but fail in CI (CI=true).

set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOKS="$(cd "$HERE/.." && pwd)"
FIX="$HERE/fixtures"

if ! command -v jq >/dev/null 2>&1; then
  if [ "${CI:-}" = "true" ]; then
    echo "FAIL: jq is required" >&2
    exit 1
  fi
  echo "SKIP: jq not found (install jq to run the hook tests; CI runs them)"
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin" "$WORK/home" "$WORK/config/chip-remote"
cat >"$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
: >"$FAKE_CURL_ARGS"
for a in "$@"; do printf '%s\n' "$a" >>"$FAKE_CURL_ARGS"; done
cat >"$FAKE_CURL_STDIN"
if [ -n "${FAKE_CURL_STDERR:-}" ]; then printf '%s\n' "$FAKE_CURL_STDERR" >&2; fi
printf '%s' "${FAKE_CURL_CODE:-201}"
exit "${FAKE_CURL_EXIT:-0}"
EOF
chmod +x "$WORK/bin/curl"

export PATH="$WORK/bin:$PATH"
export HOME="$WORK/home"
export XDG_CONFIG_HOME="$WORK/config"
export FAKE_CURL_ARGS="$WORK/curl.args"
export FAKE_CURL_STDIN="$WORK/curl.stdin"
unset CHIP_REMOTE_URL CHIP_REMOTE_TOKEN CHIP_REMOTE_HOST CHIP_REMOTE_CONFIG
LOG="$HOME/.cache/chip-remote/hook.log"

write_config() {
  cat >"$XDG_CONFIG_HOME/chip-remote/env" <<'EOF'
CHIP_REMOTE_URL=https://chip-remote.example.test/
CHIP_REMOTE_TOKEN=tok_secret_123
CHIP_REMOTE_HOST=test-host
EOF
}

FAILS=0
PASSES=0
CURRENT=""

ok() { PASSES=$((PASSES + 1)); }
ng() {
  FAILS=$((FAILS + 1))
  echo "  FAIL [$CURRENT]: $*" >&2
}
assert_eq() { # expected actual message
  if [ "$1" = "$2" ]; then ok; else ng "$3: expected '$1', got '$2'"; fi
}
assert_file_contains() { # file needle message
  if [ -f "$1" ] && grep -qF -- "$2" "$1"; then ok; else ng "$3: '$2' not in $1"; fi
}

# arg_after NAME -> the argv element following NAME in the last curl call
arg_after() {
  awk -v k="$1" 'found { print; exit } $0 == k { found = 1 }' "$FAKE_CURL_ARGS"
}
last_arg() { tail -n 1 "$FAKE_CURL_ARGS"; }

# run_hook SCRIPT FIXTURE -> sets RC, OUT
run_hook() {
  rm -f "$FAKE_CURL_ARGS" "$FAKE_CURL_STDIN" "$LOG"
  OUT="$(bash "$HOOKS/$1" <"$FIX/$2")"
  RC=$?
}

expect_clean_exit() {
  assert_eq 0 "$RC" "exit code"
  assert_eq "" "$OUT" "stdout must be empty"
}

check_spawn() { # fixture task_id title cwd session_id(json)
  CURRENT="spawn $1"
  write_config
  run_hook post-spawn-task.sh "$1"
  expect_clean_exit
  if [ ! -f "$FAKE_CURL_ARGS" ]; then
    ng "curl was not called (log: $(cat "$LOG" 2>/dev/null))"
    return
  fi
  assert_eq POST "$(arg_after -X)" "method"
  assert_eq "https://chip-remote.example.test/v1/chips" "$(last_arg)" "url"
  assert_eq "5" "$(arg_after --max-time)" "timeout"
  assert_eq "Content-Type: application/json; charset=utf-8" "$(arg_after -H)" "content-type"
  assert_file_contains "$FAKE_CURL_STDIN" 'header = "Authorization: Bearer tok_secret_123"' "auth header via --config"
  if grep -qF tok_secret_123 "$FAKE_CURL_ARGS"; then ng "token leaked into argv"; else ok; fi

  local body
  body="$(arg_after --data-binary)"
  assert_eq "$2" "$(printf '%s' "$body" | jq -r .task_id)" "body.task_id"
  assert_eq "$3" "$(printf '%s' "$body" | jq -r .title)" "body.title"
  assert_eq "$4" "$(printf '%s' "$body" | jq -r .cwd)" "body.cwd"
  assert_eq "test-host" "$(printf '%s' "$body" | jq -r .host)" "body.host"
  assert_eq "$5" "$(printf '%s' "$body" | jq -c .session_id)" "body.session_id"
  assert_eq "$(jq -r .tool_input.tldr "$FIX/$1")" "$(printf '%s' "$body" | jq -r .tldr)" "body.tldr"
  assert_eq "task_id,title,tldr,cwd,host,session_id" "$(printf '%s' "$body" | jq -r 'keys_unsorted | join(",")')" "body keys"
  if [ -s "$LOG" ]; then ng "unexpected log: $(cat "$LOG")"; else ok; fi
}

echo "== post-spawn-task.sh"
check_spawn spawn-string.json task_12d25b98 "README の古いバッジを直す" \
  /home/claude/work/chip-remote '"3f1c2b9e-0d7a-4e51-9a60-1b2c3d4e5f60"'
check_spawn spawn-content.json task_0a1b2c3d "未使用の設定項目を削除" \
  /home/claude/work/other-repo '"3f1c2b9e-0d7a-4e51-9a60-1b2c3d4e5f60"'
check_spawn spawn-array.json task_ffee0011 "テストを追加" \
  /home/claude/work/chip-remote 'null'

CURRENT="spawn without task_id"
write_config
run_hook post-spawn-task.sh spawn-no-id.json
expect_clean_exit
if [ -f "$FAKE_CURL_ARGS" ]; then ng "curl must not be called"; else ok; fi
assert_file_contains "$LOG" "task_id not found" "log entry"

CURRENT="spawn without config"
rm -f "$XDG_CONFIG_HOME/chip-remote/env"
run_hook post-spawn-task.sh spawn-string.json
expect_clean_exit
if [ -f "$FAKE_CURL_ARGS" ]; then ng "curl must not be called"; else ok; fi
assert_file_contains "$LOG" "not set" "log entry"

CURRENT="spawn with curl timeout"
write_config
export FAKE_CURL_EXIT=28 FAKE_CURL_CODE=000 FAKE_CURL_STDERR="curl: (28) Operation timed out"
run_hook post-spawn-task.sh spawn-string.json
unset FAKE_CURL_EXIT FAKE_CURL_CODE FAKE_CURL_STDERR
expect_clean_exit
assert_file_contains "$LOG" "Operation timed out" "curl stderr logged"
assert_file_contains "$LOG" "http_code=000" "failure logged"

CURRENT="spawn with http 401"
export FAKE_CURL_CODE=401
run_hook post-spawn-task.sh spawn-string.json
unset FAKE_CURL_CODE
expect_clean_exit
assert_file_contains "$LOG" "http_code=401" "failure logged"

CURRENT="spawn with idempotent 200"
export FAKE_CURL_CODE=200
run_hook post-spawn-task.sh spawn-string.json
unset FAKE_CURL_CODE
expect_clean_exit
if [ -s "$LOG" ]; then ng "200 must not be logged as failure"; else ok; fi

echo "== post-dismiss-task.sh"
CURRENT="dismiss"
write_config
export FAKE_CURL_CODE=204
run_hook post-dismiss-task.sh dismiss.json
unset FAKE_CURL_CODE
expect_clean_exit
if [ -f "$FAKE_CURL_ARGS" ]; then
  assert_eq DELETE "$(arg_after -X)" "method"
  assert_eq "https://chip-remote.example.test/v1/chips/task_12d25b98" "$(last_arg)" "url"
  assert_file_contains "$FAKE_CURL_STDIN" 'Authorization: Bearer tok_secret_123' "auth header"
  if [ -s "$LOG" ]; then ng "unexpected log: $(cat "$LOG")"; else ok; fi
else
  ng "curl was not called (log: $(cat "$LOG" 2>/dev/null))"
fi

CURRENT="dismiss with malformed task_id"
run_hook post-dismiss-task.sh dismiss-bad-id.json
expect_clean_exit
if [ -f "$FAKE_CURL_ARGS" ]; then ng "curl must not be called"; else ok; fi
assert_file_contains "$LOG" "invalid or missing" "log entry"

CURRENT="dismiss with http 404"
export FAKE_CURL_CODE=404
run_hook post-dismiss-task.sh dismiss.json
unset FAKE_CURL_CODE
expect_clean_exit
assert_file_contains "$LOG" "http_code=404" "failure logged"

echo
echo "passed: $PASSES, failed: $FAILS"
[ "$FAILS" -eq 0 ]
