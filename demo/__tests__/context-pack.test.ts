import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import type { IpcBridge } from "../ipc";
import { createBackend } from "../backend/backend";
import { MAX_HOST_NOTE_BYTES, MAX_PACK_BYTES, MAX_PACK_FILES, PackBuilder, estimatePromptTokens, stripForPrompt } from "../backend/context-pack";
import { NOTES_ROOT } from "../backend/vfs";
import type { ContextPack } from "../../src/services/tauri";
import type { BufferDocument } from "../../src/types/buffer";

const ROOT = process.cwd();
const rust = (path: string) => readFileSync(join(ROOT, path), "utf8");

function productConstant(source: string, name: string): number | undefined {
  const value = source.match(new RegExp(`const ${name}: (?:u64|usize) = ([\\d\\s*]+);`))?.[1];
  return value === undefined ? undefined : value.split("*").reduce((product, factor) => product * Number(factor.trim()), 1);
}

function backend() {
  const bridge: IpcBridge = { emit: () => {}, send: () => {}, end: () => {} };
  return createBackend(bridge).handle;
}

type Handle = ReturnType<typeof backend>;

async function writeNote(handle: Handle, target: string, content: string): Promise<string> {
  const doc = (await handle("new_note_from_link", { target })) as BufferDocument;
  await handle("save_buffer_content", { id: doc.id, content });
  return doc.source_path as string;
}

describe("the demo's context pack", () => {
  it("keeps the limits jotra_core sets", () => {
    expect(productConstant(rust("crates/jotra-core/src/prompt/pack.rs"), "MAX_PACK_FILES")).toBe(MAX_PACK_FILES);
    expect(productConstant(rust("crates/jotra-core/src/prompt/pack.rs"), "MAX_PACK_BYTES")).toBe(MAX_PACK_BYTES);
    expect(productConstant(rust("crates/jotra-core/src/notes/host/mod.rs"), "MAX_NOTE_BYTES")).toBe(MAX_HOST_NOTE_BYTES);
  });

  it("prepares a file the way jotra_core::prompt::strip_for_prompt does", () => {
    const cases: [string, string][] = [
      ["hello world", "hello world\n"],
      ["", ""],
      ["   \n\t\n  \n", ""],
      ["---\ntitle: Test\ntags: [a, b]\n---\nBody text\n", "Body text\n"],
      ["---\ntitle: Test\n---\n", ""],
      ["---\ntitle: Test\nBody keeps going\n", "---\ntitle: Test\nBody keeps going\n"],
      ["intro\n---\nnot frontmatter\n---\nrest\n", "intro\n---\nnot frontmatter\n---\nrest\n"],
      ["---\r\ntitle: Test\r\n---\r\nBody\r\n", "Body\n"],
      ["before <!-- note --> after", "before  after\n"],
      ["keep\n<!-- a note\nthat spans lines -->\nalso keep\n", "keep\n\nalso keep\n"],
      ["```html\n<!-- keep me -->\n```\n", "```html\n<!-- keep me -->\n```\n"],
      ["~~~\n<!-- keep me -->\n~~~\n", "~~~\n<!-- keep me -->\n~~~\n"],
      ["<!-- drop -->\n```\n<!-- keep -->\n```\n<!-- drop too -->\n", "\n```\n<!-- keep -->\n```\n"],
      ["text <!-- never closed\nmore text\n", "text <!-- never closed\nmore text\n"],
      ["a  \nb\t\nc", "a\nb\nc\n"],
      ["body\n\n\n\n", "body\n"],
      ["---\ndraft: true\n---\n<!-- todo: tighten -->\nThe prompt body.\n", "\nThe prompt body.\n"],
    ];
    for (const [input, prepared] of cases) expect(stripForPrompt(input), JSON.stringify(input)).toBe(prepared);
  });

  it("estimates the tokens jotra_core::prompt::estimate_tokens gives", () => {
    const fixture = JSON.parse(rust("crates/jotra-core/tests/fixtures/token-estimates.json")) as {
      cases: { name: string; text: string; tokens: number }[];
    };
    expect(fixture.cases.length).toBeGreaterThan(0);
    for (const row of fixture.cases) expect(estimatePromptTokens(row.text), row.name).toBe(row.tokens);
  });

  it("wraps each file in an element named by its path, as jotra_core::prompt::pack does", () => {
    const pack = new PackBuilder();
    pack.add("plan.md", "# Plan\nShip it.\n");
    pack.add("notes/meeting.md", "Agenda\n");
    pack.add("say \"hi\" <now>.md", "---\ntitle: A\n---\nBody <!-- aside -->\n");
    pack.add("empty.md", "");
    pack.skip("photo.png", "not_text");
    const built = pack.finish();
    expect(built.text).toBe(
      '<file path="plan.md">\n# Plan\nShip it.\n</file>\n\n' +
        '<file path="notes/meeting.md">\nAgenda\n</file>\n\n' +
        '<file path="say &quot;hi&quot; &lt;now>.md">\nBody\n</file>\n\n' +
        '<file path="empty.md">\n</file>',
    );
    expect(built.files.map((file) => file.path)).toEqual(["plan.md", "notes/meeting.md", 'say "hi" <now>.md', "empty.md"]);
    expect(built.tokens).toBe(estimatePromptTokens(built.text));
    expect(built.skipped).toEqual([{ path: "photo.png", reason: "not_text" }]);
  });

  it("stops at the file limit and at the byte limit, counting what it leaves out", () => {
    const byFiles = new PackBuilder();
    for (let n = 0; n < MAX_PACK_FILES + 3; n++) byFiles.add(`${n}.md`, "x\n");
    expect(byFiles.isFull()).toBe(true);
    expect(byFiles.finish()).toMatchObject({ omitted: 3, files: { length: MAX_PACK_FILES } });

    const big = "a ".repeat(MAX_PACK_BYTES / 4);
    const byBytes = new PackBuilder();
    byBytes.add("first.md", big);
    byBytes.add("second.md", big);
    byBytes.add("small.md", "x\n");
    const built = byBytes.finish();
    expect(built.files).toHaveLength(1);
    expect(built.omitted).toBe(2);
    expect(new TextEncoder().encode(built.text).length).toBeLessThanOrEqual(MAX_PACK_BYTES);
  });

  // Before the demo answered it, prompt_context_pack threw DemoCommandError
  // and the Copy as context dialog showed that the files could not be packed.
  it("packs a note with the notes it links to, a tag and a folder, with an open tab's text", async () => {
    const handle = backend();
    const plan = await writeNote(handle, "Pack/Plan.md", "See [[Pack/Budget]] and [[Nowhere]].\n#pack/q4\n");
    await writeNote(handle, "Pack/Budget.md", "Seeds: 40\n#pack\n");

    const linked = (await handle("prompt_context_pack", {
      source: { kind: "linked", path: plan },
      live: [{ path: `${NOTES_ROOT}/Pack/Budget.md`, text: "Seeds: 55\n" }],
    })) as ContextPack;
    expect(linked.files.map((file) => file.path)).toEqual(["Pack/Plan.md", "Pack/Budget.md"]);
    expect(linked.text).toContain('<file path="Pack/Budget.md">\nSeeds: 55\n</file>');
    expect(linked).toMatchObject({ skipped: [], omitted: 0 });

    const tagged = (await handle("prompt_context_pack", { source: { kind: "tag", tag: " #pack" }, live: [] })) as ContextPack;
    expect(tagged.files.map((file) => file.path)).toEqual(["Pack/Budget.md", "Pack/Plan.md"]);

    const folder = (await handle("prompt_context_pack", {
      source: { kind: "folder", path: `${NOTES_ROOT}/Pack/` },
      live: [],
    })) as ContextPack;
    expect(folder.files.map((file) => file.path)).toEqual(["Pack/Budget.md", "Pack/Plan.md"]);
    expect(await handle("prompt_context_pack", { source: { kind: "tag", tag: "#" }, live: [] })).toMatchObject({
      text: "",
      files: [],
    });
  });

  it("refuses a file or folder outside the notes folder in the words the command uses", async () => {
    const handle = backend();
    await expect(handle("prompt_context_pack", { source: { kind: "folder", path: "/Users/you/Desktop" }, live: [] })).rejects.toBe(
      "/Users/you/Desktop is outside the folder",
    );
    await expect(handle("prompt_context_pack", { source: { kind: "linked", path: NOTES_ROOT }, live: [] })).rejects.toBe(
      `${NOTES_ROOT} is outside the folder`,
    );
  });

  it("lists a note too large to read and a note with no file as skipped, and drops a link to a trashed note", async () => {
    const handle = backend();
    const plan = await writeNote(handle, "Big/Plan.md", "[[Big/Huge]] [[Big/Gone]]\n");
    await writeNote(handle, "Big/Huge.md", "x\n");
    const gone = (await handle("open_file", { path: await writeNote(handle, "Big/Gone.md", "y\n") })) as { doc: BufferDocument };
    await handle("delete_note", { id: gone.doc.id });
    const pack = (await handle("prompt_context_pack", {
      source: { kind: "linked", path: plan },
      live: [{ path: `${NOTES_ROOT}/Big/Huge.md`, text: "z".repeat(MAX_HOST_NOTE_BYTES + 1) }],
    })) as ContextPack;
    expect(pack.files.map((file) => file.path)).toEqual(["Big/Plan.md"]);
    expect(pack.skipped).toEqual([{ path: "Big/Huge.md", reason: "too_large" }]);

    const missing = (await handle("prompt_context_pack", {
      source: { kind: "linked", path: `${NOTES_ROOT}/Big/Missing.md` },
      live: [],
    })) as ContextPack;
    expect(missing).toMatchObject({ files: [], skipped: [{ path: "Big/Missing.md", reason: "unreadable" }] });
  });
});
