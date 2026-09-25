import { describe, it, expect, beforeAll, afterEach } from "vitest";
import { SELF, runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import {
  Agent,
  BASE,
  MAIN_DEVICE,
  api,
  connectAgent,
  fcm,
  getChip,
  hubStub,
  postChip,
  sentFor,
  uid,
  waitFor,
} from "./helpers";

let agents: Agent[] = [];

async function agent(): Promise<Agent> {
  const a = await connectAgent();
  agents.push(a);
  return a;
}

afterEach(async () => {
  for (const a of agents) {
    if (!a.closed) await a.close();
  }
  agents = [];
});

beforeAll(async () => {
  const res = await api("/v1/devices", {
    method: "POST",
    body: JSON.stringify({ fcm_token: MAIN_DEVICE, name: "Pixel 8" }),
  });
  expect(res.status).toBe(201);
});

/** deadline を過去にしてから alarm を走らせる (実時間を待たずに timeout 経路を踏む)。 */
async function expireDeadline(taskId: string): Promise<void> {
  const stub = hubStub();
  await runInDurableObject(stub, (_instance, state) => {
    state.storage.sql.exec(`UPDATE chips SET deadline_at = 1 WHERE task_id = ?`, taskId);
  });
  expect(await runDurableObjectAlarm(stub)).toBe(true);
}

describe("/health", () => {
  it("認証なしで ok / agent_connected を返す", async () => {
    const res = await SELF.fetch(`${BASE}/health`);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true, agent_connected: false });
  });

  it("agent 接続中は agent_connected=true", async () => {
    await agent();
    const res = await SELF.fetch(`${BASE}/health`);
    expect(await res.json()).toEqual({ ok: true, agent_connected: true });
  });
});

describe("POST /v1/chips (agent 未接続)", () => {
  it("新規は 201 + 即 FCM chip (located=false)、再送は 200 で上書きしない", async () => {
    const id = uid();
    const r1 = await postChip(id, { session_id: "sess-1" });
    expect(r1.status).toBe(201);
    const { chip } = (await r1.json()) as { chip: Record<string, unknown> };
    expect(chip).toMatchObject({
      task_id: id,
      title: `title of ${id}`,
      tldr: "tldr",
      cwd: "/home/claude/x",
      host: "mini-ryzen",
      session_id: "sess-1",
      status: "notified",
      located: false,
      error: null,
    });
    expect(sentFor(id)).toEqual([
      {
        type: "chip",
        task_id: id,
        title: `title of ${id}`,
        tldr: "tldr",
        cwd: "/home/claude/x",
        host: "mini-ryzen",
        located: "false",
      },
    ]);
    const sentMsg = fcm().sent.find((m) => m.data.task_id === id)!;
    expect(sentMsg.android).toEqual({ priority: "high" });

    const r2 = await postChip(id, { title: "changed" });
    expect(r2.status).toBe(200);
    expect(((await r2.json()) as { chip: { title: string } }).chip.title).toBe(`title of ${id}`);
    expect(sentFor(id)).toHaveLength(1);
  });

  it("tldr / cwd / host / session_id は省略可", async () => {
    const id = uid();
    const res = await api("/v1/chips", {
      method: "POST",
      body: JSON.stringify({ task_id: id, title: "t" }),
    });
    expect(res.status).toBe(201);
    expect(((await res.json()) as { chip: unknown }).chip).toMatchObject({
      tldr: "",
      cwd: "",
      host: "",
      session_id: null,
    });
  });

  it.each([
    ["JSON でない", "not json"],
    ["配列", "[]"],
    ["task_id 欠落", JSON.stringify({ title: "t" })],
    ["task_id 空", JSON.stringify({ task_id: "", title: "t" })],
    ["title 欠落", JSON.stringify({ task_id: "task_x" })],
    ["tldr が数値", JSON.stringify({ task_id: "task_x", title: "t", tldr: 1 })],
  ])("不正な body (%s) は 400", async (_name, body) => {
    const res = await api("/v1/chips", { method: "POST", body });
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "invalid_request" });
  });
});

describe("GET /v1/chips", () => {
  it("open=1 は withdrawn / done を除き新しい順", async () => {
    const a = uid();
    const b = uid();
    await postChip(a);
    await new Promise((r) => setTimeout(r, 5));
    await postChip(b);
    await api(`/v1/chips/${a}`, { method: "DELETE" });

    const open = (await (await api("/v1/chips?open=1")).json()) as { chips: { task_id: string }[] };
    const ids = open.chips.map((c) => c.task_id);
    expect(ids).toContain(b);
    expect(ids).not.toContain(a);

    const all = (await (await api("/v1/chips")).json()) as { chips: { task_id: string; created_at: number }[] };
    const allIds = all.chips.map((c) => c.task_id);
    expect(allIds.indexOf(b)).toBeLessThan(allIds.indexOf(a));
    const times = all.chips.map((c) => c.created_at);
    expect([...times].sort((x, y) => y - x)).toEqual(times);
  });
});

describe("DELETE /v1/chips/:task_id", () => {
  it("withdrawn にして agent に chip.withdrawn、FCM に chip_cancel。2 回目は再送しない", async () => {
    const id = uid();
    await postChip(id);
    const ag = await agent();

    const res = await api(`/v1/chips/${id}`, { method: "DELETE" });
    expect(res.status).toBe(200);
    expect(((await res.json()) as { chip: { status: string } }).chip.status).toBe("withdrawn");
    await ag.next("chip.withdrawn", (m) => m.task_id === id);
    expect(sentFor(id).filter((d) => d.type === "chip_cancel")).toEqual([
      { type: "chip_cancel", task_id: id },
    ]);

    const again = await api(`/v1/chips/${id}`, { method: "DELETE" });
    expect(again.status).toBe(200);
    expect(sentFor(id).filter((d) => d.type === "chip_cancel")).toHaveLength(1);
  });

  it("agent 未接続でも withdrawn になる", async () => {
    const id = uid();
    await postChip(id);
    const res = await api(`/v1/chips/${id}`, { method: "DELETE" });
    expect(res.status).toBe(200);
    expect((await getChip(id))?.status).toBe("withdrawn");
  });

  it("未知の task_id は 404", async () => {
    const res = await api(`/v1/chips/${uid()}`, { method: "DELETE" });
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "not_found" });
  });
});

describe("POST /v1/chips/:task_id/action", () => {
  it("agent 未接続は 409 agent_offline", async () => {
    const id = uid();
    await postChip(id);
    const res = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "start" }),
    });
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({ error: "agent_offline" });
  });

  it("未知の chip は 404", async () => {
    await agent();
    const res = await api(`/v1/chips/${uid()}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "start" }),
    });
    expect(res.status).toBe(404);
  });

  it.each([["{}"], [JSON.stringify({ action: "open" })], ["nope"]])("不正な action (%s) は 400", async (body) => {
    const res = await api(`/v1/chips/${uid()}/action`, { method: "POST", body });
    expect(res.status).toBe(400);
  });

  it("agent 接続中は 202 + WS action、action.result ok で done + FCM chip_result", async () => {
    const id = uid();
    await postChip(id, { tldr: "why" });
    const ag = await agent();

    const res = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "start" }),
    });
    expect(res.status).toBe(202);
    const { request_id } = (await res.json()) as { request_id: string };
    expect(request_id).toMatch(/^[0-9a-f-]{36}$/);

    const msg = await ag.next("action", (m) => m.task_id === id);
    expect(msg).toEqual({
      type: "action",
      request_id,
      task_id: id,
      action: "start",
      title: `title of ${id}`,
      tldr: "why",
    });
    expect((await getChip(id))?.status).toBe("acting");

    ag.sendJson({ type: "action.result", request_id, task_id: id, ok: true });
    await waitFor(() => sentFor(id).some((d) => d.type === "chip_result"));
    expect(sentFor(id).filter((d) => d.type === "chip_result")).toEqual([
      { type: "chip_result", task_id: id, action: "start", ok: "true", error: "" },
    ]);
    expect(await getChip(id)).toMatchObject({ status: "done", error: null });

    // done は closed。
    const again = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "dismiss" }),
    });
    expect(again.status).toBe(409);
    expect(await again.json()).toEqual({ error: "chip_closed" });
  });

  it("withdrawn は 409 chip_closed", async () => {
    const id = uid();
    await postChip(id);
    await api(`/v1/chips/${id}`, { method: "DELETE" });
    await agent();
    const res = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "start" }),
    });
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({ error: "chip_closed" });
  });

  it("action.result ok=false は failed + error、failed からは再 action できる", async () => {
    const id = uid();
    await postChip(id);
    const ag = await agent();
    const res = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "dismiss" }),
    });
    const { request_id } = (await res.json()) as { request_id: string };
    ag.sendJson({ type: "action.result", request_id, task_id: id, ok: false, error: "button_not_found" });
    await waitFor(() => sentFor(id).some((d) => d.type === "chip_result"));
    expect(sentFor(id).find((d) => d.type === "chip_result")).toEqual({
      type: "chip_result",
      task_id: id,
      action: "dismiss",
      ok: "false",
      error: "button_not_found",
    });
    expect(await getChip(id)).toMatchObject({ status: "failed", error: "button_not_found" });

    const retry = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "start" }),
    });
    expect(retry.status).toBe(202);
    expect(await getChip(id)).toMatchObject({ status: "acting", error: null });
  });

  it("ok=false で error 省略なら error=unknown", async () => {
    const id = uid();
    await postChip(id);
    const ag = await agent();
    const res = await api(`/v1/chips/${id}/action`, {
      method: "POST",
      body: JSON.stringify({ action: "start" }),
    });
    const { request_id } = (await res.json()) as { request_id: string };
    ag.sendJson({ type: "action.result", request_id, task_id: id, ok: false });
    await waitFor(async () => (await getChip(id))?.status === "failed");
    expect((await getChip(id))?.error).toBe("unknown");
  });

  it("request_id が合わない action.result は無視する", async () => {
    const id = uid();
    await postChip(id);
    const ag = await agent();
    await api(`/v1/chips/${id}/action`, { method: "POST", body: JSON.stringify({ action: "start" }) });
    ag.sendJson({ type: "action.result", request_id: "stale", task_id: id, ok: true });
    // 後続メッセージが処理された = 前のメッセージも処理済み。
    ag.sendJson({ type: "chip.located", task_id: id });
    await waitFor(async () => (await getChip(id))?.located === true);
    expect((await getChip(id))?.status).toBe("acting");
    expect(sentFor(id).filter((d) => d.type === "chip_result")).toHaveLength(0);
  });

  it("agent が 30 秒応答しなければ alarm で failed / agent_timeout + chip_result", async () => {
    const id = uid();
    await postChip(id);
    await agent();
    const before = Date.now();
    await api(`/v1/chips/${id}/action`, { method: "POST", body: JSON.stringify({ action: "start" }) });

    const deadline = await runInDurableObject(hubStub(), (_i, state) =>
      state.storage.sql
        .exec<{ deadline_at: number; deadline_kind: string }>(
          `SELECT deadline_at, deadline_kind FROM chips WHERE task_id = ?`,
          id,
        )
        .one(),
    );
    expect(deadline.deadline_kind).toBe("action");
    expect(deadline.deadline_at).toBeGreaterThanOrEqual(before + 30_000 - 1000);

    await expireDeadline(id);
    expect(await getChip(id)).toMatchObject({ status: "failed", error: "agent_timeout" });
    expect(sentFor(id).filter((d) => d.type === "chip_result")).toEqual([
      { type: "chip_result", task_id: id, action: "start", ok: "false", error: "agent_timeout" },
    ]);
  });
});

describe("agent WebSocket", () => {
  it("接続直後に hello で open な chip を送る", async () => {
    const open = uid();
    const closed = uid();
    await postChip(open);
    await postChip(closed);
    await api(`/v1/chips/${closed}`, { method: "DELETE" });

    const ag = await agent();
    const hello = await ag.next("hello");
    const ids = (hello.chips as { task_id: string }[]).map((c) => c.task_id);
    expect(ids).toContain(open);
    expect(ids).not.toContain(closed);
  });

  it("agent 接続中の新規 chip は chip.new を送り、chip.located まで FCM を待つ", async () => {
    const ag = await agent();
    const id = uid();
    const res = await postChip(id);
    expect(res.status).toBe(201);
    expect(((await res.json()) as { chip: { status: string } }).chip.status).toBe("located_pending");

    const msg = await ag.next("chip.new", (m) => (m.chip as { task_id: string }).task_id === id);
    expect(msg.chip).toMatchObject({ task_id: id, status: "located_pending", located: false });
    expect(sentFor(id)).toHaveLength(0);

    ag.sendJson({ type: "chip.located", task_id: id, pane_title: "pane" });
    await waitFor(() => sentFor(id).length > 0);
    expect(sentFor(id)).toEqual([expect.objectContaining({ type: "chip", task_id: id, located: "true" })]);
    expect(await getChip(id)).toMatchObject({ status: "notified", located: true });

    // 2 回目の located は再通知しない。
    ag.sendJson({ type: "chip.located", task_id: id });
    ag.sendJson({ type: "chip.not_found", task_id: id });
    ag.sendJson({ type: "pong" });
    const probe = uid();
    await postChip(probe);
    ag.sendJson({ type: "chip.not_found", task_id: probe });
    await waitFor(() => sentFor(probe).length > 0);
    expect(sentFor(id)).toHaveLength(1);
  });

  it("chip.not_found は located=false で FCM 通知", async () => {
    const ag = await agent();
    const id = uid();
    await postChip(id);
    ag.sendJson({ type: "chip.not_found", task_id: id });
    await waitFor(() => sentFor(id).length > 0);
    expect(sentFor(id)).toEqual([expect.objectContaining({ type: "chip", located: "false" })]);
    expect(await getChip(id)).toMatchObject({ status: "notified", located: false });
  });

  it("located=false で通知済みの chip が後から見つかったら located=true で再通知", async () => {
    const id = uid();
    await postChip(id); // agent 未接続 → located=false で通知
    const ag = await agent();
    ag.sendJson({ type: "chip.located", task_id: id });
    await waitFor(() => sentFor(id).length === 2);
    expect(sentFor(id).map((d) => d.located)).toEqual(["false", "true"]);
    expect((await getChip(id))?.located).toBe(true);
  });

  it("acting 中の chip.located は located だけ更新して通知しない", async () => {
    const id = uid();
    await postChip(id);
    const ag = await agent();
    await api(`/v1/chips/${id}/action`, { method: "POST", body: JSON.stringify({ action: "start" }) });
    ag.sendJson({ type: "chip.located", task_id: id });
    await waitFor(async () => (await getChip(id))?.located === true);
    expect(sentFor(id).filter((d) => d.type === "chip")).toHaveLength(1);
  });

  it("withdrawn / 未知 chip への located は無視", async () => {
    const id = uid();
    await postChip(id);
    await api(`/v1/chips/${id}`, { method: "DELETE" });
    const ag = await agent();
    ag.sendJson({ type: "chip.located", task_id: id });
    ag.sendJson({ type: "chip.located", task_id: uid() });
    ag.sendJson({ type: "action.result", task_id: uid(), request_id: "x", ok: true });
    const probe = uid();
    await postChip(probe);
    ag.sendJson({ type: "chip.located", task_id: probe });
    await waitFor(() => sentFor(probe).length > 0);
    expect(await getChip(id)).toMatchObject({ status: "withdrawn", located: false });
  });

  it("agent が 75 秒報告しなければ alarm で located=false 通知 (安全網)", async () => {
    await agent();
    const id = uid();
    await postChip(id);
    const row = await runInDurableObject(hubStub(), (_i, state) =>
      state.storage.sql
        .exec<{ deadline_at: number; deadline_kind: string; created_at: number }>(
          `SELECT deadline_at, deadline_kind, created_at FROM chips WHERE task_id = ?`,
          id,
        )
        .one(),
    );
    expect(row.deadline_kind).toBe("locate");
    expect(row.deadline_at - row.created_at).toBe(75_000);

    await expireDeadline(id);
    expect(sentFor(id)).toEqual([expect.objectContaining({ type: "chip", located: "false" })]);
    expect(await getChip(id)).toMatchObject({ status: "notified", located: false });
  });

  it("deadline が全部消えたら alarm も消える", async () => {
    await agent();
    const id = uid();
    await postChip(id);
    await api(`/v1/chips/${id}`, { method: "DELETE" });
    // 他テストの残り deadline も全部片付ける。
    await runInDurableObject(hubStub(), (_i, state) => {
      state.storage.sql.exec(`UPDATE chips SET deadline_at = NULL, deadline_kind = NULL`);
    });
    const id2 = uid();
    await postChip(id2);
    await api(`/v1/chips/${id2}`, { method: "DELETE" });
    const alarm = await runInDurableObject(hubStub(), (_i, state) => state.storage.getAlarm());
    expect(alarm).toBeNull();
  });

  it("新しい agent が来たら古い方を close 4000 で追い出し、以後の送信は新しい方だけ", async () => {
    const a1 = await agent();
    const a2 = await agent();
    await waitFor(() => a1.closed);
    expect(a1.closed?.code).toBe(4000);

    const id = uid();
    await postChip(id);
    await a2.next("chip.new", (m) => (m.chip as { task_id: string }).task_id === id);
    expect(a1.msgs.some((m) => m.type === "chip.new")).toBe(false);

    const h = (await (await SELF.fetch(`${BASE}/health`)).json()) as { agent_connected: boolean };
    expect(h.agent_connected).toBe(true);
  });

  it("壊れた / 未知のメッセージは無視して接続を保つ", async () => {
    const ag = await agent();
    ag.ws.send("not json");
    ag.ws.send("42");
    ag.ws.send(new TextEncoder().encode(JSON.stringify({ type: "mystery" })));
    const id = uid();
    await postChip(id);
    ag.sendJson({ type: "chip.not_found", task_id: id });
    await waitFor(() => sentFor(id).length > 0);
    expect(ag.closed).toBeNull();
  });

  it("Upgrade ヘッダ無しは 426", async () => {
    const res = await api("/v1/agent/ws");
    expect(res.status).toBe(426);
    expect(await res.json()).toEqual({ error: "expected_websocket" });
  });

  it("DO に直接 Upgrade 無しで来ても 426", async () => {
    const res = await hubStub().fetch("https://hub/v1/agent/ws");
    expect(res.status).toBe(426);
  });

  it("token 無しの upgrade は 401", async () => {
    const res = await SELF.fetch(`${BASE}/v1/agent/ws`, { headers: { Upgrade: "websocket" } });
    expect(res.status).toBe(401);
  });
});

describe("POST /v1/devices", () => {
  it("upsert: 新規 201、同じ token は 200", async () => {
    const t = uid("tok");
    const r1 = await api("/v1/devices", { method: "POST", body: JSON.stringify({ fcm_token: t, name: "a" }) });
    expect(r1.status).toBe(201);
    expect(await r1.json()).toEqual({ ok: true });
    const r2 = await api("/v1/devices", { method: "POST", body: JSON.stringify({ fcm_token: t }) });
    expect(r2.status).toBe(200);
    const name = await runInDurableObject(hubStub(), (_i, state) =>
      state.storage.sql.exec<{ name: string }>(`SELECT name FROM devices WHERE fcm_token = ?`, t).one().name,
    );
    expect(name).toBe("");
    await runInDurableObject(hubStub(), (_i, state) => {
      state.storage.sql.exec(`DELETE FROM devices WHERE fcm_token = ?`, t);
    });
  });

  it.each([["{}"], [JSON.stringify({ fcm_token: "" })], [JSON.stringify({ fcm_token: "x", name: 3 })]])(
    "不正な body (%s) は 400",
    async (body) => {
      const res = await api("/v1/devices", { method: "POST", body });
      expect(res.status).toBe(400);
    },
  );

  it("FCM が 404 / UNREGISTERED を返した token は削除し、それ以外のエラーは残す", async () => {
    for (const t of ["dead-1", "gone-1", "bad-1"]) {
      await api("/v1/devices", { method: "POST", body: JSON.stringify({ fcm_token: t, name: t }) });
    }
    const id = uid();
    await postChip(id);
    for (const t of ["dead-1", "gone-1", "bad-1"]) expect(sentFor(id, t)).toHaveLength(1);
    expect(sentFor(id)).toHaveLength(1);

    const tokens = await runInDurableObject(hubStub(), (_i, state) =>
      state.storage.sql
        .exec<{ fcm_token: string }>(`SELECT fcm_token FROM devices ORDER BY fcm_token`)
        .toArray()
        .map((r) => r.fcm_token),
    );
    expect(tokens).toContain("bad-1");
    expect(tokens).toContain(MAIN_DEVICE);
    expect(tokens).not.toContain("dead-1");
    expect(tokens).not.toContain("gone-1");
    await runInDurableObject(hubStub(), (_i, state) => {
      state.storage.sql.exec(`DELETE FROM devices WHERE fcm_token = 'bad-1'`);
    });
  });

  it("access token は DO にキャッシュされ、送信ごとに取り直さない", async () => {
    const before = fcm().tokenRequests.length;
    await postChip(uid());
    await postChip(uid());
    expect(fcm().tokenRequests.length).toBe(before);
    expect(fcm().tokenRequests.length).toBeGreaterThanOrEqual(1);
    const req = fcm().tokenRequests[0];
    expect(req.get("grant_type")).toBe("urn:ietf:params:oauth:grant-type:jwt-bearer");
    expect(req.get("assertion")?.split(".")).toHaveLength(3);
  });

  it("device が 0 件なら送信しない", async () => {
    const saved = await runInDurableObject(hubStub(), (_i, state) => {
      const rows = state.storage.sql.exec<{ fcm_token: string; name: string }>(`SELECT fcm_token, name FROM devices`).toArray();
      state.storage.sql.exec(`DELETE FROM devices`);
      return rows;
    });
    const before = fcm().sent.length;
    await postChip(uid());
    expect(fcm().sent.length).toBe(before);
    for (const d of saved) {
      await api("/v1/devices", { method: "POST", body: JSON.stringify({ fcm_token: d.fcm_token, name: d.name }) });
    }
  });
});
