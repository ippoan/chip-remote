/**
 * HubDO — 全体で 1 個 (idFromName("hub")) の Durable Object。
 *
 * chip の状態機械 (docs/PROTOCOL.md「chip の状態」) と device token を SQLite に持ち、
 * Windows agent の WebSocket を hibernatable に 1 本だけ hold する。
 *
 *   located_pending ──(chip.located)──▶ notified ──(action)──▶ acting ──▶ done / failed
 *         └──(chip.not_found / LOCATE_TIMEOUT_MS)──▶ notified (located=false)
 *   any ──(DELETE)──▶ withdrawn
 *
 * タイムアウト (locate の安全網 / action の agent 応答待ち) は chips.deadline_at に
 * 保存し、DO alarm を「最も近い deadline」に合わせて張り直す (alarm は 1 DO に 1 本)。
 * in-memory 状態は持たない (hibernation で消えても困らないように)。
 */
import { DurableObject } from "cloudflare:workers";
import { Env, settings } from "./env";
import { CachedToken, FcmSender, fcmConfig } from "./fcm";

export type ChipStatus = "located_pending" | "notified" | "acting" | "done" | "failed" | "withdrawn";
export type ChipAction = "start" | "dismiss";

export interface Chip {
  task_id: string;
  title: string;
  tldr: string;
  cwd: string;
  host: string;
  session_id: string | null;
  status: ChipStatus;
  located: boolean;
  created_at: number;
  updated_at: number;
  error: string | null;
}

export interface NewChip {
  task_id: string;
  title: string;
  tldr: string;
  cwd: string;
  host: string;
  session_id: string | null;
}

export type ActionOutcome =
  | { ok: true; request_id: string }
  | { ok: false; status: 404 | 409; error: "not_found" | "agent_offline" | "chip_closed" };

type DeadlineKind = "locate" | "action";

type ChipRow = {
  task_id: string;
  title: string;
  tldr: string;
  cwd: string;
  host: string;
  session_id: string | null;
  status: ChipStatus;
  located: number;
  created_at: number;
  updated_at: number;
  error: string | null;
  deadline_at: number | null;
  deadline_kind: DeadlineKind | null;
  request_id: string | null;
  pending_action: ChipAction | null;
};

/** 接続中 agent の世代。新しい接続が来たら +1 し、古い WS は無視 / close する。 */
type AgentAttachment = { gen: number };

/** open (= phone / agent に見せる) でない状態。 */
const CLOSED: ReadonlySet<ChipStatus> = new Set(["withdrawn", "done"]);

const CLOSE_REPLACED = 4000;
const TOKEN_CACHE_KEY = "fcm_access_token";
const AGENT_GEN_KEY = "agent_gen";

function toChip(r: ChipRow): Chip {
  return {
    task_id: r.task_id,
    title: r.title,
    tldr: r.tldr,
    cwd: r.cwd,
    host: r.host,
    session_id: r.session_id,
    status: r.status,
    located: r.located === 1,
    created_at: r.created_at,
    updated_at: r.updated_at,
    error: r.error,
  };
}

export class HubDO extends DurableObject<Env> {
  private readonly sql: SqlStorage;
  private sender: FcmSender | null = null;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(
      `CREATE TABLE IF NOT EXISTS chips(
        task_id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        tldr TEXT NOT NULL,
        cwd TEXT NOT NULL,
        host TEXT NOT NULL,
        session_id TEXT,
        status TEXT NOT NULL,
        located INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        error TEXT,
        deadline_at INTEGER,
        deadline_kind TEXT,
        request_id TEXT,
        pending_action TEXT
      );`,
    );
    this.sql.exec(
      `CREATE TABLE IF NOT EXISTS devices(
        fcm_token TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );`,
    );
    this.sql.exec(`CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);`);
  }

  // ─── RPC (Worker → DO) ────────────────────────────────────────────────────

  agentConnected(): boolean {
    return this.agent() !== null;
  }

  /** POST /v1/chips。既存 task_id は上書きせず created=false。 */
  async createChip(input: NewChip): Promise<{ created: boolean; chip: Chip }> {
    const existing = this.row(input.task_id);
    if (existing) return { created: false, chip: toChip(existing) };

    const now = Date.now();
    const agent = this.agent();
    const status: ChipStatus = agent ? "located_pending" : "notified";
    this.sql.exec(
      `INSERT INTO chips(task_id, title, tldr, cwd, host, session_id, status, located,
         created_at, updated_at, deadline_at, deadline_kind)
       VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?)`,
      input.task_id,
      input.title,
      input.tldr,
      input.cwd,
      input.host,
      input.session_id,
      status,
      now,
      now,
      agent ? now + settings(this.env).locateTimeoutMs : null,
      agent ? "locate" : null,
    );
    const row = this.row(input.task_id)!;
    if (agent) {
      // agent が UIA で探して chip.located / chip.not_found を返すまで FCM は待つ。
      this.rescheduleAlarm();
      this.send(agent, { type: "chip.new", chip: toChip(row) });
    } else {
      await this.notifyChip(row);
    }
    return { created: true, chip: toChip(row) };
  }

  /**
   * DELETE /v1/chips/:task_id。未知なら null。withdrawn / done 済みなら何も送らず返す
   * (done = phone の action で完了済み。ここで chip_cancel を送ると phone の結果通知が消える)。
   */
  async withdrawChip(taskId: string): Promise<Chip | null> {
    const row = this.row(taskId);
    if (!row) return null;
    if (row.status === "withdrawn" || row.status === "done") return toChip(row);

    this.update(taskId, { status: "withdrawn", deadline_at: null, deadline_kind: null });
    this.rescheduleAlarm();
    const agent = this.agent();
    if (agent) this.send(agent, { type: "chip.withdrawn", task_id: taskId });
    await this.notify({ type: "chip_cancel", task_id: taskId });
    return toChip(this.row(taskId)!);
  }

  /** GET /v1/chips (open=true なら withdrawn / done を除く)。新しい順。 */
  listChips(open: boolean): Chip[] {
    const where = open ? `WHERE status NOT IN ('withdrawn', 'done')` : "";
    return this.sql
      .exec<ChipRow>(`SELECT * FROM chips ${where} ORDER BY created_at DESC, rowid DESC LIMIT 200`)
      .toArray()
      .map(toChip);
  }

  /** POST /v1/chips/:task_id/action。 */
  requestAction(taskId: string, action: ChipAction): ActionOutcome {
    const row = this.row(taskId);
    if (!row) return { ok: false, status: 404, error: "not_found" };
    const agent = this.agent();
    if (!agent) return { ok: false, status: 409, error: "agent_offline" };
    if (CLOSED.has(row.status)) return { ok: false, status: 409, error: "chip_closed" };

    const requestId = crypto.randomUUID();
    this.update(taskId, {
      status: "acting",
      error: null,
      request_id: requestId,
      pending_action: action,
      deadline_at: Date.now() + settings(this.env).actionTimeoutMs,
      deadline_kind: "action",
    });
    this.rescheduleAlarm();
    this.send(agent, {
      type: "action",
      request_id: requestId,
      task_id: taskId,
      action,
      title: row.title,
      tldr: row.tldr,
    });
    return { ok: true, request_id: requestId };
  }

  /** POST /v1/devices。fcm_token で upsert。 */
  upsertDevice(fcmToken: string, name: string): { created: boolean } {
    const now = Date.now();
    const exists = this.sql.exec(`SELECT 1 FROM devices WHERE fcm_token = ?`, fcmToken).toArray().length > 0;
    this.sql.exec(
      `INSERT INTO devices(fcm_token, name, created_at, updated_at) VALUES (?, ?, ?, ?)
       ON CONFLICT(fcm_token) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at`,
      fcmToken,
      name,
      now,
      now,
    );
    return { created: !exists };
  }

  // ─── agent WebSocket (hibernatable) ───────────────────────────────────────

  async fetch(req: Request): Promise<Response> {
    if (req.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return Response.json({ error: "expected_websocket" }, { status: 426 });
    }
    const gen = this.nextAgentGen();
    // 同時接続は 1 本。古い接続は close 4000 で追い出す。
    for (const old of this.ctx.getWebSockets()) {
      try {
        old.close(CLOSE_REPLACED, "replaced by a newer agent connection");
      } catch {
        // 既に閉じている
      }
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment({ gen } satisfies AgentAttachment);
    this.send(server, { type: "hello", chips: this.listChips(true) });
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (!this.isCurrent(ws)) return;
    let msg: Record<string, unknown>;
    try {
      const text = typeof message === "string" ? message : new TextDecoder().decode(message);
      const parsed: unknown = JSON.parse(text);
      if (typeof parsed !== "object" || parsed === null) throw new Error("not an object");
      msg = parsed as Record<string, unknown>;
    } catch {
      console.warn("agent sent a non-JSON message; ignored");
      return;
    }
    const taskId = typeof msg.task_id === "string" ? msg.task_id : "";
    switch (msg.type) {
      case "chip.located":
        return this.onLocated(taskId);
      case "chip.not_found":
        return this.onNotFound(taskId);
      case "action.result":
        return this.onActionResult(
          taskId,
          typeof msg.request_id === "string" ? msg.request_id : "",
          msg.ok === true,
          typeof msg.error === "string" ? msg.error : null,
        );
      case "pong":
        return;
      default:
        console.warn(`agent sent unknown message type: ${String(msg.type)}`);
    }
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string): Promise<void> {
    // compat date 2026-04-07 未満は close handshake を自前で返す必要がある。
    try {
      ws.close(code, reason);
    } catch {
      // 既に閉じている
    }
  }

  // ─── alarm (locate の安全網 / action timeout) ─────────────────────────────

  async alarm(): Promise<void> {
    const now = Date.now();
    const due = this.sql
      .exec<ChipRow>(`SELECT * FROM chips WHERE deadline_at IS NOT NULL AND deadline_at <= ?`, now)
      .toArray();
    for (const row of due) {
      this.update(row.task_id, { deadline_at: null, deadline_kind: null });
      if (row.deadline_kind === "locate" && row.status === "located_pending") {
        this.update(row.task_id, { status: "notified", located: 0 });
        await this.notifyChip(this.row(row.task_id)!);
      } else if (row.deadline_kind === "action" && row.status === "acting") {
        this.update(row.task_id, { status: "failed", error: "agent_timeout" });
        await this.notify({
          type: "chip_result",
          task_id: row.task_id,
          action: row.pending_action ?? "",
          ok: "false",
          error: "agent_timeout",
        });
      }
    }
    this.rescheduleAlarm();
  }

  // ─── agent → Worker メッセージ ─────────────────────────────────────────────

  private async onLocated(taskId: string): Promise<void> {
    const row = this.row(taskId);
    if (!row || CLOSED.has(row.status)) return;
    if (row.status === "located_pending") {
      this.update(taskId, { status: "notified", located: 1, deadline_at: null, deadline_kind: null });
      this.rescheduleAlarm();
      await this.notifyChip(this.row(taskId)!);
    } else if (row.status === "notified" && row.located === 0) {
      // 先に located=false で通知済み (agent 再接続後の hello 等で見つかった) →
      // 同じ通知 ID で located=true に更新する。
      this.update(taskId, { located: 1 });
      await this.notifyChip(this.row(taskId)!);
    } else if (row.located === 0) {
      this.update(taskId, { located: 1 });
    }
  }

  private async onNotFound(taskId: string): Promise<void> {
    const row = this.row(taskId);
    if (!row || row.status !== "located_pending") return;
    this.update(taskId, { status: "notified", located: 0, deadline_at: null, deadline_kind: null });
    this.rescheduleAlarm();
    await this.notifyChip(this.row(taskId)!);
  }

  private async onActionResult(
    taskId: string,
    requestId: string,
    ok: boolean,
    error: string | null,
  ): Promise<void> {
    const row = this.row(taskId);
    if (!row || row.status !== "acting" || row.request_id !== requestId) {
      console.warn(`stale action.result ignored: task_id=${taskId} request_id=${requestId}`);
      return;
    }
    const err = ok ? null : error ?? "unknown";
    this.update(taskId, {
      status: ok ? "done" : "failed",
      error: err,
      deadline_at: null,
      deadline_kind: null,
    });
    this.rescheduleAlarm();
    await this.notify({
      type: "chip_result",
      task_id: taskId,
      action: row.pending_action ?? "",
      ok: ok ? "true" : "false",
      error: err ?? "",
    });
  }

  // ─── FCM ──────────────────────────────────────────────────────────────────

  private notifyChip(row: ChipRow): Promise<void> {
    return this.notify({
      type: "chip",
      task_id: row.task_id,
      title: row.title,
      tldr: row.tldr,
      cwd: row.cwd,
      host: row.host,
      located: row.located === 1 ? "true" : "false",
    });
  }

  /** 登録済み全 device に data メッセージを送る。FCM 未設定なら log してスキップ。 */
  private async notify(data: Record<string, string>): Promise<void> {
    const sender = await this.fcm();
    if (!sender) {
      console.log(`fcm not configured; skip ${data.type} task_id=${data.task_id}`);
      return;
    }
    const tokens = this.sql
      .exec<{ fcm_token: string }>(`SELECT fcm_token FROM devices`)
      .toArray()
      .map((r) => r.fcm_token);
    if (tokens.length === 0) {
      console.log(`no devices registered; skip ${data.type} task_id=${data.task_id}`);
      return;
    }
    const results = await Promise.all(tokens.map((t) => sender.send(t, data)));
    results.forEach((r, i) => {
      if (r === "unregistered") {
        this.sql.exec(`DELETE FROM devices WHERE fcm_token = ?`, tokens[i]);
      }
    });
  }

  private async fcm(): Promise<FcmSender | null> {
    if (this.sender) return this.sender;
    const cfg = await fcmConfig(this.env);
    if (!cfg) return null;
    this.sender = new FcmSender(cfg, {
      get: () => {
        const r = this.sql
          .exec<{ value: string }>(`SELECT value FROM kv WHERE key = ?`, TOKEN_CACHE_KEY)
          .toArray()[0];
        return r ? (JSON.parse(r.value) as CachedToken) : null;
      },
      set: (t) => {
        if (t === null) {
          this.sql.exec(`DELETE FROM kv WHERE key = ?`, TOKEN_CACHE_KEY);
        } else {
          this.sql.exec(
            `INSERT OR REPLACE INTO kv(key, value) VALUES (?, ?)`,
            TOKEN_CACHE_KEY,
            JSON.stringify(t),
          );
        }
      },
    });
    return this.sender;
  }

  // ─── helpers ──────────────────────────────────────────────────────────────

  private row(taskId: string): ChipRow | null {
    return this.sql.exec<ChipRow>(`SELECT * FROM chips WHERE task_id = ?`, taskId).toArray()[0] ?? null;
  }

  private update(taskId: string, fields: Partial<Omit<ChipRow, "task_id">>): void {
    const keys = Object.keys(fields) as (keyof typeof fields)[];
    const sets = keys.map((k) => `${k} = ?`).join(", ");
    this.sql.exec(
      `UPDATE chips SET ${sets}, updated_at = ? WHERE task_id = ?`,
      ...keys.map((k) => fields[k] ?? null),
      Date.now(),
      taskId,
    );
  }

  private rescheduleAlarm(): void {
    const next = this.sql
      .exec<{ next: number | null }>(`SELECT MIN(deadline_at) AS next FROM chips WHERE deadline_at IS NOT NULL`)
      .one().next;
    if (next === null) {
      void this.ctx.storage.deleteAlarm();
    } else {
      void this.ctx.storage.setAlarm(next);
    }
  }

  private nextAgentGen(): number {
    const cur = this.sql
      .exec<{ value: string }>(`SELECT value FROM kv WHERE key = ?`, AGENT_GEN_KEY)
      .toArray()[0];
    const gen = (cur ? Number(cur.value) : 0) + 1;
    this.sql.exec(`INSERT OR REPLACE INTO kv(key, value) VALUES (?, ?)`, AGENT_GEN_KEY, String(gen));
    return gen;
  }

  private isCurrent(ws: WebSocket): boolean {
    const att = ws.deserializeAttachment() as AgentAttachment | null;
    const cur = this.sql
      .exec<{ value: string }>(`SELECT value FROM kv WHERE key = ?`, AGENT_GEN_KEY)
      .toArray()[0];
    return !!att && !!cur && att.gen === Number(cur.value);
  }

  /** 現世代の、開いている agent WS。無ければ null。 */
  private agent(): WebSocket | null {
    for (const ws of this.ctx.getWebSockets()) {
      if (ws.readyState === WebSocket.OPEN && this.isCurrent(ws)) return ws;
    }
    return null;
  }

  private send(ws: WebSocket, msg: Record<string, unknown>): void {
    try {
      ws.send(JSON.stringify(msg));
    } catch (e) {
      console.warn(`ws send failed: ${(e as Error).message}`);
    }
  }
}
