import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

// Inline draws with the runtimes the app bundles from node_modules; the preview
// draws with the copies vendored under src-tauri/assets. The two must be the
// same files, or a block reads one way inline and another in the preview.
const root = process.cwd();

function bytesOf(path: string): Buffer {
  return readFileSync(resolve(root, path));
}

function versionOf(pkg: string): string {
  const manifest = JSON.parse(readFileSync(resolve(root, "node_modules", pkg, "package.json"), "utf8")) as {
    version: string;
  };
  return manifest.version;
}

describe("the vendored renderer runtimes", () => {
  it.each([
    ["src-tauri/assets/mermaid/mermaid.min.js", "node_modules/mermaid/dist/mermaid.min.js"],
    ["src-tauri/assets/katex/katex.min.js", "node_modules/katex/dist/katex.min.js"],
    ["src-tauri/assets/katex/katex.min.css", "node_modules/katex/dist/katex.min.css"],
  ])("%s is the installed %s", (vendored, installed) => {
    expect(bytesOf(vendored).equals(bytesOf(installed))).toBe(true);
  });

  it("vendors each KaTeX font as the installed package ships it", () => {
    const fonts = readdirSync(resolve(root, "src-tauri/assets/katex/fonts"));
    expect(fonts.length).toBeGreaterThan(0);
    for (const font of fonts) {
      const same = bytesOf(`src-tauri/assets/katex/fonts/${font}`).equals(bytesOf(`node_modules/katex/dist/fonts/${font}`));
      expect(same, font).toBe(true);
    }
  });

  it("names the installed versions in the vendored files", () => {
    expect(bytesOf("src-tauri/assets/mermaid/mermaid.min.js").toString("utf8")).toContain(`version:"${versionOf("mermaid")}"`);
    expect(bytesOf("src-tauri/assets/katex/katex.min.js").toString("utf8")).toContain(`version:"${versionOf("katex")}"`);
  });
});
