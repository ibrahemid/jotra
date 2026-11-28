import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// The app CSP's connect-src names only the IPC origins. Vite's module preload
// polyfill fetches each chunk it preloads, which that policy refuses on a
// webview with no native modulepreload, so the build leaves it out.
describe("the build's module preload", () => {
  it("has no polyfill", () => {
    const config = readFileSync(resolve(process.cwd(), "vite.config.ts"), "utf8");
    const build = /\n {2}build: \{\n([\s\S]*?)\n {2}\},\n/.exec(config)?.[1] ?? "";
    expect(build).toMatch(/\n {4}modulePreload: \{ polyfill: false \},/);
  });
});
