import { createRoot, createSignal } from "solid-js";
import * as api from "../../services/tauri";
import { logFailure } from "../../lib/log";
import { showToast } from "./notifications";

// Singleton state — Jotra is single-window. Whether the OS starts Jotra at
// login, as the OS has it.

/** The code of a change the OS made that the config could not record. */
const UNSAVED = "ERR_LOGIN_ITEM_UNSAVED";

export function createLoginItemStore() {
  const [enabled, setEnabled] = createSignal(false);
  const [busy, setBusy] = createSignal(false);

  async function load(): Promise<void> {
    try {
      setEnabled((await api.launchAtLoginStatus()).enabled);
    } catch {
      logFailure("whether Jotra opens at login could not be read");
    }
  }

  /** Asks the OS to start Jotra at login, or to stop. A refusal leaves the
   * switch where it was and says so. */
  async function set(on: boolean): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      setEnabled((await api.setLaunchAtLogin(on)).enabled);
    } catch (error) {
      logFailure("open at login could not be changed");
      if (String(error instanceof Error ? error.message : error).startsWith(UNSAVED)) {
        setEnabled(on);
        showToast("Could not save your settings", "error");
      } else {
        showToast(on ? "Couldn't add Jotra to your login items" : "Couldn't remove Jotra from your login items", "error");
      }
    } finally {
      setBusy(false);
    }
  }

  return { enabled, busy, load, set };
}

export const loginItemStore = createRoot(createLoginItemStore);
