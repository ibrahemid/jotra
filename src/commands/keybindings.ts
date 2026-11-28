import {
  executeCommand,
  getAllCommands,
  getCommand,
  notifyRegistryChanged,
} from "./registry";
import { effectiveBinding } from "./effective-bindings";
import { isCommandOffered } from "./app-commands";
import { isModalOpen } from "../lib/modal-stack";
import { canonicalChord, chordFromKey, chordFromPosition } from "../lib/chord";
import { isImeComposing } from "../lib/ime";
import { detectPlatform, type Platform } from "../lib/platform";
import { firstRunStore } from "../stores/global/first-run";

export {
  effectiveBinding,
  setKeybindingOverrides,
  useCommandBinding,
  useEffectiveBinding,
} from "./effective-bindings";

const LEGACY_DEFAULT_BINDINGS: ReadonlyMap<string, string> = new Map([
  ["buffer.new", "CmdOrCtrl+N"],
  ["buffer.close", "CmdOrCtrl+W"],
  ["history.restoreLast", "CmdOrCtrl+Shift+T"],
  ["sidebar.toggle", "CmdOrCtrl+S"],
  ["palette.open", "CmdOrCtrl+Shift+P"],
]);

export function pruneLegacyDefaultOverrides(
  loaded: Readonly<Record<string, string>>,
): Record<string, string> {
  const result: Record<string, string> = {};
  let changed = false;
  for (const [id, binding] of Object.entries(loaded)) {
    if (LEGACY_DEFAULT_BINDINGS.get(id) === binding) {
      changed = true;
      continue;
    }
    result[id] = binding;
  }
  return changed ? result : { ...loaded };
}

/** The chord a keystroke spells from the character it produced. */
export function normalizeKey(e: KeyboardEvent, platform: Platform = detectPlatform()): string {
  return chordFromKey(e, platform);
}

/** A command that holds a chord, and whether the chord is one of its aliases. */
interface ChordHolder {
  id: string;
  alias: boolean;
}

const keyBindingMap = new Map<string, ChordHolder[]>();

/**
 * The holder a chord runs: the last registered among the commands the
 * shortcut editor lists, so a chord the person gave a command is never taken
 * by an app that is off; a command of an app that is off only when no other
 * command holds the chord.
 */
function answeringHolder(holders: readonly ChordHolder[] | undefined): string | undefined {
  if (!holders || holders.length === 0) return undefined;
  for (let i = holders.length - 1; i >= 0; i--) {
    const cmd = getCommand(holders[i].id);
    if (cmd && isCommandOffered(cmd)) return holders[i].id;
  }
  return holders[holders.length - 1].id;
}

/**
 * The command a keystroke names: by the character first, then by the key's
 * position, so Ctrl+N on an Arabic layout (character "ى", position KeyN) still
 * makes a file while punctuation chords keep their character. An alias names
 * the character it is for, so a shifted punctuation key reaches it by
 * character only: Cmd+? on a US layout is not the CmdOrCtrl+Shift+/ alias.
 */
function commandForKey(e: KeyboardEvent, platform: Platform): string | undefined {
  const byKey = answeringHolder(keyBindingMap.get(chordFromKey(e, platform)));
  if (byKey) return byKey;
  const byPosition = chordFromPosition(e, platform);
  if (byPosition === null) return undefined;
  const holders = keyBindingMap.get(byPosition);
  const punctuation = !/^(Key[A-Z]|Digit[0-9]|Space)$/.test(e.code ?? "");
  return answeringHolder(punctuation ? holders?.filter((holder) => !holder.alias) : holders);
}

let lastShiftTime = 0;
let shiftCommandId: string | null = null;

export function rebuildKeyMap() {
  keyBindingMap.clear();
  shiftCommandId = null;
  const platform = detectPlatform();

  for (const cmd of getAllCommands()) {
    const primary = effectiveBinding(cmd.id, cmd.keybinding);
    const bindings = [
      ...(primary ? [{ binding: primary, alias: false }] : []),
      ...(cmd.keybindingAliases ?? []).map((binding) => ({ binding, alias: true })),
    ];
    for (const { binding, alias } of bindings) {
      if (!binding) continue;
      if (binding === "Shift+Shift") {
        shiftCommandId = cmd.id;
        continue;
      }
      const chord = canonicalChord(binding, platform);
      keyBindingMap.set(chord, [...(keyBindingMap.get(chord) ?? []), { id: cmd.id, alias }]);
    }
  }

  // Conflicts are shown where they can be fixed: the shortcut editor marks
  // every conflicting row with what it collides with.
  notifyRegistryChanged();
}

/**
 * True when keyboard focus is inside a CodeMirror editor. Editor-scoped
 * commands fire only here. Lives in `src/commands/` (not a component or store),
 * so reading `document.activeElement` here is permitted.
 */
function isEditorFocused(): boolean {
  const el = document.activeElement;
  return el !== null && el.closest(".cm-editor") !== null;
}

/**
 * True when keyboard focus is inside a CodeMirror editor or any native text
 * input, where a non-global app-scoped chord must yield to the editor/input.
 * Lives in `src/commands/` (not a component or store), so reading
 * `document.activeElement` here is permitted.
 */
function isTextEntryFocused(): boolean {
  const el = document.activeElement;
  if (!el) return false;
  if (el.closest(".cm-editor")) return true;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA") return true;
  return (el as HTMLElement).isContentEditable === true;
}

export function handleKeyDown(e: KeyboardEvent): boolean {
  // The first launch's question is the only thing the window can answer until
  // it is answered: the palette, Settings and every tab chord would open
  // behind the screen and take the focus the two options need. One gate here
  // rather than a check in each command.
  if (firstRunStore.step() !== null) return false;
  if (isModalOpen()) return false;
  // The pending key is meant for the input method, not the command map.
  if (isImeComposing(e)) return false;
  if (e.key === "Shift" && shiftCommandId) {
    const now = Date.now();
    if (now - lastShiftTime < 400) {
      e.preventDefault();
      e.stopPropagation();
      executeCommand(shiftCommandId);
      lastShiftTime = 0;
      return true;
    }
    lastShiftTime = now;
    return false;
  }

  if (e.key !== "Shift") {
    lastShiftTime = 0;
  }

  const commandId = commandForKey(e, detectPlatform());
  if (!commandId) return false;
  const cmd = getCommand(commandId);
  if (!cmd) return false;

  // Scope gate: an editor command runs only with focus inside a CodeMirror
  // editor; a non-global app command yields to any focused text entry; a
  // global app command runs anywhere.
  if (cmd.scope === "editor") {
    if (!isEditorFocused()) return false;
  } else if (!cmd.global && isTextEntryFocused()) {
    return false;
  }

  // Run before deciding: a command that declines (returns false) is a
  // fall-through, so the keystroke reaches CodeMirror and the browser exactly
  // as a native keymap miss would — no preventDefault, no stopPropagation.
  if (!executeCommand(commandId)) return false;
  e.preventDefault();
  e.stopPropagation();
  return true;
}

export function installKeyboardHandler() {
  document.addEventListener("keydown", handleKeyDown, { capture: true });
}

export function uninstallKeyboardHandler() {
  document.removeEventListener("keydown", handleKeyDown, { capture: true });
}
