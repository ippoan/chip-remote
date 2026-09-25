# hooks — spawn_task / dismiss_task を Worker に報告する PostToolUse hook

`mcp__ccd_session__spawn_task` が成功すると chip を `POST /v1/chips` で、
`mcp__ccd_session__dismiss_task` が成功すると `DELETE /v1/chips/:task_id` で Worker に知らせる。
契約は [`docs/PROTOCOL.md`](../docs/PROTOCOL.md)。

hook は **spawn_task を呼んだ claude が動いているホスト** で発火する。
普段は SSH 先の mini-ryzen (Linux)、ローカルセッションなら Windows。

> **Rust の Windows agent (`agent/`) が動いているなら hook は不要。** agent が Claude desktop の
> セッションファイル (`%APPDATA%\Claude\claude-code-sessions`) を監視して、SSH 先のセッションも含め
> 同じ `POST` / `DELETE /v1/chips` を送る ([`docs/PROTOCOL.md`](../docs/PROTOCOL.md)「Claude desktop のセッションファイル」)。
> hook を残しても両方冪等なので害は無い (同じ chip が二重に報告されるだけ。先に届いた方の `host` / `cwd` が残る)。
> hook が要るのは agent を止めている (`watchSessions: false` を含む) ときや、Claude desktop 以外から spawn_task を使うとき。

| ファイル | 用途 |
|---|---|
| `post-spawn-task.sh` / `post-dismiss-task.sh` | Linux (bash + jq + curl) |
| `windows/post-spawn-task.ps1` / `windows/post-dismiss-task.ps1` | Windows (PowerShell 5.1) |
| `test/run.sh` | bash hook のテスト (偽 curl を PATH 先頭に置く。jq が無いと SKIP、CI では失敗扱い) |
| `test/run-windows.ps1` | ps1 hook のテスト (localhost の HttpListener で受ける) |

共通の振る舞い:

- **Claude を止めない**: 何が起きても exit 0、stdout には何も出さない、通信は 5 秒でタイムアウト。
- 失敗は log に追記するだけ (Linux: `~/.cache/chip-remote/hook.log`、Windows: `%LOCALAPPDATA%\chip-remote\hook.log`)。
- `task_id` は `tool_response` を文字列化して `task_[0-9a-f]+` で拾う (文字列 / `{content:[{type,text}]}` / content 配列のどれでも可)。
- `title` / `tldr` / `cwd` は `tool_input` から。`cwd` が無ければ hook 入力の `cwd` (セッションの cwd)。
- token はプロセス引数に出さない (bash は `curl --config -` で stdin から渡す)。
- PostToolUse は **成功した呼び出しでしか発火しない**。mini-ryzen の既存 PreToolUse hook
  (cwd ガード・スロット上限) で deny された spawn は報告されない — それで正しい (chip が出ていないので)。

## mini-ryzen (Linux) へのインストール

前提: `bash`, `jq`, `curl` (mini-ryzen には入っている)。

```sh
# 1. スクリプトを置く
mkdir -p ~/.claude/hooks/chip-remote
cp hooks/post-spawn-task.sh hooks/post-dismiss-task.sh ~/.claude/hooks/chip-remote/
chmod +x ~/.claude/hooks/chip-remote/*.sh

# 2. 接続設定 (shell の KEY=VALUE、hook が source する)
mkdir -p ~/.config/chip-remote
umask 077
cat > ~/.config/chip-remote/env <<'EOF'
CHIP_REMOTE_URL=https://chip-remote.ippoan.org
CHIP_REMOTE_TOKEN=<Worker の CHIP_REMOTE_TOKEN と同じ値>
# CHIP_REMOTE_HOST=mini-ryzen   # 省略時は hostname
EOF
chmod 600 ~/.config/chip-remote/env
```

`XDG_CONFIG_HOME` を設定しているならそちらの `chip-remote/env` が読まれる。

### 3. `~/.claude/settings.json` に PostToolUse を追加

**既存の `hooks` (PreToolUse の cwd ガード・スロット上限など) は残したまま**、
`hooks.PostToolUse` 配列に次の 2 エントリを足す (`PostToolUse` が既にあれば要素を追記):

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "mcp__ccd_session__spawn_task",
        "hooks": [
          {
            "type": "command",
            "command": "$HOME/.claude/hooks/chip-remote/post-spawn-task.sh",
            "async": true,
            "timeout": 10
          }
        ]
      },
      {
        "matcher": "mcp__ccd_session__dismiss_task",
        "hooks": [
          {
            "type": "command",
            "command": "$HOME/.claude/hooks/chip-remote/post-dismiss-task.sh",
            "async": true,
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

`async: true` なので Claude は hook の完了を待たない。設定は起動 cwd から読まれるので、
ユーザー設定 (`~/.claude/settings.json`) に置くのが確実。

### 動作確認

```sh
# テスト一式 (ネットワークは使わない)
bash hooks/test/run.sh

# 実 Worker に投げる手動確認 — 本当に chip が登録され phone に通知が飛ぶので注意
printf '%s' '{"session_id":"manual","cwd":"/tmp","tool_input":{"title":"hook 手動確認","tldr":"消してよい"},"tool_response":"Noted (task_id: task_deadbeef)."}' \
  | ~/.claude/hooks/chip-remote/post-spawn-task.sh
tail ~/.cache/chip-remote/hook.log    # 何も出なければ成功 (失敗時だけ記録する)
printf '%s' '{"tool_input":{"task_id":"task_deadbeef"}}' | ~/.claude/hooks/chip-remote/post-dismiss-task.sh
```

## Windows へのインストール (ローカルセッション用)

設定は agent と共有の `%APPDATA%\chip-remote\config.json` (`url`, `token`、任意で `host`)。
`windows-agent\install.ps1` を実行済みならもう存在する。無ければ `windows-agent\config.example.json` を
そこへコピーして `url` / `token` を埋める (UTF-8 のまま)。

`%USERPROFILE%\.claude\settings.json` の `hooks.PostToolUse` に追加
(パスはリポジトリの置き場所に合わせる。Claude Code は hook を Git Bash 経由で実行するので `/` 区切りで書く):

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "mcp__ccd_session__spawn_task",
        "hooks": [
          {
            "type": "command",
            "command": "powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:/Users/mtama/claude/chip-remote/hooks/windows/post-spawn-task.ps1",
            "async": true,
            "timeout": 10
          }
        ]
      },
      {
        "matcher": "mcp__ccd_session__dismiss_task",
        "hooks": [
          {
            "type": "command",
            "command": "powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:/Users/mtama/claude/chip-remote/hooks/windows/post-dismiss-task.ps1",
            "async": true,
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

- stdin はバイト列として UTF-8 で読む (`[Console]::In` は OEM コードページで読んで日本語が化けるため)。
- 送信 body も `UTF8.GetBytes` + `application/json; charset=utf-8`。
- `.ps1` は **ASCII のみ** (PS 5.1 は BOM なし UTF-8 を ANSI と誤読する)。CI で検査している。

テスト: `powershell -NoProfile -ExecutionPolicy Bypass -File hooks\test\run-windows.ps1`
