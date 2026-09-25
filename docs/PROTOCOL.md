# chip-remote プロトコル

`hooks/`・Worker (リポジトリ直下)・`windows-agent/`・`android/` の 4 者の契約。
どれかを変えるときはこのファイルを同じ PR で更新する。

## 登場人物

| 名前 | 動く場所 | 役割 |
|---|---|---|
| hook (任意) | spawn_task を呼ぶ claude が動くホスト (mini-ryzen / Windows) | PostToolUse で chip の生成・取り下げを Worker に報告。agent が動いていれば不要 (冗長な二重報告になるだけ) |
| Worker + `HubDO` | Cloudflare (`chip-remote.ippoan.org`) | chip の状態を保持、FCM 送信、agent WS の中継 |
| agent | Claude desktop が動く Windows (ログオン中の対話セッション) | Claude desktop のセッションファイルを監視して chip の生成・解消を Worker に報告 (hook と同じ HTTP API)。UIA で chip を探し、ボタンを InvokePattern で押す |
| phone | Android | FCM 通知にボタン、押されたら Worker に action を POST |

DO は 1 個だけ (`idFromName("hub")`)。個人用なので利用者の概念は持たない。

## 認証

全エンドポイント (`/health` 以外) は `Authorization: Bearer <CHIP_REMOTE_TOKEN>`。
WebSocket も同じヘッダで upgrade する (PowerShell の `ClientWebSocket.Options.SetRequestHeader` で付けられる)。
比較は定数時間 (前後の空白・改行は trim)。token は GCP `cloudsql-sv` の `CHIP_REMOTE_TOKEN` が SoT で、
Worker には CF Secrets Store binding で渡す (secrets-inventory MCP の `sync_from_gcp` で写す)。

## chip の状態

```
located_pending ──(agent: chip.located)──▶ notified ──(phone: action)──▶ acting ──▶ done / failed
      │                     │                                                │
      └──(agent: chip.not_found, 5s)───▶ notified (located=false)            │
any ──(hook / agent: DELETE)──▶ withdrawn                                            │
```

`task_id` が主キー (spawn_task の戻り値 `task_xxxxxxxx`)。UI 上には task_id は出ないので、
agent は **title (+ tldr)** で chip を特定する。

## HTTP

### `POST /v1/chips` (hook / agent → Worker)

```json
{ "task_id": "task_12d25b98", "title": "...", "tldr": "...", "cwd": "/home/claude/x",
  "host": "mini-ryzen", "session_id": "uuid-or-null" }
```

- 同じ task_id の再送は冪等 (上書きしない、200)。新規は 201。hook と agent の両方が同じ chip を送ってよい
  (先に届いた方の `host` / `cwd` が残る)。
- 新規なら agent WS に `chip.new` を送る。agent 未接続なら即 FCM 通知 (`located=false`)。

### `DELETE /v1/chips/:task_id` (hook: dismiss_task 時 / agent: セッションファイルから chip が消えた時)

状態を `withdrawn` に。agent に `chip.withdrawn`、FCM に `chip_cancel`。
未知の task_id は 404 (hook / agent は成功扱いにする)。

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

### レスポンスとエラーの細則

- `POST /v1/chips` (201/200) と `DELETE` (200) は `{ "chip": Chip }`。`POST /v1/devices` は `{ "ok": true }` (新規 201 / 更新 200)。
- エラーは `{ "error": code }`: 400 `invalid_request` / 401 `unauthorized` / 404 `not_found` (未知の task_id) /
  426 `expected_websocket` / 503 `token_not_configured` / 500 `internal`。action の判定順は 400 → 404 → `agent_offline` → `chip_closed`。
- 既に `withdrawn` / `done` の chip への DELETE は 200 で状態を変えず、agent・FCM へは送らない
  (`done` の chip を withdrawn にすると `chip_cancel` で phone の結果通知が消えるため)。
- `located=false` で通知済みの chip に後から `chip.located` が来たら、同じ通知 ID で `located=true` の `chip` を送り直す
  (`acting` 中などは located だけ更新して通知しない)。
- `request_id` が合わない `action.result` は古い結果として無視。
- `failed` は open 一覧と `hello` に含め、再度の action を受け付ける。成功した action は start / dismiss とも `done`。
- Worker 側にも locate の安全網 `LOCATE_TIMEOUT_MS` (既定 75 秒) がある。agent から報告が無ければ `located=false` で通知する。

## Claude desktop のセッションファイル (agent の chip 源)

agent は hook に頼らず、Claude desktop が Code セッションごとに書く JSON を 2 秒ごとに見て chip を知る
(2026-09 時点 Claude desktop 2.7032 で実測)。

- 場所: `%APPDATA%\Claude\claude-code-sessions\<id>\<id>\local_<uuid>.json` (1 セッション 1 ファイル)。
  同じ階層の `scheduled-tasks.json` / `backlog/tasks.json` などは対象外。
  SSH 先 (mini-ryzen) で動くセッションのファイルもこの PC にある (`cwd` が `/home/...`)。
- 頻繁に書き直される (`lastActivityAt` 等)。agent は (mtime, サイズ) が変わったファイルだけ読み直し、
  JSON として読めない (書き込み途中) ならそのラウンドは前回の内容のまま次で読み直す。

使うフィールド (他は無視):

| フィールド | 中身 |
|---|---|
| `title` | セッションのタイトル (サイドバーの表示と同じ文字列) |
| `cliSessionId` | Claude Code の `session_id` (hook が送るのと同じ値) → `POST /v1/chips` の `session_id` |
| `cwd` | セッションの cwd (Windows パス、SSH セッションは `/home/...`) |
| `isArchived` | true のセッションは chip が無いものとして扱う |
| `backgroundTaskSuggestions` | 未解決の chip の配列 `[{ id: "task_xxxxxxxx", title, tldr, prompt, createdAt: "<ms の文字列>" }]`。無いときはキーごと無い |
| `resolvedBackgroundTaskSuggestions` | 解決済み `{ "task_xxx": "started_notified" \| "dismissed" }` |

agent の振る舞い:

- 起動時は全ファイルを読み、未解決の chip を全部 `POST /v1/chips` する (Worker が 201 新規 / 200 既知で冪等)。
  既に `resolved…` にある過去の chip は送らない。
- 以後、ファイル単位の差分で
  - `backgroundTaskSuggestions` に新しく出た → `POST /v1/chips` (`{task_id, title, tldr, cwd, host, session_id}`、
    `host` は config の `host`、無ければ PC 名)
  - `backgroundTaskSuggestions` から消えた (`resolved…` に載った / 載らずに消えた / ファイル削除 / アーカイブ) →
    `DELETE /v1/chips/:task_id`。ただし **agent 自身がスマホの action で押した chip は DELETE しない**
    (Worker は既に `done`。DELETE すると `withdrawn` になり `chip_cancel` で結果通知が消えるため)
- `task_id` は `task_` + 英数字だけを扱う (URL パスに入るため)。
- 通信失敗・5xx・408・429・401/403 は 1 秒から倍々 (最大 60 秒) で再送。401/403 も待つのは config.json の token 修正を
  再起動なしで拾うため。その他の 4xx は諦めてログに残す。token はログに出さない。
- `action` を受けたとき、そのセッションファイルにある chip なら title / tldr はファイルの値 (UI と完全一致) を使い、
  セッションのタイトルも UIA に渡す (目的のセッションのペインを出してから探すため)。

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
| `chip.not_found` | `{ task_id }` | 画面を動かさずに探して無かった (既定 5 秒) → Worker が FCM 通知 (`located=false`) |
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
Group (チャットペインのコンテナ)
  StatusBar                          ← chip の本文
    Text   "推奨タスク"
    Text   <title>
    Group
      Text <tldr>
  Button "提案を非表示"               ← dismiss (StatusBar の「兄弟」)
  Group '' > Button "前の提案を表示"   ┐ 同じセッションに chip が複数あるときだけ出るページ送り。
  Text   "2件中1番目"                 │ 画面に出るのは現在の 1 件だけで、他の chip は
  Group '' > Button "次の提案を表示"   ┘ 「次」を押すまで木に存在しない
  Group  "ワークツリーで開始"          ← (同じく兄弟)
    Button "ワークツリーで開始"        ← start (InvokePattern 対応)
    Button "その他の開始オプション"
  Group  "チャットメッセージ"
```

ボタンは StatusBar の子ではなく、直後に並ぶ兄弟要素 (2026-09-25 の実機で確認)。
agent は StatusBar の後ろの兄弟を、次の StatusBar か「開始」以外の名前付き Group に当たるまでたどり、
その chip のボタンとして扱う。目的の chip が見つからなければ「次の提案を表示」でページを送って探す
(同じタイトルに戻ったら打ち切り、押した後は木の更新を最大 2 秒待つ)。

- 探索範囲は Claude のメインウィンドウ (`claude.exe` の `MainWindowHandle`) の Descendants。
- Chromium は UIA クライアントを検知してから遅延でアクセシビリティ木を構築するので、
  初回 `FindAll` は title bar の 3 ボタンしか返らない。1〜2 秒待って再取得する。
- chip は **そのセッションのチャットペインが表示されているときだけ** 木に出る。locate (読むだけ) では
  表示中でなければ `chip.not_found`。action では、agent がセッションファイルから chip の持ち主の
  セッション名を引き、表示中のペインに無ければ **サイドバーのそのセッションを Invoke して開いてから** 押す
  (ユーザーが見ていたペインは置き換わる。スマホから押したとき = 不在時だけなので許容)。
  - 表示中のペイン: ヘッダの Button `<title>、セッション名を変更`
  - サイドバーのセッション: Button `<状態 or PR 前置き> <title>` で、子がちょうど [StatusBar または Image, Group] の 2 つ
    (「実行中」「未読の返答」は StatusBar、「アイドル」や `#21 · マージ済み` は Image)。
    隣の `<title>のその他のオプション` や、チャット本文中の同名ボタン (子なし) は対象外
  - サイドバーが閉じている・折りたたみやスクロールで木に無い場合は `chip_not_found`
- **ウィンドウが他のウィンドウに完全に隠れている / 最小化 / ディスプレイ電源 OFF の間、Chromium は描画を止め、
  その間に出た chip は木に反映されない。** agent は探す・押す直前に
  (1) `ES_DISPLAY_REQUIRED` でディスプレイを起こし、(2) Claude のウィンドウを TOPMOST (非アクティブ) にし、
  (3) 1px 動かして戻す。**これはスマホから action が来たときだけ** (chip が出た時点ではユーザーが PC にいるので窓を動かさない) (Chromium は重なり順の変化だけでは再計算しない。位置変更イベントで ~0.3 秒で再描画)。
  終わったら TOPMOST を外して元の前面ウィンドウの後ろに戻す。
- ボタンを InvokePattern で押すと Chromium がフォーカスを移すため、**Claude のウィンドウがアクティブになる**。
- 画面ロック中は対象外 (描画されない)。agent は常駐中 `ES_SYSTEM_REQUIRED` でスリープだけ防ぐ
  (電源設定は変えない。ノート PC の蓋を閉じた場合は防げない)。
