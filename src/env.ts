/**
 * Worker / DO の binding と設定値。
 *
 * 設定値は wrangler.toml の [vars] で渡し、ここで数値化する (ハードコードしない)。
 * secret は CF Secrets Store binding (本番) か plain string (テスト)。readSecret で両形を解決する。
 */
import type { HubDO } from "./hub";

/** CF Secrets Store の binding。テストでは plain string を直接 inject する。 */
export type SecretsStoreBinding = { get(): Promise<string> };

/** secret を文字列で返す。未設定・取得失敗は "" (呼び出し側が fail-closed / skip を決める)。 */
export async function readSecret(v: SecretsStoreBinding | string | undefined): Promise<string> {
  if (v === undefined) return "";
  if (typeof v === "string") return v;
  try {
    return await v.get();
  } catch (e) {
    console.error(`secrets store get failed: ${(e as Error).message}`);
    return "";
  }
}

export interface Env {
  HUB: DurableObjectNamespace<HubDO>;

  // ─── secrets ───
  /** hook / agent / phone 共通の Bearer token。未設定なら /health 以外 503 (fail-closed)。 */
  CHIP_REMOTE_TOKEN?: SecretsStoreBinding | string;
  /**
   * FCM 送信専用 service account (chip-remote-fcm@alc-fcm) の鍵 JSON をそのまま。
   * GCP (cloudsql-sv) の同名 secret が SoT。未設定なら FCM はスキップ。
   */
  CHIP_REMOTE_FCM_SA_KEY?: SecretsStoreBinding | string;

  // ─── 設定値 (文字列 vars。未設定なら下の default) ───
  /** FCM HTTP v1 の Firebase project id。 */
  FCM_PROJECT_ID?: string;
  /** action を agent が処理するまでの待ち時間 (ms)。 */
  ACTION_TIMEOUT_MS?: string;
  /** agent が chip.located / chip.not_found を返すまでの安全網 (ms)。 */
  LOCATE_TIMEOUT_MS?: string;
}

export interface Settings {
  actionTimeoutMs: number;
  locateTimeoutMs: number;
}

const DEFAULTS: Settings = {
  actionTimeoutMs: 30_000,
  locateTimeoutMs: 75_000,
};

function num(raw: string | undefined, fallback: number): number {
  if (raw === undefined || raw === "") return fallback;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : fallback;
}

export function settings(env: Env): Settings {
  return {
    actionTimeoutMs: num(env.ACTION_TIMEOUT_MS, DEFAULTS.actionTimeoutMs),
    locateTimeoutMs: num(env.LOCATE_TIMEOUT_MS, DEFAULTS.locateTimeoutMs),
  };
}
