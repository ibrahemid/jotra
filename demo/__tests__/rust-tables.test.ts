import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { MCP_TOOLS, PROVIDERS } from "../backend/ai";
import { MAX_ATTACHED_NOTES, MISSING_CONVERSATION } from "../backend/commands/ai-chat";
import {
  ERR_NOT_A_NOTE,
  NAME_IS_EMPTY,
  NOTHING_TO_PUT_BACK,
  NOT_YOURS_TO_DELETE,
  TRASH_UNDO_MAX_BYTES,
} from "../backend/commands/buffers";
import { DEFAULT_CAPTURE_CHORD } from "../backend/commands/config";
import {
  BINARY_CHECK_BYTES,
  ERR_OPEN_TOO_LARGE,
  HEX_DUMP_MAX_BYTES,
  THRESHOLD_LARGE_BYTES,
  THRESHOLD_MAX_BYTES,
  THRESHOLD_NORMAL_BYTES,
} from "../backend/classify";

// The page answers from copies of Rust tables and refusals. These hold the
// copies to the source, so a provider, a tool or a sentence changed in Rust
// fails here rather than leaving the page describing an older app.
const ROOT = process.cwd();
const rust = (path: string) => readFileSync(join(ROOT, path), "utf8");

function fields(source: string, name: string): string[] {
  return [...source.matchAll(new RegExp(`\\b${name}: "([^"]*)"`, "g"))].map((match) => match[1]);
}

function list(source: string, constant: string): string[] {
  const body = source.slice(source.indexOf(`pub const ${constant}`));
  const array = body.slice(body.indexOf("["), body.indexOf("];") + 1);
  return [...array.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

/** The `PROVIDERS` literal alone, so nothing in the file's tests is read as a row. */
function providerTable(source: string): string {
  const start = source.indexOf("pub const PROVIDERS");
  const end = source.indexOf("\n];", start);
  if (start === -1 || end === -1) throw new Error("providers.rs no longer declares PROVIDERS as a literal");
  return source.slice(start, end);
}

function stringConstant(source: string, name: string): string | undefined {
  return source.match(new RegExp(`const ${name}: &str = "([^"]*)";`))?.[1];
}

function numberConstant(source: string, name: string): number | undefined {
  const value = source.match(new RegExp(`const ${name}: usize = (\\d+);`))?.[1];
  return value === undefined ? undefined : Number(value);
}

/** A `u64` or `usize` constant written as a product of literals, such as `5 * 1024 * 1024`. */
function productConstant(source: string, name: string): number | undefined {
  const value = source.match(new RegExp(`const ${name}: (?:u64|usize) = ([\\d\\s*]+);`))?.[1];
  return value === undefined ? undefined : value.split("*").reduce((product, factor) => product * Number(factor.trim()), 1);
}

describe("the demo's copies of Rust tables", () => {
  const providers = providerTable(rust("crates/jotra-core/src/ai/providers.rs"));

  it("lists the providers jotra_core::ai::providers lists, in its order", () => {
    expect(PROVIDERS.map((row) => row.id)).toEqual(fields(providers, "id"));
  });

  it("carries each provider's endpoint and default model", () => {
    const ids = fields(providers, "id");
    const urls = fields(providers, "base_url");
    const models = fields(providers, "default_model");
    expect(ids.length).toBeGreaterThan(0);
    expect(PROVIDERS.map((row) => row.base_url)).toEqual(urls);
    expect(PROVIDERS.map((row) => row.default_model)).toEqual(models);
  });

  it("carries each provider's suggested models and whether it needs a key", () => {
    const curated = [...providers.matchAll(/curated_models: &\[([^\]]*)\]/g)].map((match) =>
      [...match[1].matchAll(/"([^"]*)"/g)].map((model) => model[1]),
    );
    const needsKey = [...providers.matchAll(/needs_key: (true|false)/g)].map((match) => match[1] === "true");
    expect(curated).toHaveLength(PROVIDERS.length);
    expect(needsKey).toHaveLength(PROVIDERS.length);
    expect(PROVIDERS.map((row) => [...row.curated_models])).toEqual(curated);
    expect(PROVIDERS.map((row) => row.needs_key)).toEqual(needsKey);
  });

  it("offers the tools jotra_core::tools offers", () => {
    const tools = rust("crates/jotra-core/src/tools.rs");
    expect(MCP_TOOLS.read).toEqual(list(tools, "READ_TOOLS"));
    expect(MCP_TOOLS.write).toEqual(list(tools, "WRITE_TOOLS"));
    expect(MCP_TOOLS.context).toEqual(list(tools, "CONTEXT_TOOLS"));
  });

  it("opens in the tiers jotra_core::file_ops sets", () => {
    const fileOps = rust("crates/jotra-core/src/file_ops.rs");
    expect(productConstant(fileOps, "THRESHOLD_NORMAL_BYTES")).toBe(THRESHOLD_NORMAL_BYTES);
    expect(productConstant(fileOps, "THRESHOLD_LARGE_BYTES")).toBe(THRESHOLD_LARGE_BYTES);
    expect(productConstant(fileOps, "THRESHOLD_MAX_BYTES")).toBe(THRESHOLD_MAX_BYTES);
    expect(productConstant(fileOps, "BINARY_CHECK_BYTES")).toBe(BINARY_CHECK_BYTES);
    expect(productConstant(fileOps, "HEX_DUMP_MAX_BYTES")).toBe(HEX_DUMP_MAX_BYTES);
    expect(stringConstant(fileOps, "ERR_OPEN_TOO_LARGE")).toBe(ERR_OPEN_TOO_LARGE);
  });

  it("refuses in the sentences the Rust commands use", () => {
    const notes = rust("crates/jotra-core/src/notes/mod.rs");
    const chat = rust("crates/jotra-ai/src/chat.rs");
    const noteCommands = rust("src-tauri/src/commands/notes.rs");
    expect(stringConstant(notes, "NAME_IS_EMPTY")).toBe(NAME_IS_EMPTY);
    expect(stringConstant(chat, "MISSING_CONVERSATION")).toBe(MISSING_CONVERSATION);
    expect(numberConstant(chat, "MAX_ATTACHED_NOTES")).toBe(MAX_ATTACHED_NOTES);
    expect(stringConstant(noteCommands, "ERR_NOT_A_NOTE")).toBe(ERR_NOT_A_NOTE);
    expect(stringConstant(noteCommands, "NOT_YOURS_TO_DELETE")).toBe(NOT_YOURS_TO_DELETE);
    expect(stringConstant(noteCommands, "NOTHING_TO_PUT_BACK")).toBe(NOTHING_TO_PUT_BACK);
    expect(productConstant(noteCommands, "TRASH_UNDO_MAX_BYTES")).toBe(TRASH_UNDO_MAX_BYTES);
  });

  it("registers the quick capture chord jotra_core::config sets", () => {
    const config = rust("crates/jotra-core/src/config/mod.rs");
    expect(config.match(/fn default_hotkey_capture\(\) -> String \{\s*"([^"]+)"/)?.[1]).toBe(DEFAULT_CAPTURE_CHORD);
  });
});
