# agent — chip-remote Windows agent (Rust / Tauri 2 トレイアプリ)

Claude desktop が動く Windows のログオンセッションに常駐し、Worker と WebSocket
(`/v1/agent/ws`) でつながって、スマホから押された spawn_task chip のボタン
(「ワークツリーで開始」/「提案を非表示」) を UI Automation で押す。
PowerShell 版 (`windows-agent/`) の置き換えで、プロトコル上の振る舞いは同じ
(契約は [`docs/PROTOCOL.md`](../docs/PROTOCOL.md))。

- `hello` / `chip.new` で届いた `located_pending` の chip を、**画面を動かさずに**
  `scanIntervalSec` ごとに探す → 見つかれば `chip.located`、`locateTimeoutSec` 経っても無ければ `chip.not_found`
- `chip.withdrawn` / `action` が来た chip は探索キューから外す
- `action` → Claude のウィンドウを一時的に前に出して chip を探し (ページ送りも)、ボタンを押して `action.result`
  (失敗コード `claude_not_running` / `chip_not_found` / `button_not_found` / `invoke_failed`)
- `ping` → `pong`
- 切断時は 1 秒から倍々で最大 60 秒待って再接続 (`hello` を受けたセッションの後は 1 秒に戻す)。
  close 4000 (別の agent が接続を奪った) を受けたら `takeoverBackoffSec` 待つ

ウィンドウは持たない (タスクトレイのアイコンだけ)。

## インストール

1. PowerShell 版が入っているなら先に外す (両方動くと close 4000 で接続を奪い合う):
   `powershell -NoProfile -ExecutionPolicy Bypass -File windows-agent\uninstall.ps1`
   (設定 `%APPDATA%\chip-remote\config.json` とログはそのまま引き継げる)
2. GitHub Release [`agent-latest`](https://github.com/ippoan/chip-remote/releases/tag/agent-latest) から
   `chip-remote-agent_x64-setup.exe` を落として実行する。ユーザー単位のインストールで管理者権限は不要。
   コード署名 (Authenticode) はしていないので SmartScreen の警告が出たら「詳細情報 → 実行」。
3. 起動するとトレイに常駐する。初回起動時に「ログオン時に起動」が自動で有効になる
   (以後トレイのチェックで切り替え、外した設定は保たれる)。
4. `config.json` が無い / `token` が空なら状態が「設定がありません」になる。
   トレイの「設定ファイルを開く」で雛形 (token 空) が作られて既定のアプリで開くので、
   `token` を埋めて保存する。30 秒以内に読み直して接続する (再起動不要)。

アンインストールは Windows の「インストールされているアプリ」から。設定とログは残る。

## config.json

`%APPDATA%\chip-remote\config.json` (UTF-8、BOM 可)。PowerShell 版・Windows hook と同じファイル・同じキーで、
新しいキーは無い。接続を張り直すたびに読み直す (値を変えたら次の再接続から有効)。

| キー | 既定 | 意味 |
|---|---|---|
| `url` | (必須) | Worker の origin。`https→wss` / `http→ws` に変えて `/v1/agent/ws` へ。`wrangler dev` なら `http://localhost:8787` |
| `token` | (必須) | `CHIP_REMOTE_TOKEN`。upgrade 時に `Authorization: Bearer` で送る。ログには出さない |
| `labels.marker` / `start` / `dismiss` / `next` | `推奨タスク` / `ワークツリーで開始` / `提案を非表示` / `次の提案を表示` | Claude desktop の UI ラベル (UI 言語に合わせる) |
| `locateTimeoutSec` | 5 | 画面を動かさない探索で `chip.not_found` を返すまでの秒数 |
| `scanIntervalSec` | 2 | 探索の間隔 (最小 1) |
| `takeoverBackoffSec` | 300 | close 4000 を受けたときの待ち |
| `raiseWaitSec` | 5 | action 時にウィンドウを前に出してから Chromium の再描画を待つ上限 |
| `preventSleep` | true | 常駐中はシステムのスリープを止める (`ES_SYSTEM_REQUIRED`、電源設定は変えない)。false→true は次の再接続で効くが、true→false は agent の再起動が必要 |

## トレイメニュー

| 項目 | 動作 |
|---|---|
| 状態: … | 「接続中」「切断中 (再接続待ち)」「設定がありません」「別の agent に交代しました」(押せない) |
| バージョン … | インストールされている版 (CI build は `0.0.<run番号>`) |
| 設定ファイルを開く | `config.json` を既定のアプリで開く (無ければ雛形を作る。開けなければメモ帳) |
| ログをコピー | 直近 2000 行のログをクリップボードへ |
| ログフォルダを開く | `%LOCALAPPDATA%\chip-remote` をエクスプローラーで開く |
| ログオン時に起動 | 自動起動 (HKCU の Run) の切り替え |
| 終了 | agent を止める |

二重起動はしない (2 つ目は何もせず終わる)。

## ログ

`%LOCALAPPDATA%\chip-remote\agent.log` (UTF-8)。1 MB を超えると `agent.log.1` に回す (1 世代)。
WS の送受信は 1 行ずつ残す (長い文字列フィールドは 80 文字、1 行は 1000 文字で切る)。token は出さない。
詳しく見たいときは環境変数 `CHIP_REMOTE_LOG=debug` (tracing の EnvFilter 書式) で起動する。

## 自動更新

起動時と 1 時間ごとに
`https://github.com/ippoan/chip-remote/releases/download/agent-latest/latest.json` を見て、
新しい版があれば NSIS を passive モードで入れて再起動する (失敗してもログに残すだけ)。

- CI (`.github/workflows/agent.yml`) は main への push ごとに `agent-latest` prerelease を作り直す。
  版は `0.0.<run番号>`
- 署名鍵 (org secret `CHIP_REMOTE_TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD`) が無い間は
  `.sig` と `latest.json` を出さない = 自動更新しない。インストーラーは出る
- 有効にするには `tauri signer generate` で鍵を作り、公開鍵を `src-tauri/tauri.conf.json` の
  `plugins.updater.pubkey` (今は `REPLACE_WITH_TAURI_SIGNER_PUBKEY`) に入れ、秘密鍵とパスワードを上の secret に入れる
  (手順は AlcAppTauri の `plan/07-auto-update.md` と同じ)
- 手元で作った `0.1.0` は CI 版 (`0.0.N`) より新しい扱いになり、更新されない

## 制約

- **画面ロック中は動かない** (Chromium が描画せず UIA の木に chip が出ない)。スリープは止めるがロックは防がない
- ノート PC の蓋を閉じたときのスリープは防げない (電源設定に従う)
- chip が見えるのは **そのセッションのチャットペインが表示されているときだけ**。分割ビューに無いセッションの chip は
  `chip.not_found` (通知は `located=false` で出る)
- chip が出た時点ではウィンドウを動かさない (ユーザーが PC の前にいるため)。覆われていれば見つからず `not_found` になる
- **スマホでボタンを押したときだけ** Claude のウィンドウを一時的に最前面 (非アクティブ) にして探し、押す。
  押すと Chromium がフォーカスを移すので Claude のウィンドウがアクティブになる

## 構成

```
agent/
  Cargo.toml              workspace (chip-core / chip-uia / src-tauri)
  crates/chip-core        config.json・WS メッセージ型・chip の照合 (OS 非依存)
  crates/chip-uia         UI Automation (Claude desktop の chip を探す・押す)
  src-tauri/src/
    lib.rs                トレイ・プラグイン (single-instance / autostart / opener / updater)
    agent.rs              再接続ループ・WS セッション・探索キュー・action (Tauri 非依存でテスト可能)
    driver.rs             ChipDriver trait + 専用スレッド (UIA の COM は Send でないので 1 スレッドに閉じる)
    uia.rs                ChipDriver の実装 = chip_uia::Uia
    logging.rs / paths.rs / status.rs
  dist/index.html         Tauri が要求する frontendDist (ウィンドウは開かない)
```

## 開発

```powershell
cd agent
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # WS はプロセス内サーバー、UIA は fake。画面は触らない
npm install --no-package-lock
# 署名鍵なしで NSIS を作る (updater の成果物だけ省く)
npx tauri build --config '{\"bundle\":{\"createUpdaterArtifacts\":false}}'
# → target\release\bundle\nsis\chip-remote-agent_0.1.0_x64-setup.exe
```

テストは実機の Claude desktop・本番 Worker に触れない (agent のロジックは `agent.rs` の純粋関数と、
`tokio-tungstenite` のプロセス内サーバー + fake driver で検証する)。
実機での確認は `wrangler dev` の Worker に向けた `config.json` で `npx tauri dev` する。
