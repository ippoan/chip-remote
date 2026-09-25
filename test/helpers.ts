import { SELF, env } from "cloudflare:test";

export const BASE = "https://chip-remote.test";
export const TOKEN = "test-token"; // vitest.config.ts の CHIP_REMOTE_TOKEN と一致
export const AUTH = { Authorization: `Bearer ${TOKEN}` };

export interface FcmSent {
  token: string;
  data: Record<string, string>;
  android: { priority: string };
}

interface FcmStubState {
  sent: FcmSent[];
  tokenRequests: URLSearchParams[];
  original: typeof fetch;
}

declare global {
  // eslint-disable-next-line no-var
  var __fcm: FcmStubState | undefined;
}

export function fcm(): FcmStubState {
  return globalThis.__fcm!;
}

/** device token の接頭辞で FCM の応答を切り替える: dead- → 404, gone- → 400 UNREGISTERED, bad- → 500。 */
export function installFcmStub(): void {
  if (globalThis.__fcm) return;
  const state: FcmStubState = { sent: [], tokenRequests: [], original: globalThis.fetch };
  globalThis.__fcm = state;
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const req = new Request(input, init);
    const url = new URL(req.url);
    if (url.origin === "https://oauth2.googleapis.com" && url.pathname === "/token") {
      state.tokenRequests.push(new URLSearchParams(await req.text()));
      return Response.json({ access_token: "ya29.test", expires_in: 3600, token_type: "Bearer" });
    }
    if (url.origin === "https://fcm.googleapis.com") {
      const body = (await req.json()) as { message: FcmSent };
      const token = body.message.token;
      state.sent.push(body.message);
      if (token.startsWith("dead-")) {
        return Response.json({ error: { code: 404, status: "NOT_FOUND" } }, { status: 404 });
      }
      if (token.startsWith("gone-")) {
        return Response.json(
          { error: { code: 400, details: [{ errorCode: "UNREGISTERED" }] } },
          { status: 400 },
        );
      }
      if (token.startsWith("bad-")) return new Response("boom", { status: 500 });
      return Response.json({ name: "projects/alc-fcm/messages/1" });
    }
    return state.original(input, init);
  };
}

/** task_id に対して送られた FCM data (main device 宛のみ)。 */
export function sentFor(taskId: string, token = MAIN_DEVICE): Record<string, string>[] {
  return fcm()
    .sent.filter((m) => m.token === token && m.data.task_id === taskId)
    .map((m) => m.data);
}

export const MAIN_DEVICE = "main-device-token";

export function uid(prefix = "task"): string {
  return `${prefix}_${crypto.randomUUID().slice(0, 8)}`;
}

export async function api(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (!headers.has("Authorization")) headers.set("Authorization", AUTH.Authorization);
  if (init.body !== undefined && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  return SELF.fetch(`${BASE}${path}`, { ...init, headers });
}

export async function postChip(taskId: string, extra: Record<string, unknown> = {}): Promise<Response> {
  return api("/v1/chips", {
    method: "POST",
    body: JSON.stringify({
      task_id: taskId,
      title: `title of ${taskId}`,
      tldr: "tldr",
      cwd: "/home/claude/x",
      host: "mini-ryzen",
      session_id: null,
      ...extra,
    }),
  });
}

export interface ChipJson {
  task_id: string;
  title: string;
  tldr: string;
  cwd: string;
  host: string;
  session_id: string | null;
  status: string;
  located: boolean;
  created_at: number;
  updated_at: number;
  error: string | null;
}

export async function getChip(taskId: string): Promise<ChipJson | undefined> {
  const res = await api("/v1/chips");
  const { chips } = (await res.json()) as { chips: ChipJson[] };
  return chips.find((c) => c.task_id === taskId);
}

export async function waitFor<T>(fn: () => T | Promise<T>, timeoutMs = 3000): Promise<NonNullable<T>> {
  const start = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v as NonNullable<T>;
    if (Date.now() - start > timeoutMs) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 10));
  }
}

export type AgentMsg = { type: string } & Record<string, unknown>;

export interface Agent {
  ws: WebSocket;
  msgs: AgentMsg[];
  closed: { code: number; reason: string } | null;
  next(type: string, pred?: (m: AgentMsg) => boolean): Promise<AgentMsg>;
  sendJson(msg: unknown): void;
  close(): Promise<void>;
}

export async function connectAgent(): Promise<Agent> {
  const res = await SELF.fetch(`${BASE}/v1/agent/ws`, {
    headers: { Upgrade: "websocket", ...AUTH },
  });
  if (res.status !== 101 || !res.webSocket) throw new Error(`ws upgrade failed: ${res.status}`);
  const ws = res.webSocket;
  const agent: Agent = {
    ws,
    msgs: [],
    closed: null,
    next: (type, pred = () => true) =>
      waitFor(() => agent.msgs.find((m) => m.type === type && pred(m))),
    sendJson: (msg) => ws.send(JSON.stringify(msg)),
    close: async () => {
      ws.close(1000, "bye");
      await waitFor(async () => {
        const h = (await (await SELF.fetch(`${BASE}/health`)).json()) as { agent_connected: boolean };
        return !h.agent_connected;
      });
    },
  };
  ws.addEventListener("message", (e) => {
    agent.msgs.push(JSON.parse(e.data as string) as AgentMsg);
  });
  ws.addEventListener("close", (e) => {
    agent.closed = { code: e.code, reason: e.reason };
  });
  ws.accept();
  return agent;
}

export function hubStub() {
  return env.HUB.get(env.HUB.idFromName("hub"));
}
