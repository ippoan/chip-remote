/**
 * 全テストファイル共通の前処理。
 *
 * 1. 外向き fetch (Google OAuth / FCM) を握りつぶすスタブを入れる。
 *    vitest-pool-workers では Worker / HubDO がテストと同じ isolate で動くので、
 *    globalThis.fetch を差し替えれば DO からの送信も捕まえられる。呼び出しは
 *    globalThis.__fcm に記録し、test/helpers.ts から参照する。alarm 等で後から走る
 *    送信も実ネットワークに出ないよう、差し替えは戻さない。
 * 2. テストファイルが変わると main module の再 import で既存 DO が invalidate される
 *    (vitest-pool-workers の仕様、"... changed, invalidating this Durable Object")。
 *    最初の 1 回を捨て打ちして新しい instance を作らせる。
 */
import { beforeAll } from "vitest";
import { hubStub, installFcmStub } from "./helpers";

installFcmStub();

beforeAll(async () => {
  try {
    await hubStub().agentConnected();
  } catch {
    // invalidated — 次の呼び出しで新しい instance になる
  }
});
