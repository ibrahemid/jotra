import { describe, it, expect, vi, afterEach } from "vitest";

const h = vi.hoisted(() => ({
  getRecoveredBuffers: vi.fn(),
  showToast: vi.fn(),
}));

vi.mock(import("../../services/tauri"), async (importOriginal) => ({
  ...(await importOriginal()),
  saveBufferContent: vi.fn().mockResolvedValue(null),
  noteDiskState: vi.fn(async () => ({ state: "no_file" as const })),
  restoreNoteFile: vi.fn().mockResolvedValue(null),
  saveNoteTo: vi.fn().mockResolvedValue("digest"),
  recordUnsavedNotes: vi.fn().mockResolvedValue(undefined),
  discardUnsavedText: vi.fn().mockResolvedValue(undefined),
  getRecoveredBuffers: h.getRecoveredBuffers,
}));

vi.mock("../../stores/global/notifications", () => ({ showToast: h.showToast }));

import { createEditorStore } from "../../stores/window/editor-store";
import { applyRecoveredNotes } from "../../stores/window/recovered-notes";
import { createBootPlan } from "../../stores/window/boot";
import type { WindowState } from "../../stores/window/createWindowState";
import type { RecoveredBuffer } from "../../services/tauri";

let stores: ReturnType<typeof createEditorStore>[] = [];

function editor() {
  const store = createEditorStore();
  stores.push(store);
  return store;
}

afterEach(() => {
  for (const store of stores) store.stopSaveListener();
  stores = [];
  h.getRecoveredBuffers.mockReset();
  h.showToast.mockReset();
});

function restoreAtLaunch(store: ReturnType<typeof createEditorStore>): Promise<void> {
  const win = { editor: store, tabs: {} } as unknown as WindowState;
  const plan = createBootPlan(win, {
    prepareShell: () => [],
    registerCommands: () => {},
    installKeyboard: () => () => {},
    runMenuAction: () => {},
    afterReveal: [],
  });
  return plan.restoreRecovered();
}

const unmade = { name: "2026-10-13.txt", folder: "Jotra" };

const restoredNote: RecoveredBuffer = {
  id: "restored",
  content: "written back",
  removed_on_disk: false,
};
const deletedNote: RecoveredBuffer = {
  id: "deleted",
  content: "typed after the file went",
  removed_on_disk: true,
  unmade_file: null,
};
const unmadeNote: RecoveredBuffer = {
  id: "unmade",
  content: "the first words",
  removed_on_disk: true,
  unmade_file: unmade,
};

describe("the notes the launch recovered", () => {
  it("brings a note whose first file the launch could not make up under a bar naming that file", () => {
    const store = editor();

    applyRecoveredNotes(store, [unmadeNote]);

    expect(store.isRemovedOnDisk("unmade")).toBe(true);
    expect(store.unmadeFileOf("unmade")).toEqual(unmade);
  });

  it("brings a note whose file was deleted up under the deleted-file bar", () => {
    const store = editor();

    applyRecoveredNotes(store, [deletedNote, { ...deletedNote, id: "older", unmade_file: undefined }]);

    expect(store.isRemovedOnDisk("deleted")).toBe(true);
    expect(store.unmadeFileOf("deleted")).toBeNull();
    expect(store.isRemovedOnDisk("older")).toBe(true);
    expect(store.unmadeFileOf("older")).toBeNull();
  });

  it("hands the editor each held note's text and the file it was to be", () => {
    const markRemovedOnDisk = vi.fn();

    applyRecoveredNotes({ markRemovedOnDisk }, [restoredNote, deletedNote, unmadeNote]);

    expect(markRemovedOnDisk.mock.calls).toEqual([
      ["deleted", "typed after the file went", undefined],
      ["unmade", "the first words", unmade],
    ]);
  });

  it("answers only the notes the launch restored, which the toast counts", () => {
    const restored = applyRecoveredNotes({ markRemovedOnDisk: vi.fn() }, [
      restoredNote,
      deletedNote,
      unmadeNote,
    ]);

    expect(restored.map((note) => note.id)).toEqual(["restored"]);
  });

  it("is what the window's launch hands the recovered notes to", async () => {
    const store = editor();
    h.getRecoveredBuffers.mockResolvedValue([restoredNote, deletedNote, unmadeNote]);

    await restoreAtLaunch(store);

    expect(h.getRecoveredBuffers).toHaveBeenCalledTimes(1);
    expect(store.isRemovedOnDisk("deleted")).toBe(true);
    expect(store.isRemovedOnDisk("unmade")).toBe(true);
    expect(store.unmadeFileOf("unmade")).toEqual(unmade);
    expect(store.isRemovedOnDisk("restored")).toBe(false);
    expect(h.showToast.mock.calls).toEqual([["Restored 1 file that could not be saved last time", "info", 6000]]);
  });

  it("launches with nothing restored when the recovered notes cannot be read", async () => {
    const store = editor();
    h.getRecoveredBuffers.mockRejectedValue(new Error("database is locked"));

    await expect(restoreAtLaunch(store)).resolves.toBeUndefined();

    expect(store.isRemovedOnDisk("deleted")).toBe(false);
    expect(h.showToast).not.toHaveBeenCalled();
  });
});
