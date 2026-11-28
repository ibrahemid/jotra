import { bufferRegistry } from "./buffer-registry";
import { showToast } from "./notifications";
import { logFailure } from "../../lib/log";

// Singleton state — Jotra is single-window
const pendingDrops = new Set<() => Promise<void>>();

/**
 * Takes rows out of Recent with an Undo on screen. The rows are deleted when
 * the toast goes, and shown again if Undo is taken. The files stay where they
 * are either way: Recent lists closed tabs, not files.
 */
export function removeFromRecent(ids: readonly string[], text: string): void {
  if (ids.length === 0) return;
  bufferRegistry.hideHistory(ids);
  const drop = async (): Promise<void> => {
    if (!pendingDrops.delete(drop)) return;
    try {
      await bufferRegistry.dropHiddenHistory(ids);
    } catch {
      logFailure("rows could not be removed from Recent");
      showToast("Could not remove the files from Recent. They are listed again.", "error");
    }
  };
  pendingDrops.add(drop);
  showToast({
    text,
    type: "success",
    action: { label: "Undo", run: () => bufferRegistry.unhideHistory(ids) },
    onClose: (undone) => {
      if (undone) pendingDrops.delete(drop);
      else void drop();
    },
  });
}

/** Deletes the rows whose Undo is still on screen, as quitting takes the toast
 * away with the window. */
export async function dropPendingRecentRemovals(): Promise<void> {
  await Promise.all([...pendingDrops].map((drop) => drop()));
}

/** [`removeFromRecent`] for one row, named in the toast. */
export function removeOneFromRecent(id: string): void {
  const row = bufferRegistry.buffers().find((b) => b.id === id);
  removeFromRecent([id], row ? `Removed "${row.title}" from Recent` : "Removed from Recent");
}
