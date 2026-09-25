/**
 * Bearer token 認証 (docs/PROTOCOL.md「認証」)。
 *
 * `Authorization: Bearer <CHIP_REMOTE_TOKEN>` を定数時間で比較する。両辺を SHA-256 に
 * してから crypto.subtle.timingSafeEqual に掛けるので、長さの違いも漏れない。
 */
import type { Env } from "./env";

export type TokenCheck = "ok" | "not_configured" | "missing_token" | "bad_token";

async function sha256(s: string): Promise<ArrayBuffer> {
  return crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
}

/** 定数時間の文字列比較 (長さも秘匿)。 */
export async function timingSafeEqual(a: string, b: string): Promise<boolean> {
  const [ha, hb] = await Promise.all([sha256(a), sha256(b)]);
  return crypto.subtle.timingSafeEqual(ha, hb);
}

export async function checkToken(req: Request, env: Env): Promise<TokenCheck> {
  // secret 投入時の末尾改行 (CR/LF) で全員が 401 にならないよう trim する。
  const configured = (env.CHIP_REMOTE_TOKEN ?? "").trim();
  if (configured === "") return "not_configured";

  const header = req.headers.get("Authorization") ?? "";
  const m = header.match(/^Bearer\s+(.+)$/i);
  const presented = m ? m[1].trim() : "";
  if (presented === "") return "missing_token";
  return (await timingSafeEqual(presented, configured)) ? "ok" : "bad_token";
}
