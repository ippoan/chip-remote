# chip-remote

Claude desktop の `spawn_task` が出す chip (「ワークツリーで開始」ボタン) は desktop アプリにしか表示されない。
web / モバイルからは見えないので、Android の通知から起動・非表示できるようにする個人開発用の中継。

```
claude (mini-ryzen / Windows) ── PostToolUse hook ──▶ Worker (Hono) + Durable Object
                                                          │ FCM            ▲ WebSocket
                                                          ▼                │
                                                  Android 通知       Windows agent (UIA)
                                                  [開始] [非表示] ──▶   chip を探してボタンを Invoke
```

| ディレクトリ | 中身 |
|---|---|
| `src/`, `test/`, `wrangler.toml` | Cloudflare Worker (Hono) + `HubDO` |
| `hooks/` | PostToolUse hook (spawn_task / dismiss_task → Worker) |
| `agent/` | Windows agent (Rust / Tauri 2 トレイ常駐。UI Automation で chip を操作)。NSIS は Release `agent-latest` |
| `windows-agent/` | 旧 PowerShell agent (`agent/` の実機確認が済んだら削除) |
| `android/` | Android アプリ (FCM 受信 → 通知 → action POST) |
| `docs/PROTOCOL.md` | 4 者間の契約 (HTTP / WS / FCM / UIA) |
