/**
 * FCM HTTP v1 送信 (docs/PROTOCOL.md「FCM」)。
 *
 * service account の private_key (PKCS8 PEM) で RS256 JWT を WebCrypto 署名し、
 * https://oauth2.googleapis.com/token で access token に交換する。access token は
 * 期限の 5 分前まで TokenCache (HubDO の SQLite) に保持して使い回す。
 *
 * メッセージは data-only + android.priority=high。data の値はすべて文字列。
 * send() は例外を投げず、結果を "ok" / "unregistered" / "error" で返す
 * (unregistered = 404 か errorCode UNREGISTERED → 呼び出し側が device token を削除)。
 */
import type { Env } from "./env";

export const TOKEN_URL = "https://oauth2.googleapis.com/token";
export const FCM_SCOPE = "https://www.googleapis.com/auth/firebase.messaging";
const DEFAULT_PROJECT_ID = "alc-fcm";
/** access token を期限のこれだけ前に捨てて取り直す。 */
const REFRESH_MARGIN_MS = 5 * 60 * 1000;
const JWT_LIFETIME_S = 3600;

export interface FcmConfig {
  projectId: string;
  clientEmail: string;
  privateKeyPem: string;
}

/** FCM secret が揃っていれば設定を返す。欠けていれば null (= 送信スキップ)。 */
export function fcmConfig(env: Env): FcmConfig | null {
  const clientEmail = (env.FCM_CLIENT_EMAIL ?? "").trim();
  const privateKeyPem = env.FCM_PRIVATE_KEY ?? "";
  if (clientEmail === "" || privateKeyPem.trim() === "") return null;
  const projectId = (env.FCM_PROJECT_ID ?? "").trim() || DEFAULT_PROJECT_ID;
  return { projectId, clientEmail, privateKeyPem };
}

export interface CachedToken {
  access_token: string;
  /** epoch ms */
  expires_at: number;
}

export interface TokenCache {
  get(): CachedToken | null;
  set(token: CachedToken | null): void;
}

export type SendResult = "ok" | "unregistered" | "error";

type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

function base64url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function base64urlJson(obj: unknown): string {
  return base64url(new TextEncoder().encode(JSON.stringify(obj)));
}

/** PKCS8 PEM → DER。service account JSON の "\n" エスケープが残っていても受ける。 */
export function pemToDer(pem: string): Uint8Array {
  const body = pem
    .replace(/\\n/g, "\n")
    .replace(/-----(BEGIN|END) [A-Z ]+-----/g, "")
    .replace(/\s+/g, "");
  const bin = atob(body);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class FcmSender {
  private key: Promise<CryptoKey> | null = null;

  constructor(
    private readonly cfg: FcmConfig,
    private readonly cache: TokenCache,
    // 呼び出し時に globalThis.fetch を引く (テストの差し替えを効かせるため)。
    private readonly fetchFn: FetchFn = (input, init) => fetch(input, init),
  ) {}

  private signingKey(): Promise<CryptoKey> {
    if (!this.key) {
      this.key = crypto.subtle.importKey(
        "pkcs8",
        pemToDer(this.cfg.privateKeyPem),
        { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
        false,
        ["sign"],
      );
      // import 失敗を永続キャッシュしない。
      this.key.catch(() => {
        this.key = null;
      });
    }
    return this.key;
  }

  /** service account JWT (RS256) を作る。 */
  async signAssertion(nowMs = Date.now()): Promise<string> {
    const iat = Math.floor(nowMs / 1000);
    const header = base64urlJson({ alg: "RS256", typ: "JWT" });
    const claims = base64urlJson({
      iss: this.cfg.clientEmail,
      scope: FCM_SCOPE,
      aud: TOKEN_URL,
      iat,
      exp: iat + JWT_LIFETIME_S,
    });
    const input = `${header}.${claims}`;
    const sig = await crypto.subtle.sign(
      "RSASSA-PKCS1-v1_5",
      await this.signingKey(),
      new TextEncoder().encode(input),
    );
    return `${input}.${base64url(new Uint8Array(sig))}`;
  }

  /** キャッシュが有効ならそれを、無ければ JWT を交換して access token を返す。 */
  async accessToken(): Promise<string> {
    const now = Date.now();
    const cached = this.cache.get();
    if (cached && cached.expires_at - REFRESH_MARGIN_MS > now) return cached.access_token;

    const assertion = await this.signAssertion(now);
    const res = await this.fetchFn(TOKEN_URL, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
        assertion,
      }).toString(),
    });
    if (!res.ok) {
      throw new Error(`oauth token exchange failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { access_token?: string; expires_in?: number };
    if (!body.access_token) throw new Error("oauth token exchange: no access_token");
    const token: CachedToken = {
      access_token: body.access_token,
      expires_at: now + (body.expires_in ?? JWT_LIFETIME_S) * 1000,
    };
    this.cache.set(token);
    return token.access_token;
  }

  /** data-only メッセージを 1 端末に送る。例外は投げない。 */
  async send(deviceToken: string, data: Record<string, string>): Promise<SendResult> {
    try {
      const accessToken = await this.accessToken();
      const res = await this.fetchFn(
        `https://fcm.googleapis.com/v1/projects/${this.cfg.projectId}/messages:send`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({
            message: { token: deviceToken, data, android: { priority: "high" } },
          }),
        },
      );
      if (res.ok) return "ok";

      const text = await res.text();
      if (res.status === 404 || isUnregistered(text)) return "unregistered";
      // access token が失効 / 取り消された → 次回は取り直す。
      if (res.status === 401) this.cache.set(null);
      console.warn(`fcm send failed: ${res.status} ${text}`);
      return "error";
    } catch (e) {
      console.warn(`fcm send error: ${(e as Error).message}`);
      return "error";
    }
  }
}

function isUnregistered(body: string): boolean {
  try {
    const parsed = JSON.parse(body) as {
      error?: { details?: Array<{ errorCode?: string }> };
    };
    return (parsed.error?.details ?? []).some((d) => d.errorCode === "UNREGISTERED");
  } catch {
    return false;
  }
}
