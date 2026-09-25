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
| `hooks/` | PostToolUse hook (任意。agent がセッションファイルから chip を拾うので通常は不要) |
| `agent/` | Windows agent (Rust / Tauri 2 トレイ常駐。UI Automation で chip を操作)。NSIS は Release `agent-latest` |
| `android/` | Android アプリ (FCM 受信 → 通知 → action POST) |
| `docs/PROTOCOL.md` | 4 者間の契約 (HTTP / WS / FCM / UIA) |

## インストール

- **Windows agent**: Release [`agent-latest`](https://github.com/ippoan/chip-remote/releases/tag/agent-latest) の
  `chip-remote-agent_x64-setup.exe` (ユーザー単位、管理者不要)。初回起動で「ログオン時に起動」が有効になる。
  設定は `%APPDATA%\chip-remote\config.json` (`url` / `token`)。詳細は [`agent/README.md`](agent/README.md)
- **Android**: スマホのブラウザで <https://ippoan.github.io/chip-remote/chip-remote.apk> を開いてインストール
  (Release の asset はリダイレクト先の署名付き URL で DL が止まることがあるので GitHub Pages から配る)。
  アプリで token を入れて「端末を登録」
- token は GCP `cloudsql-sv` の Secret Manager `CHIP_REMOTE_TOKEN`
