/**
 * chip-remote Worker エントリ (Hono)。契約は docs/PROTOCOL.md。
 *
 *   GET    /health                       … { ok, agent_connected }。認証なし
 *   POST   /v1/chips                     … hook: chip 生成 (201 / 冪等 200)
 *   DELETE /v1/chips/:task_id            … hook: chip 取り下げ
 *   GET    /v1/chips[?open=1]            … phone: chip 一覧 (新しい順)
 *   POST   /v1/chips/:task_id/action     … phone: start / dismiss (202 / 409)
 *   POST   /v1/devices                   … phone: FCM token 登録 (upsert)
 *   GET    /v1/agent/ws                  … Windows agent の WebSocket (hibernatable)
 *
 * Worker は認証と入力検証だけを持ち、状態はすべて HubDO (idFromName("hub")) の RPC に委ねる。
 */
import { Hono } from "hono";
import type { Context } from "hono";
import { checkToken } from "./auth";
import type { Env } from "./env";
import type { ChipAction, NewChip } from "./hub";

export { HubDO } from "./hub";

type AppEnv = { Bindings: Env };

export const app = new Hono<AppEnv>();

function hub(env: Env) {
  return env.HUB.get(env.HUB.idFromName("hub"));
}

function error(c: Context<AppEnv>, code: string, status: 400 | 401 | 404 | 409 | 426 | 500 | 503) {
  return c.json({ error: code }, status);
}

// /health 以外はすべて Bearer 認証。token 未設定なら fail-closed (503)。
app.use("*", async (c, next) => {
  if (c.req.path === "/health") return next();
  const r = await checkToken(c.req.raw, c.env);
  if (r === "not_configured") return error(c, "token_not_configured", 503);
  if (r !== "ok") return error(c, "unauthorized", 401);
  return next();
});

app.get("/health", async (c) => {
  const connected = await hub(c.env).agentConnected();
  return c.json({ ok: true, agent_connected: connected });
});

async function readJson(c: Context<AppEnv>): Promise<Record<string, unknown> | null> {
  try {
    const body: unknown = await c.req.json();
    return typeof body === "object" && body !== null && !Array.isArray(body)
      ? (body as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function optString(v: unknown): string | null | undefined {
  if (v === undefined || v === null) return undefined;
  return typeof v === "string" ? v : null;
}

app.post("/v1/chips", async (c) => {
  const body = await readJson(c);
  if (!body) return error(c, "invalid_request", 400);
  const { task_id, title } = body;
  if (typeof task_id !== "string" || task_id === "" || typeof title !== "string") {
    return error(c, "invalid_request", 400);
  }
  const tldr = optString(body.tldr);
  const cwd = optString(body.cwd);
  const host = optString(body.host);
  const sessionId = optString(body.session_id);
  if (tldr === null || cwd === null || host === null || sessionId === null) {
    return error(c, "invalid_request", 400);
  }
  const input: NewChip = {
    task_id,
    title,
    tldr: tldr ?? "",
    cwd: cwd ?? "",
    host: host ?? "",
    session_id: sessionId ?? null,
  };
  const r = await hub(c.env).createChip(input);
  return c.json({ chip: r.chip }, r.created ? 201 : 200);
});

app.get("/v1/chips", async (c) => {
  const chips = await hub(c.env).listChips(c.req.query("open") === "1");
  return c.json({ chips });
});

app.delete("/v1/chips/:task_id", async (c) => {
  const chip = await hub(c.env).withdrawChip(c.req.param("task_id"));
  if (!chip) return error(c, "not_found", 404);
  return c.json({ chip });
});

app.post("/v1/chips/:task_id/action", async (c) => {
  const body = await readJson(c);
  const action = body?.action;
  if (action !== "start" && action !== "dismiss") return error(c, "invalid_request", 400);
  const r = await hub(c.env).requestAction(c.req.param("task_id"), action as ChipAction);
  if (!r.ok) return error(c, r.error, r.status);
  return c.json({ request_id: r.request_id }, 202);
});

app.post("/v1/devices", async (c) => {
  const body = await readJson(c);
  const token = body?.fcm_token;
  const name = optString(body?.name);
  if (typeof token !== "string" || token === "" || name === null) {
    return error(c, "invalid_request", 400);
  }
  const r = await hub(c.env).upsertDevice(token, name ?? "");
  return c.json({ ok: true }, r.created ? 201 : 200);
});

app.get("/v1/agent/ws", async (c) => {
  if (c.req.header("Upgrade")?.toLowerCase() !== "websocket") {
    return error(c, "expected_websocket", 426);
  }
  return hub(c.env).fetch(c.req.raw);
});

app.notFound((c) => error(c, "not_found", 404));

app.onError((err, c) => {
  console.error(`unhandled error: ${err.message}`);
  return error(c, "internal", 500);
});

export default app;
