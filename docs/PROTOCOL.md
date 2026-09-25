# chip-remote プロトコル

`hooks/`・Worker (リポジトリ直下)・`windows-agent/`・`android/` の 4 者の契約。
どれかを変えるときはこのファイルを同じ PR で更新する。

## 登場人物

| 名前 | 動く場所 | 役割 |
|---|---|---|
| hook | spawn_task を呼ぶ claude が動くホスト (mini-ryzen / Windows) | PostToolUse で chip の生成・取り下げを Worker に報告 |
| Worker + `HubDO` | Cloudflare (`chip-remote.ippoan.org`) | chip の状態を保持、FCM 送信、agent WS の中継 |
| agent | Claude desktop が動く Windows (ログオン中の対話セッション) | UIA で chip を探し、ボタンを InvokePattern で押す |
| phone | Android | FCM 通知にボタン、押されたら Worker に action を POST |

DO は 1 個だけ (`idFromName("hub")`)。個人用なので利用者の概念は持たない。

## 認証

全エンドポイント (`/health` 以外) は `Authorization: Bearer <CHIP_REMOTE_TOKEN>`。
WebSocket も同じヘッダで upgrade する (PowerShell の `ClientWebSocket.Options.SetRequestHeader` で付けられる)。
比較は定数時間。token は Worker secret (`wrangler secret put CHIP_REMOTE_TOKEN`)。

## chip の状態

```
located_pending ──(agent: chip.located)──▶ notified ──(phone: action)──▶ acting ──▶ done / failed
      │                     │                                                │
      └──(agent: chip.not_found, 60s)──▶ notified (located=false)            │
any ──(hook: DELETE)──▶ withdrawn                                            │
```

`task_id` が主キー (spawn_task の戻り値 `task_xxxxxxxx`)。UI 上には task_id は出ないので、
agent は **title (+ tldr)** で chip を特定する。

## HTTP

### `POST /v1/chips` (hook → Worker)

```json
{ "task_id": "task_12d25b98", "title": "...", "tldr": "...", "cwd": "/home/claude/x",
  "host": "mini-ryzen", "session_id": "uuid-or-null" }
```

- 同じ task_id の再送は冪等 (上書きしない、200)。新規は 201。
- 新規なら agent WS に `chip.new` を送る。agent 未接続なら即 FCM 通知 (`located=false`)。

### `DELETE /v1/chips/:task_id` (hook → Worker、dismiss_task 時)

状態を `withdrawn` に。agent に `chip.withdrawn`、FCM に `chip_cancel`。

### `GET /v1/chips?open=1`

`withdrawn` / `done` 以外を新しい順に返す。`{ "chips": [Chip] }`。phone のアプリ画面用。

### `POST /v1/chips/:task_id/action` (phone → Worker)

```json
{ "action": "start" | "dismiss" }
```

- agent 未接続 → `409 {"error":"agent_offline"}`
- chip が `withdrawn`/`done` → `409 {"error":"chip_closed"}`
- それ以外 → 状態 `acting`、agent に `action` を送り `202 {"request_id": "..."}`。
- 結果は agent の `action.result` を受けて FCM `chip_result` で返す (HTTP は待たない)。
- agent が 30 秒応答しなければ `failed` (error=`agent_timeout`) にして `chip_result` を送る。

### `POST /v1/devices` (phone → Worker)

```json
{ "fcm_token": "...", "name": "Pixel 8" }
```

fcm_token で upsert。FCM が `UNREGISTERED` / `404` を返した token は削除する。

### `GET /v1/agent/ws` (agent → Worker、WebSocket)

hibernatable WebSocket。同時接続は 1 本 (新しい接続が来たら古い方を close 4000)。

### `GET /health`

`{ "ok": true, "agent_connected": bool }`。認証なし。

## WebSocket メッセージ (JSON text)

### Worker → agent

| type | 中身 | いつ |
|---|---|---|
| `hello` | `{ chips: Chip[] }` | 接続直後。未完了 chip の再同期 |
| `chip.new` | `{ chip: Chip }` | hook から新規 chip |
| `chip.withdrawn` | `{ task_id }` | hook から取り下げ |
| `action` | `{ request_id, task_id, action, title, tldr }` | phone が押した |
| `ping` | `{}` | 任意 (agent は `pong` を返す) |

### agent → Worker

| type | 中身 | 意味 |
|---|---|---|
| `chip.located` | `{ task_id, pane_title? }` | UIA で chip を見つけた → Worker が FCM 通知 (`located=true`) |
| `chip.not_found` | `{ task_id }` | 60 秒探して無かった → Worker が FCM 通知 (`located=false`) |
| `action.result` | `{ request_id, task_id, ok, error? }` | ボタンを押した結果 |
| `pong` | `{}` | |

## FCM (data-only、`android.priority=high`)

値はすべて文字列 (FCM data の制約)。

| `type` | 追加フィールド | phone の動作 |
|---|---|---|
| `chip` | `task_id, title, tldr, cwd, host, located` (`"true"`/`"false"`) | 通知を出す。ボタン「開始」「非表示」 |
| `chip_result` | `task_id, action, ok, error` | 通知を「開始しました」等に更新 (ok=false なら error 表示) |
| `chip_cancel` | `task_id` | 通知を消す |

通知 ID は `task_id.hashCode()`。

## Chip (JSON)

```json
{ "task_id": "...", "title": "...", "tldr": "...", "cwd": "...", "host": "...",
  "session_id": null, "status": "notified", "located": true,
  "created_at": 1790000000000, "updated_at": 1790000000000, "error": null }
```

## UIA での chip の見え方 (2026-09 時点 Claude desktop 2.7032)

実測。ラベル文字列は UI の言語で変わるので agent の設定ファイルに外出しする。

```
StatusBar (chip 全体)
  Text   "推奨タスク"
  Text   <title>
  Group
    Text <tldr>
  Button "提案を非表示"             ← dismiss
  Group  "ワークツリーで開始"
    Button "ワークツリーで開始"      ← start (InvokePattern 対応)
    Button "その他の開始オプション"
```

- 探索範囲は Claude のメインウィンドウ (`claude.exe` の `MainWindowHandle`) の Descendants。
- Chromium は UIA クライアントを検知してから遅延でアクセシビリティ木を構築するので、
  初回 `FindAll` は title bar の 3 ボタンしか返らない。1〜2 秒待って再取得する。
- chip は **そのセッションのチャットペインが表示されているときだけ** 木に出る。
  分割ビューに無いセッションの chip は見つからない (v1 は `chip.not_found` で通知だけ出す)。
