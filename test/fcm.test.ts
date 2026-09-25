import { describe, it, expect } from "vitest";
import { env, runInDurableObject } from "cloudflare:test";
import { CachedToken, FCM_SCOPE, FcmSender, TOKEN_URL, TokenCache, fcmConfig, pemToDer } from "../src/fcm";
import type { Env } from "../src/env";
import type { HubDO } from "../src/hub";
import { fcm, hubStub, uid } from "./helpers";

async function keyPair(): Promise<{ pem: string; publicKey: CryptoKey }> {
  const pair = (await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const der = new Uint8Array((await crypto.subtle.exportKey("pkcs8", pair.privateKey)) as ArrayBuffer);
  let bin = "";
  for (const b of der) bin += String.fromCharCode(b);
  // service account JSON から貼ったときのように "\n" エスケープで改行を表す。
  const pem = `-----BEGIN PRIVATE KEY-----\\n${btoa(bin).replace(/(.{64})/g, "$1\\n")}\\n-----END PRIVATE KEY-----\\n`;
  return { pem, publicKey: pair.publicKey };
}

function memCache(initial: CachedToken | null = null): TokenCache & { value: CachedToken | null } {
  const c = {
    value: initial,
    get: () => c.value,
    set: (t: CachedToken | null) => {
      c.value = t;
    },
  };
  return c;
}

function b64urlDecode(s: string): Uint8Array {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4);
  return Uint8Array.from(atob(b64), (ch) => ch.charCodeAt(0));
}

type Call = { url: string; init: RequestInit };

function scripted(responses: Array<Response | Error>): { calls: Call[]; fn: typeof fetch } {
  const calls: Call[] = [];
  const fn = (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init: init ?? {} });
    const r = responses.shift();
    if (!r) throw new Error("no more scripted responses");
    if (r instanceof Error) throw r;
    return r;
  }) as typeof fetch;
  return { calls, fn };
}

const tokenOk = () => Response.json({ access_token: "at-1", expires_in: 3600 });

describe("fcmConfig", () => {
  it("client email / private key が無ければ null", () => {
    expect(fcmConfig({ ...env, FCM_PRIVATE_KEY: "" })).toBeNull();
    expect(fcmConfig({ ...env, FCM_CLIENT_EMAIL: " " })).toBeNull();
  });

  it("project id は既定 alc-fcm", () => {
    const cfg = fcmConfig({ ...env, FCM_PROJECT_ID: undefined } as Env);
    expect(cfg?.projectId).toBe("alc-fcm");
    expect(fcmConfig({ ...env, FCM_PROJECT_ID: "other" })?.projectId).toBe("other");
  });
});

describe("pemToDer", () => {
  it("実改行 / \\n エスケープのどちらでも同じ DER", () => {
    const pem = "-----BEGIN PRIVATE KEY-----\nAAEC\nAwQ=\n-----END PRIVATE KEY-----\n";
    expect(pemToDer(pem)).toEqual(new Uint8Array([0, 1, 2, 3, 4]));
    expect(pemToDer(pem.replace(/\n/g, "\\n"))).toEqual(new Uint8Array([0, 1, 2, 3, 4]));
  });
});

describe("FcmSender", () => {
  it("RS256 service account JWT を作り、公開鍵で検証できる", async () => {
    const { pem, publicKey } = await keyPair();
    const sender = new FcmSender(
      { projectId: "p", clientEmail: "sa@p.iam.gserviceaccount.com", privateKeyPem: pem },
      memCache(),
    );
    const jwt = await sender.signAssertion(1_700_000_000_000);
    const [h, c, s] = jwt.split(".");
    expect(JSON.parse(new TextDecoder().decode(b64urlDecode(h)))).toEqual({ alg: "RS256", typ: "JWT" });
    expect(JSON.parse(new TextDecoder().decode(b64urlDecode(c)))).toEqual({
      iss: "sa@p.iam.gserviceaccount.com",
      scope: FCM_SCOPE,
      aud: TOKEN_URL,
      iat: 1_700_000_000,
      exp: 1_700_003_600,
    });
    const ok = await crypto.subtle.verify(
      "RSASSA-PKCS1-v1_5",
      publicKey,
      b64urlDecode(s),
      new TextEncoder().encode(`${h}.${c}`),
    );
    expect(ok).toBe(true);
  });

  it("data-only + android.priority=high で送り、access token をキャッシュする", async () => {
    const { pem } = await keyPair();
    const cache = memCache();
    const { calls, fn } = scripted([tokenOk(), Response.json({}), Response.json({})]);
    const sender = new FcmSender({ projectId: "alc-fcm", clientEmail: "sa@x", privateKeyPem: pem }, cache, fn);

    expect(await sender.send("dev", { type: "chip_cancel", task_id: "t" })).toBe("ok");
    expect(await sender.send("dev", { type: "chip_cancel", task_id: "t2" })).toBe("ok");
    expect(calls.map((c) => c.url)).toEqual([
      TOKEN_URL,
      "https://fcm.googleapis.com/v1/projects/alc-fcm/messages:send",
      "https://fcm.googleapis.com/v1/projects/alc-fcm/messages:send",
    ]);
    const tokenBody = new URLSearchParams(calls[0].init.body as string);
    expect(tokenBody.get("grant_type")).toBe("urn:ietf:params:oauth:grant-type:jwt-bearer");
    expect(new Headers(calls[1].init.headers).get("Authorization")).toBe("Bearer at-1");
    expect(JSON.parse(calls[1].init.body as string)).toEqual({
      message: { token: "dev", data: { type: "chip_cancel", task_id: "t" }, android: { priority: "high" } },
    });
    expect(cache.value?.access_token).toBe("at-1");
    expect(cache.value!.expires_at).toBeGreaterThan(Date.now() + 3500_000);
  });

  it("期限 5 分前を切ったキャッシュは取り直す", async () => {
    const { pem } = await keyPair();
    const cache = memCache({ access_token: "old", expires_at: Date.now() + 4 * 60 * 1000 });
    const { calls, fn } = scripted([Response.json({ access_token: "new" }), Response.json({})]);
    const sender = new FcmSender({ projectId: "p", clientEmail: "sa@x", privateKeyPem: pem }, cache, fn);
    expect(await sender.send("dev", {})).toBe("ok");
    expect(calls[0].url).toBe(TOKEN_URL);
    expect(cache.value?.access_token).toBe("new");
  });

  it("有効なキャッシュがあれば token endpoint を叩かない", async () => {
    const cache = memCache({ access_token: "cached", expires_at: Date.now() + 3600_000 });
    const { calls, fn } = scripted([Response.json({})]);
    const sender = new FcmSender({ projectId: "p", clientEmail: "sa@x", privateKeyPem: "unused" }, cache, fn);
    expect(await sender.send("dev", {})).toBe("ok");
    expect(calls).toHaveLength(1);
  });

  it("404 / UNREGISTERED は unregistered、その他は error", async () => {
    const cache = memCache({ access_token: "cached", expires_at: Date.now() + 3600_000 });
    const { fn } = scripted([
      new Response("", { status: 404 }),
      Response.json({ error: { details: [{ errorCode: "UNREGISTERED" }] } }, { status: 400 }),
      Response.json({ error: { details: [{ errorCode: "INVALID_ARGUMENT" }] } }, { status: 400 }),
      new Response("not json", { status: 500 }),
    ]);
    const sender = new FcmSender({ projectId: "p", clientEmail: "sa@x", privateKeyPem: "unused" }, cache, fn);
    expect(await sender.send("a", {})).toBe("unregistered");
    expect(await sender.send("b", {})).toBe("unregistered");
    expect(await sender.send("c", {})).toBe("error");
    expect(await sender.send("d", {})).toBe("error");
  });

  it("401 はキャッシュを捨てる", async () => {
    const cache = memCache({ access_token: "revoked", expires_at: Date.now() + 3600_000 });
    const { fn } = scripted([new Response("", { status: 401 })]);
    const sender = new FcmSender({ projectId: "p", clientEmail: "sa@x", privateKeyPem: "unused" }, cache, fn);
    expect(await sender.send("a", {})).toBe("error");
    expect(cache.value).toBeNull();
  });

  it("token 交換の失敗 / access_token 欠落 / ネットワーク例外は error (例外を投げない)", async () => {
    const { pem } = await keyPair();
    const { fn } = scripted([
      new Response("denied", { status: 400 }),
      Response.json({}),
      new Error("network down"),
    ]);
    const sender = new FcmSender({ projectId: "p", clientEmail: "sa@x", privateKeyPem: pem }, memCache(), fn);
    expect(await sender.send("a", {})).toBe("error");
    expect(await sender.send("a", {})).toBe("error");
    expect(await sender.send("a", {})).toBe("error");
  });

  it("壊れた private key は error、次回は import をやり直す", async () => {
    const { fn } = scripted([]);
    const sender = new FcmSender({ projectId: "p", clientEmail: "sa@x", privateKeyPem: "AAAA" }, memCache(), fn);
    expect(await sender.send("a", {})).toBe("error");
    expect(await sender.send("a", {})).toBe("error");
  });

  it("既定の fetch は globalThis.fetch (テストスタブ) を使う", async () => {
    const { pem } = await keyPair();
    const before = fcm().sent.length;
    const sender = new FcmSender({ projectId: "alc-fcm", clientEmail: "sa@x", privateKeyPem: pem }, memCache());
    expect(await sender.send("stub-dev", { type: "chip_cancel", task_id: "x" })).toBe("ok");
    expect(fcm().sent.length).toBe(before + 1);
  });
});

describe("HubDO: FCM 未設定", () => {
  it("log してスキップし、chip の処理自体は続く", async () => {
    const id = uid();
    const before = fcm().sent.length;
    const chip = await runInDurableObject(hubStub(), async (instance: HubDO) => {
      const target = instance as unknown as { env: Env; sender: unknown };
      const saved = { env: target.env, sender: target.sender };
      target.env = { ...saved.env, FCM_PRIVATE_KEY: "" };
      target.sender = null;
      try {
        return (
          await instance.createChip({ task_id: id, title: "t", tldr: "", cwd: "", host: "", session_id: null })
        ).chip;
      } finally {
        target.env = saved.env;
        target.sender = saved.sender;
      }
    });
    expect(chip.status).toBe("notified");
    expect(fcm().sent.length).toBe(before);
  });
});
