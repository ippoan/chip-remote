import { defineWorkersConfig } from "@cloudflare/vitest-pool-workers/config";

/**
 * vitest を workerd 上 (@cloudflare/vitest-pool-workers) で動かす。
 *
 * wrangler.toml は読み込まず、DO / vars / secret を miniflare options に直書きする
 * (cdp-relay と同方式)。FCM の service account 鍵はテスト起動時に使い捨ての RSA 鍵を
 * WebCrypto で生成して inject する (= 秘密鍵をリポジトリに置かない)。FCM / OAuth への
 * 外向き fetch は test/setup.ts が globalThis.fetch を差し替えて握りつぶす。
 */
async function makeTestPrivateKeyPem(): Promise<string> {
  const pair = (await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const der = new Uint8Array((await crypto.subtle.exportKey("pkcs8", pair.privateKey)) as ArrayBuffer);
  let bin = "";
  for (const b of der) bin += String.fromCharCode(b);
  const b64 = btoa(bin).replace(/(.{64})/g, "$1\n");
  return `-----BEGIN PRIVATE KEY-----\n${b64}\n-----END PRIVATE KEY-----\n`;
}

// package.json が CJS 扱いなので top-level await は使えない → Promise を渡す。
export default defineWorkersConfig(
  makeTestPrivateKeyPem().then((testPrivateKey) => ({
      test: {
        setupFiles: ["./test/setup.ts"],
        coverage: {
          // v8 coverage は vitest-pool-workers (workerd isolate) と相性が悪い。
          // istanbul はソースレベル instrument なので workerd 内まで通る。
          provider: "istanbul" as const,
          reporter: ["text", "json-summary", "lcov"],
          reportsDirectory: "./coverage",
          include: ["src/**/*.ts"],
          // src/env.ts は型と設定値のパースだけ。
          exclude: ["src/env.ts"],
        },
        poolOptions: {
          workers: {
            // HubDO は idFromName("hub") の 1 個だけなので、テストファイル間で agent WS /
            // chip が干渉しないよう 1 worker で直列に流す。
            singleWorker: true,
            // SQLite-backed DO の per-test 隔離ストレージは sqlite-shm/wal の stack-frame
            // pop に失敗する既知問題があるため無効化する。各テストはユニークな task_id を
            // 採番するので干渉しない。
            isolatedStorage: false,
            main: "./src/index.ts",
            miniflare: {
              compatibilityDate: "2025-05-01",
              compatibilityFlags: ["nodejs_compat"],
              durableObjects: {
                HUB: { className: "HubDO", useSQLite: true },
              },
              bindings: {
                // テスト用 token (test/helpers.ts の TOKEN と一致させる)。
                CHIP_REMOTE_TOKEN: "test-token",
                FCM_PROJECT_ID: "alc-fcm",
                CHIP_REMOTE_FCM_SA_KEY: JSON.stringify({
                  client_email: "chip-remote@alc-fcm.iam.gserviceaccount.com",
                  private_key: testPrivateKey,
                  project_id: "alc-fcm",
                }),
                ACTION_TIMEOUT_MS: "30000",
                LOCATE_TIMEOUT_MS: "75000",
              },
            },
          },
        },
      },
  })),
);
