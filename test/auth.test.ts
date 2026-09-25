import { describe, it, expect } from "vitest";
import { SELF, env } from "cloudflare:test";
import { app } from "../src/index";
import { timingSafeEqual } from "../src/auth";
import { BASE, TOKEN } from "./helpers";

describe("Bearer 認証", () => {
  it("Authorization 無しは 401", async () => {
    const res = await SELF.fetch(`${BASE}/v1/chips`);
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "unauthorized" });
  });

  it("token 不一致は 401", async () => {
    const res = await SELF.fetch(`${BASE}/v1/chips`, { headers: { Authorization: "Bearer wrong" } });
    expect(res.status).toBe(401);
  });

  it("Bearer 以外の scheme は 401", async () => {
    const res = await SELF.fetch(`${BASE}/v1/chips`, { headers: { Authorization: `Basic ${TOKEN}` } });
    expect(res.status).toBe(401);
  });

  it("正しい token は通る", async () => {
    const res = await SELF.fetch(`${BASE}/v1/chips`, { headers: { Authorization: `Bearer ${TOKEN}` } });
    expect(res.status).toBe(200);
  });

  it("未知パスも認証が先 (401)、認証後は 404", async () => {
    expect((await SELF.fetch(`${BASE}/nope`)).status).toBe(401);
    const res = await SELF.fetch(`${BASE}/nope`, { headers: { Authorization: `Bearer ${TOKEN}` } });
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "not_found" });
  });

  it("CHIP_REMOTE_TOKEN 未設定なら 503 (fail-closed)、/health は通る", async () => {
    const noToken = { ...env, CHIP_REMOTE_TOKEN: "" };
    const res = await app.request(`${BASE}/v1/chips`, { headers: { Authorization: "Bearer x" } }, noToken);
    expect(res.status).toBe(503);
    expect(await res.json()).toEqual({ error: "token_not_configured" });
    const health = await app.request(`${BASE}/health`, {}, noToken);
    expect(health.status).toBe(200);
  });

  it("DO 側の例外は 500 internal", async () => {
    const broken = {
      ...env,
      HUB: {
        idFromName: () => {
          throw new Error("boom");
        },
      },
    };
    const res = await app.request(`${BASE}/health`, {}, broken);
    expect(res.status).toBe(500);
    expect(await res.json()).toEqual({ error: "internal" });
  });
});

describe("timingSafeEqual", () => {
  it("一致 / 不一致 / 長さ違い", async () => {
    expect(await timingSafeEqual("abc", "abc")).toBe(true);
    expect(await timingSafeEqual("abc", "abd")).toBe(false);
    expect(await timingSafeEqual("abc", "abcd")).toBe(false);
  });
});
