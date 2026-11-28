import { createRoot, createSignal } from "solid-js";
import * as api from "../../services/tauri";
import { onEvent, type UnlistenFn } from "../../services/events";
import { logFailure } from "../../lib/log";
import { sameChord } from "../../lib/chord";
import { detectPlatform, type Platform } from "../../lib/platform";

/** The chord Jotra ships with for showing and hiding the window. */
export const SHIPPED_WINDOW_CHORD = "CmdOrCtrl+Shift+Space";

/** The chord Jotra ships with for the quick capture note (ADR-055). */
export const SHIPPED_CAPTURE_CHORD = "CmdOrCtrl+Shift+Alt+Space";

/** How one chord that works from any app is read, listened for and changed.
 * Functions rather than the service exports themselves, so a test that mocks
 * the services with only the window's calls can still load this module. */
interface GlobalChordCalls {
  status: () => Promise<api.GlobalHotkeyStatus>;
  rebind: (chord: string) => Promise<api.HotkeyRebind>;
  subscribe: (apply: (status: api.GlobalHotkeyStatus) => void) => Promise<UnlistenFn>;
  /** What a failed read is logged as. */
  unread: string;
  /** The other chord, when the OS is never asked for this one while that
   * one holds the same keys. */
  heldFirstBy?: () => ChordHolder;
  /** The platform whose shortcuts the two chords are read as. */
  platform?: Platform;
}

/** What one chord's store says about the keys the OS gave Jotra. */
export interface ChordHolder {
  chord: () => string;
  isHeld: () => boolean;
}

// Singleton state — Jotra is single-window. Each chord that works from any app
// has one answer saying whether this machine let Jotra have it.

function createGlobalChordStore(calls: GlobalChordCalls) {
  const [status, setStatus] = createSignal<api.GlobalHotkeyStatus>({
    chord: "",
    registered: false,
    unsupported: false,
  });

  const chord = () => status().chord;
  const isHeld = () => status().registered;
  const isUnsupported = () => status().unsupported === true;
  /** The other chord holds these keys, so the OS was not asked for them. */
  const isHeldByOther = () => {
    const now = status();
    const other = calls.heldFirstBy?.();
    return (
      now.chord !== "" &&
      !now.registered &&
      other !== undefined &&
      other.isHeld() &&
      sameChord(now.chord, other.chord(), calls.platform ?? detectPlatform())
    );
  };
  // An empty chord is one turned off, which nothing else can be holding.
  const isTaken = () => chord() !== "" && !isHeld() && !isUnsupported() && !isHeldByOther();

  function apply(next: api.GlobalHotkeyStatus): void {
    setStatus(next);
  }

  // Read once at startup as well as listened for: the answer is settled before
  // the window exists, so the event that carried it has already been and gone.
  async function load(): Promise<void> {
    try {
      apply(await calls.status());
    } catch {
      logFailure(calls.unread);
    }
  }

  async function subscribe(): Promise<UnlistenFn> {
    return calls.subscribe(apply);
  }

  /** Asks the OS for `next`, and answers whether it was given. A refusal
   * keeps the chord there was, which `chord()` then still names. */
  async function rebind(next: string): Promise<boolean> {
    const answer = await calls.rebind(next);
    apply(answer.status);
    return !answer.refused && (answer.status.registered || next === "");
  }

  return { chord, isHeld, isTaken, isHeldByOther, isUnsupported, load, subscribe, rebind };
}

export function createHotkeyStore() {
  return createGlobalChordStore({
    status: () => api.globalHotkeyStatus(),
    rebind: (chord) => api.setGlobalHotkey(chord),
    subscribe: (apply) => onEvent("hotkey:status", apply),
    unread: "the global shortcut's state could not be read",
  });
}

export function createCaptureHotkeyStore(
  windowChord: () => ChordHolder = () => hotkeyStore,
  platform?: Platform,
) {
  return createGlobalChordStore({
    status: () => api.captureHotkeyStatus(),
    rebind: (chord) => api.setCaptureHotkey(chord),
    subscribe: (apply) => onEvent("hotkey:capture-status", apply),
    unread: "the capture shortcut's state could not be read",
    heldFirstBy: windowChord,
    platform,
  });
}

export const hotkeyStore = createRoot(createHotkeyStore);
export const captureHotkeyStore = createRoot(() => createCaptureHotkeyStore());
