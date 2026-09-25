# CLAUDE.md

Claude desktop の `spawn_task` chip を Android 通知から起動・非表示する個人用の中継
(hook + Cloudflare Worker/DO + Windows agent + Android)。

このリポジトリで Claude Code セッションを動かす時の作業ガイド。共通項は
[ippoan/claude-md](https://github.com/ippoan/claude-md) の `CLAUDE.md.template` に従う。

## まず読むもの

- [`docs/PROTOCOL.md`](./docs/PROTOCOL.md) — 4 者 (hook / Worker / agent / phone) の契約。
  HTTP・WS・FCM・chip の状態・UIA の見え方はすべてここが正。
- [`README.md`](./README.md) — 全体像

## ディレクトリ

| ディレクトリ | 中身 |
|---|---|
| `src/`, `test/`, `wrangler.toml` | Cloudflare Worker (Hono) + `HubDO` (`idFromName("hub")` の 1 個) |
| `hooks/` | PostToolUse hook (spawn_task / dismiss_task → Worker) |
| `windows-agent/` | PowerShell 常駐 agent (UI Automation で chip を操作) |
| `android/` | Android アプリ (FCM 受信 → 通知 → action POST) |

**契約 (エンドポイント・メッセージ・FCM payload・状態・エラーコード) を変えるときは
`docs/PROTOCOL.md` を同じ PR で更新する。** 片側だけ変えると他の 3 者が壊れる。

## secret

Worker secret (`wrangler secret put <NAME>`、`[secrets] required` は使わない — wrangler.toml 参照):

- `CHIP_REMOTE_TOKEN` — hook / agent / phone 共通の Bearer token。未設定なら 503 (fail-closed)
- `FCM_PRIVATE_KEY` — FCM 送信用 service account の private_key (PKCS8 PEM)
- `FCM_CLIENT_EMAIL` — 同 service account の client_email

FCM の 2 つが未設定なら送信は log してスキップする (agent / hook の経路は動く)。
**secret を会話 / log / tool param に出さない。**

## ビルド / テスト (Worker)

PR を出す前に手元で green に:

```sh
npm install
npm run typecheck
npm test            # vitest + @cloudflare/vitest-pool-workers (workerd 上)
npx wrangler deploy --dry-run --outdir <tmp>   # wrangler.toml の検証
```

テストは FCM / OAuth への外向き fetch を `test/setup.ts` のスタブで握りつぶし、
service account 鍵は `vitest.config.ts` が起動時に使い捨て生成する (リポジトリに鍵を置かない)。

CI (`.github/workflows/ci.yml`) は `main` への PR ごとに ci-workflows の `frontend-ci.yml`
(project_type: worker) で同じことを回し、merge で `wrangler deploy` する。

## GitHub 自動化 (重要)

- **`main` に直 push しない。** PR を作る。
- PR / commit は `Refs #N` を使う (`Closes/Fixes/Resolves` は禁止 — auto-close 防止)。
- auto-merge を reflex で有効化しない (user 明示指示時のみ)。

---

_共通項を直すときは [`ippoan/claude-md`](https://github.com/ippoan/claude-md) の
`CLAUDE.md.template` を更新すること。_
