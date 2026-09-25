# windows-agent — Claude desktop の chip を UI Automation で押す常駐 agent

Claude desktop が動く Windows のログオンセッション内で常駐し、Worker と WebSocket
(`/v1/agent/ws`) でつながる。

- hook から chip が届く (`hello` / `chip.new`) → UIA で chip を探して `chip.located`、
  `locateTimeoutSec` (既定 60 秒) 見つからなければ `chip.not_found`
- phone がボタンを押す (`action`) → chip を探して「ワークツリーで開始」/「提案を非表示」を
  InvokePattern で押し、`action.result` を返す
  (失敗コード: `claude_not_running` / `chip_not_found` / `button_not_found` / `invoke_failed`)
- `ping` には `pong`

契約は [`docs/PROTOCOL.md`](../docs/PROTOCOL.md)。

| ファイル | 中身 |
|---|---|
| `ChipUia.psm1` | UIA 操作 (`Get-ClaudeWindow`, `Get-ChipList`, `Find-Chip`, `Invoke-ChipAction`, `Get-ChipLabels`) |
| `chip-remote-agent.ps1` | 常駐ループ + デバッグ用 `-Probe` / `-Invoke` |
| `install.ps1` / `uninstall.ps1` | タスクスケジューラ登録 / 削除 |
| `config.example.json` | 設定の雛形 (UTF-8、日本語ラベル入り) |
| `test/Test-PowerShellFiles.ps1` | ASCII・構文・Import・PSScriptAnalyzer の静的検査 (CI と同じ) |

## セットアップ

```powershell
cd C:\Users\mtama\claude\chip-remote\windows-agent
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
notepad $env:APPDATA\chip-remote\config.json      # url と token を埋める
Start-ScheduledTask -TaskName chip-remote-agent    # または install.ps1 -Start
Get-Content $env:LOCALAPPDATA\chip-remote\agent.log -Tail 20 -Wait -Encoding UTF8
```

`install.ps1` がすること:

- `%APPDATA%\chip-remote\config.json` が無ければ `config.example.json` をバイトコピーで作る
  (Windows hook と共有)
- タスク `chip-remote-agent` を登録: 現ユーザーのログオン時に起動、`Interactive` (UIA は
  ユーザーのデスクトップセッション内でしか動かない)、`RunLevel Limited`、実行時間無制限、
  失敗時 1 分おきに 3 回再起動。管理者権限は不要
- 実行コマンドは `powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File <path>\chip-remote-agent.ps1`

アンインストール: `.\uninstall.ps1` (設定とログは残す)、`.\uninstall.ps1 -Purge` で
`%APPDATA%\chip-remote` と `%LOCALAPPDATA%\chip-remote` も削除。

### config.json

```json
{
  "url": "https://chip-remote.ippoan.org",
  "token": "<CHIP_REMOTE_TOKEN>",
  "labels": { "marker": "推奨タスク", "start": "ワークツリーで開始", "dismiss": "提案を非表示" },
  "locateTimeoutSec": 60,
  "scanIntervalSec": 2,
  "takeoverBackoffSec": 300
}
```

- `url` は `https→wss` / `http→ws` に変換して `/v1/agent/ws` へ。ローカルの `wrangler dev` なら
  `"url": "http://localhost:8787"` でよい。
- `labels` は Claude desktop の UI 言語に合わせる。省略時は上の日本語が既定値。
  **ラベルは config (UTF-8) にだけ書く** — `.ps1` / `.psm1` は ASCII 限定 (下記)。
- `takeoverBackoffSec`: Worker から close 4000 (別の agent が接続を奪った) を受けたときの待ち時間。
  通常の切断は 1 秒から倍々で最大 60 秒。

## 手動確認 (ネットワーク不要)

```powershell
# 今 UIA 木に見えている chip を列挙 (読むだけ)
powershell -NoProfile -ExecutionPolicy Bypass -File .\chip-remote-agent.ps1 -Probe

# 1 つの chip のボタンを押す (title 完全一致、同名が複数あるときは -Tldr で絞る)
powershell -NoProfile -ExecutionPolicy Bypass -File .\chip-remote-agent.ps1 -Invoke dismiss -Title "README の古いバッジを直す"
powershell -NoProfile -ExecutionPolicy Bypass -File .\chip-remote-agent.ps1 -Invoke start -Title "..." -Tldr "..."
```

`-Probe` の出力例:

```
window: "Claude" hwnd=393838
chips: 1

[1] title:   README の古いバッジを直す
    tldr:    CI バッジのリンク切れを見つけた。新しいセッションで差し替える。
    buttons: 提案を非表示 | ワークツリーで開始 | その他の開始オプション
    offscreen=False rect=812,640 560x96
```

`-Invoke` の終了コードは成功 0 / 失敗 1 (`failed: chip_not_found (...)` のように理由を表示)。

## 制約・注意

- **Claude のウィンドウが隠れていても押せる** (一瞬最前面にして 1px 動かし、Chromium に再描画させる)。
  押すとボタンにフォーカスが移るので Claude のウィンドウがアクティブになる。
- **同じセッションに chip が複数ある**と画面には 1 件ずつしか出ないので、「次の提案を表示」でページを送って探す。
- **スリープしない**: 常駐中は `SetThreadExecutionState(ES_SYSTEM_REQUIRED)` でスリープを防ぐ
  (`preventSleep: false` で無効化)。電源設定は変えない。ノート PC の蓋を閉じたときは防げない。
- **画面ロック中は押せない** (Chromium が描画しない)。


- **chip は、そのセッションのチャットペインが表示されているときだけ UIA 木に出る。**
  分割ビューに無いセッションの chip は見つからない → `chip.not_found` になり、phone には
  `located=false` の通知だけが出る (その状態で押しても `chip_not_found`)。
- Chromium はアクセシビリティ木を遅延構築する。しばらく UIA で触っていないウィンドウは
  初回の検索でタイトルバーのボタンしか返さないので、chip が 0 件なら 1.5 秒待って再検索する
  (30 秒以内に検索済みのウィンドウは待たない)。
- chip の特定は title の完全一致 (次に空白正規化一致)、複数あれば tldr で絞る。task_id は UI に出ない。
- 1 デスクトップセッションに agent は 1 つ (名前付き mutex `Local\chip-remote-agent`)。
  2 つ目は即終了する。
- `-WindowStyle Hidden` でもログオン時に PowerShell の窓が一瞬見えることがある。
- token は `%APPDATA%\chip-remote\config.json` に平文で置く (ユーザープロファイルの ACL で保護)。
- ログ: `%LOCALAPPDATA%\chip-remote\agent.log` (1 MB で `agent.log.1` にローテート)。
  送受信した WS メッセージ (chip の title を含む) も記録する。
- `.ps1` / `.psm1` は **ASCII のみ**。Windows PowerShell 5.1 は BOM なし UTF-8 を ANSI として読むため、
  日本語リテラルを入れると壊れる。既定ラベルも `ChipUia.psm1` ではコードポイント配列で持っている。
  `test\Test-PowerShellFiles.ps1` (CI でも実行) が検査する。
