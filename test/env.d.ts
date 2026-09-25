import type { Env } from "../src/env";

declare module "cloudflare:test" {
  // vitest.config.ts の miniflare bindings が満たす。
  interface ProvidedEnv extends Env {}
}
