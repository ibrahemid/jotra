import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("../../services/tauri", () => ({
  saveBufferContent: vi.fn().mockResolvedValue(null),
  saveBufferAsUtf8: vi.fn().mockResolvedValue("digest"),
  noteDiskState: vi.fn(async () => ({ state: "no_file" })),
}));

import { createEditorStore } from "../../stores/window/editor-store";
import { resetAutosave } from "../../services/autosave";
import { saveBufferAsUtf8, saveBufferContent } from "../../services/tauri";

const mockedSave = vi.mocked(saveBufferContent);
const mockedSaveAsUtf8 = vi.mocked(saveBufferAsUtf8);

let stores: Array<{ stopSaveListener: () => void }> = [];

/** A store with note "one" loaded into the editor. */
function loadedStore(text: string) {
  const store = createEditorStore();
  stores.push(store);
  const view = {
    state: { doc: { toString: () => text } },
  } as unknown as import("@codemirror/view").EditorView;
  store.registerView(view);
  store.setCurrentBufferId("one");
  return store;
}

beforeEach(() => {
  resetAutosave();
  mockedSave.mockReset();
  mockedSave.mockResolvedValue(null);
  mockedSaveAsUtf8.mockReset();
  mockedSaveAsUtf8.mockResolvedValue("digest");
});

afterEach(() => {
  for (const store of stores) store.stopSaveListener();
  stores = [];
  resetAutosave();
});

describe("editorStore saveAsUtf8", () => {
  it("writes the note's text through the UTF-8 save, not the ordinary one", async () => {
    const store = loadedStore("Café 😀");

    const result = await store.saveAsUtf8("one");

    expect(result.ok).toBe(true);
    expect(mockedSaveAsUtf8).toHaveBeenCalledWith("one", "Café 😀");
    expect(mockedSave).not.toHaveBeenCalled();
  });

  it("writes nothing while the note's file is being asked about", async () => {
    const store = loadedStore("Café 😀");
    store.recordFileEvent("one", "modified");

    const result = await store.saveAsUtf8("one");

    expect(result.ok).toBe(true);
    expect(mockedSaveAsUtf8).not.toHaveBeenCalled();
  });

  it("keeps the removed mark for a file deleted while the write was on its way", async () => {
    // The UTF-8 save is refused for a file that is gone, as the ordinary save
    // is, so its landing says there was a file when it left and no more.
    let land: (digest: string) => void = () => {};
    mockedSaveAsUtf8.mockImplementationOnce(
      () => new Promise<string>((resolve) => (land = resolve)),
    );
    const store = loadedStore("Café 😀");

    const written = store.saveAsUtf8("one");
    store.recordFileEvent("one", "removed");
    land("digest");
    await written;

    expect(store.isRemovedOnDisk("one")).toBe(true);
  });
});
