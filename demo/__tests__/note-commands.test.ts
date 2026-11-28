import { describe, expect, it } from "vitest";
import type { IpcBridge } from "../ipc";
import { createBackend } from "../backend/backend";
import { DEFAULT_CAPTURE_CHORD } from "../backend/commands/config";
import { TRASH_UNDO_MAX_BYTES } from "../backend/commands/buffers";
import { NOTES_ROOT } from "../backend/vfs";
import type { BufferDocument } from "../../src/types/buffer";
import type { DirListing } from "../../src/types/workspace";

function backend() {
  const emitted: { event: string; payload: unknown }[] = [];
  const bridge: IpcBridge = { emit: (event, payload) => emitted.push({ event, payload }), send: () => {}, end: () => {} };
  return { handle: createBackend(bridge).handle, emitted };
}

type Handle = ReturnType<typeof backend>["handle"];

async function writeNote(handle: Handle, target: string, content: string): Promise<BufferDocument> {
  const doc = (await handle("new_note_from_link", { target })) as BufferDocument;
  await handle("save_buffer_content", { id: doc.id, content });
  return (await handle("get_buffer", { id: doc.id })) as BufferDocument;
}

const names = async (handle: Handle, dir: string) =>
  ((await handle("list_notes_dir", { dirPath: dir })) as DirListing).entries.map((entry) => entry.name);

// Before the demo answered them, each of these commands threw
// DemoCommandError and the page logged it.
describe("the demo's note commands", () => {
  it("creates a new note in a folder of the notes folder, and refuses any other path", async () => {
    const { handle } = backend();
    await writeNote(handle, "Plots/Beds.md", "Beds\n");
    const made = (await handle("new_note_in_folder", { folder: `${NOTES_ROOT}/Plots/` })) as BufferDocument;
    expect(made.source_path?.startsWith(`${NOTES_ROOT}/Plots/`)).toBe(true);
    expect(await names(handle, `${NOTES_ROOT}/Plots`)).toContain(made.filename);
    await handle("save_buffer_content", { id: made.id, content: "Compost rota\n" });
    expect(await handle("auto_retitle_note", { id: made.id })).toMatchObject({ kind: "renamed" });

    const atRoot = (await handle("new_note_in_folder", { folder: NOTES_ROOT })) as BufferDocument;
    expect(atRoot.source_path?.slice(0, atRoot.source_path.lastIndexOf("/"))).toBe(NOTES_ROOT);

    for (const folder of [`${NOTES_ROOT}/No such folder`, `${NOTES_ROOT}/Plots/../..`, "/Users/you/Desktop"]) {
      await expect(handle("new_note_in_folder", { folder })).rejects.toBe("ERR_NOT_A_NOTE");
    }
  });

  it("puts back the note the last Move to Trash took, once", async () => {
    const { handle } = backend();
    const first = await writeNote(handle, "Trash/First.md", "first\n");
    const second = await writeNote(handle, "Trash/Second.md", "second\n");
    const firstToken = (await handle("delete_note", { id: first.id })) as string;
    const secondToken = (await handle("delete_note", { id: second.id })) as string;
    expect(typeof firstToken).toBe("string");
    expect(await names(handle, `${NOTES_ROOT}/Trash`)).toEqual([]);

    await expect(handle("undo_delete_note", { token: firstToken })).rejects.toBe("There is nothing to put back.");
    expect(await handle("undo_delete_note", { token: secondToken })).toBe(second.source_path);
    expect(await names(handle, `${NOTES_ROOT}/Trash`)).toEqual(["Second.md"]);
    const reopened = (await handle("open_file", { path: second.source_path })) as { doc: BufferDocument };
    expect(new TextDecoder().decode((await handle("read_buffer_content", { id: reopened.doc.id })) as ArrayBuffer)).toBe(
      "second\n",
    );
    await expect(handle("undo_delete_note", { token: secondToken })).rejects.toBe("There is nothing to put back.");
  });

  it("leaves a file that took the trashed note's name in place", async () => {
    const { handle } = backend();
    const note = await writeNote(handle, "Taken/Name.md", "old\n");
    const token = (await handle("delete_note", { id: note.id })) as string;
    await writeNote(handle, "Taken/Name.md", "new\n");
    await expect(handle("undo_delete_note", { token })).rejects.toBe('A file named "Name.md" is already there.');
  });

  it("gives no Undo for a note with no file or one past the size the Undo keeps", async () => {
    const { handle } = backend();
    const fresh = (await handle("create_buffer", {})) as BufferDocument;
    expect(await handle("delete_note", { id: fresh.id })).toBeNull();
    const big = await writeNote(handle, "Big.md", "x".repeat(TRASH_UNDO_MAX_BYTES + 1));
    expect(await handle("delete_note", { id: big.id })).toBeNull();
  });

  it("answers the quick capture chord and a file manager request", async () => {
    const { handle } = backend();
    expect(await handle("capture_hotkey_status", {})).toEqual({
      chord: DEFAULT_CAPTURE_CHORD,
      registered: true,
      unsupported: false,
    });
    expect(await handle("show_workspace_entry_in_file_manager", { path: `${NOTES_ROOT}/To do.txt` })).toBeNull();
  });
});
