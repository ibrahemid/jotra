import type { Platform } from "./platform";
import HOTKEY_TOKENS from "./hotkey-tokens.json";

/** The fields of a `KeyboardEvent` a chord is read from. */
export interface ChordEvent {
  key: string;
  code?: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  getModifierState?: (modifier: string) => boolean;
}

const ASCII_LETTER_OR_DIGIT = /^[A-Za-z0-9]$/;

/**
 * The mark a punctuation key's position carries unshifted, by its code, for
 * a chord that holds Shift: Shift turns `\` into `|`, and the chord is still
 * written with the key, as `CmdOrCtrl+Shift+\` is. The same reading the
 * shifted digit gets.
 */
const PUNCTUATION_AT: Readonly<Record<string, string>> = {
  Backslash: "\\",
  Slash: "/",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Backquote: "`",
};
const FUNCTION_KEY = /^F([1-9]|1[0-9]|2[0-4])$/;

/**
 * Modifiers in the order every stored chord spells them. On macOS Command is
 * `CmdOrCtrl` and Control is `Ctrl`, so a Control chord stays the editor's
 * (Ctrl+N moves down a line); elsewhere either key is `CmdOrCtrl`.
 */
function modifierParts(event: ChordEvent, platform: Platform): string[] {
  const parts: string[] = [];
  if (platform === "mac") {
    if (event.metaKey) parts.push("CmdOrCtrl");
    if (event.ctrlKey) parts.push("Ctrl");
  } else if (event.metaKey || event.ctrlKey) {
    parts.push("CmdOrCtrl");
  }
  if (event.shiftKey && event.key !== "Shift") parts.push("Shift");
  if (event.altKey) parts.push("Alt");
  return parts;
}

function keyName(key: string): string {
  if (key === " ") return "Space";
  return key.length === 1 ? key.toUpperCase() : key;
}

/**
 * The Latin letter or digit printed at the key's position, for a key whose
 * own character is not one: a non-Latin layout (Arabic, Russian, Greek), an
 * Option-composed character on macOS, a dead key, a shifted digit, or a
 * shifted punctuation key. `null` under AltGr, where Ctrl+Alt is the layout
 * typing a character rather than a chord.
 */
function positionName(event: ChordEvent): string | null {
  if (ASCII_LETTER_OR_DIGIT.test(event.key)) return null;
  if (event.getModifierState?.("AltGraph")) return null;
  const code = event.code ?? "";
  // Option+Space on a Mac types a no-break space, which no chord names.
  if (code === "Space") return "Space";
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1];
  return event.shiftKey ? (PUNCTUATION_AT[code] ?? null) : null;
}

/** The chord spelled from the character the key produced. */
export function chordFromKey(event: ChordEvent, platform: Platform): string {
  return [...modifierParts(event, platform), keyName(event.key)].join("+");
}

/**
 * The chord spelled from the key's position, or `null` when the position
 * names nothing the character did not. Matching tries this only after the
 * character's chord misses, so punctuation, Dvorak and AZERTY letters keep
 * the meaning printed on the key.
 */
export function chordFromPosition(event: ChordEvent, platform: Platform): string | null {
  const name = positionName(event);
  if (name === null) return null;
  return [...modifierParts(event, platform), name].join("+");
}

/**
 * The chord a recorder stores: the character for a Latin letter, digit or
 * unshifted punctuation, the position for anything else, so a chord recorded
 * on one layout is the same keys on another.
 */
export function chordForRecording(event: ChordEvent, platform: Platform): string {
  const printable = event.key.length === 1 && event.key >= " " && event.key <= "~";
  const shiftedDigit = event.shiftKey === true && /^Digit[0-9]$/.test(event.code ?? "");
  if (printable && !shiftedDigit) return chordFromKey(event, platform);
  return chordFromPosition(event, platform) ?? chordFromKey(event, platform);
}

export function isFunctionKey(key: string): boolean {
  return FUNCTION_KEY.test(key);
}

/**
 * A chord's parts, modifiers first. A trailing `++` is the plus key, which a
 * plain split on `+` would lose.
 */
export function chordParts(binding: string): string[] {
  if (binding.endsWith("++")) {
    const head = binding.slice(0, -2);
    return head === "" ? ["+"] : [...head.split("+"), "+"];
  }
  return binding.split("+");
}

type HotkeyMod = keyof typeof HOTKEY_TOKENS.modifiers;

/** The modifier and key spellings `parse_hotkey_chord` in jotra-core reads,
 * by their lowercase or uppercase form. */
const MODIFIER_BY_ALIAS: ReadonlyMap<string, HotkeyMod> = new Map(
  (Object.entries(HOTKEY_TOKENS.modifiers) as [HotkeyMod, string[]][]).flatMap(
    ([name, aliases]) => aliases.map((alias) => [alias, name] as const),
  ),
);
const KEY_BY_ALIAS: ReadonlyMap<string, string> = new Map(
  Object.entries(HOTKEY_TOKENS.keys).flatMap(([name, aliases]) =>
    aliases.map((alias) => [alias, name] as const),
  ),
);

// Rust's `str::trim` set: it removes U+0085 and keeps U+FEFF, unlike String.prototype.trim.
const RUST_TRIM =
  /^[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/g;

const asciiLower = (text: string) => text.replace(/[A-Z]/g, (c) => c.toLowerCase());
const asciiUpper = (text: string) => text.replace(/[a-z]/g, (c) => c.toUpperCase());

/** The key a token names, as jotra-core's `HotkeyKey` calls it, or
 * `undefined` for a token it does not read as a key. */
function hotkeyKey(token: string): string | undefined {
  const upper = asciiUpper(token);
  const named = KEY_BY_ALIAS.get(upper);
  if (named !== undefined) return named;
  if (/^[A-Z]$/.test(upper)) return `Key${upper}`;
  if (/^[0-9]$/.test(upper)) return `Digit${upper}`;
  const fn = /^F([0-9]+)$/.exec(upper);
  if (fn === null) return undefined;
  const n = Number(fn[1]);
  return n >= 1 && n <= 24 ? `F${n}` : undefined;
}

/** A chord as `parse_hotkey_chord` reads it, or `null` where it fails. */
function parseHotkeyChord(binding: string): { modifiers: Set<HotkeyMod>; key: string } | null {
  const trimmed = binding.replace(RUST_TRIM, "");
  if (trimmed === "") return null;
  const modifiers = new Set<HotkeyMod>();
  let key: string | undefined;
  for (const raw of trimmed.split("+")) {
    const token = raw.replace(RUST_TRIM, "");
    if (token === "") return null;
    const modifier = MODIFIER_BY_ALIAS.get(asciiLower(token));
    if (modifier !== undefined) {
      modifiers.add(modifier);
      continue;
    }
    const named = hotkeyKey(token);
    if (named === undefined || key !== undefined) return null;
    key = named;
  }
  return key === undefined ? null : { modifiers, key };
}

const HELD_ORDER = ["Super", "Control", "Alt", "Shift"];

/**
 * The shortcut the OS is asked for on `platform`, as `chord_from_config` in
 * src-tauri builds it from a chord, or `null` for a chord it cannot read. Two
 * chords the OS holds as one shortcut come out the same: on macOS `Cmd` and
 * `CmdOrCtrl` are both Command, elsewhere `Ctrl` and `CmdOrCtrl` are both
 * Control.
 */
export function osShortcut(binding: string, platform: Platform): string | null {
  const chord = parseHotkeyChord(binding);
  if (chord === null) return null;
  const os = platform === "mac" ? "mac" : "other";
  if ((HOTKEY_TOKENS.keysTheOsLacks[os] as readonly string[]).includes(chord.key)) return null;
  const held: Readonly<Record<HotkeyMod, string>> = HOTKEY_TOKENS.held[os];
  const modifiers = new Set([...chord.modifiers].map((modifier) => held[modifier]));
  return [...HELD_ORDER.filter((modifier) => modifiers.has(modifier)), chord.key].join("+");
}

const TYPED_MODIFIER_ORDER = ["CmdOrCtrl", "Ctrl", "Shift", "Alt"];
const TYPED_PUNCTUATION: Readonly<Record<string, string>> = {
  Backquote: "`",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  LeftBracket: "[",
  RightBracket: "]",
  Backslash: "\\",
  Minus: "-",
  Equal: "=",
};

/** How `modifierParts` spells a modifier: Command is `CmdOrCtrl`, and off
 * macOS so is Control. */
function typedModifier(modifier: HotkeyMod, platform: Platform): string {
  if (modifier === "Cmd") return "CmdOrCtrl";
  if (modifier === "Ctrl" && platform !== "mac") return "CmdOrCtrl";
  return modifier;
}

/** How `keyName` spells a key a chord names. A key outside jotra-core's set
 * keeps its spelling. */
function typedKey(key: string): string {
  if (key.length === 1) return keyName(key);
  const named = hotkeyKey(key);
  if (named === undefined) return key;
  return TYPED_PUNCTUATION[named] ?? named;
}

/**
 * A stored chord in the form this platform's events produce: modifiers by any
 * spelling jotra-core reads, in the order `modifierParts` writes them, and the
 * key as `keyName` writes it. Off macOS there is one Control key, so a `Ctrl`
 * recorded on a Mac is `CmdOrCtrl` here. A chord with a modifier jotra-core
 * does not read keeps its order.
 */
export function canonicalChord(binding: string, platform: Platform): string {
  const trimmed = binding.trim();
  if (trimmed === "") return trimmed;
  const parts = chordParts(trimmed);
  const key = parts[parts.length - 1];
  const written = parts.slice(0, -1);
  const named = written.map((part) => MODIFIER_BY_ALIAS.get(asciiLower(part)));
  const modifiers = [
    ...new Set(
      written.map((part, i) => {
        const modifier = named[i];
        return modifier === undefined ? part : typedModifier(modifier, platform);
      }),
    ),
  ];
  if (named.every((modifier) => modifier !== undefined)) {
    modifiers.sort((a, b) => TYPED_MODIFIER_ORDER.indexOf(a) - TYPED_MODIFIER_ORDER.indexOf(b));
  }
  return [...modifiers, typedKey(key)].join("+");
}

const HOLDING_MODIFIERS: ReadonlySet<string> = new Set(["CmdOrCtrl", "Ctrl", "Cmd", "Alt"]);
const KEY_THAT_TYPES_NOTHING = /^F(1[3-9]|2[0-4])$/;

/**
 * Whether the OS can hold `binding` for every app without taking a key the
 * other apps type: it needs Cmd, Ctrl or Alt, or a key from F13 to F24.
 * Shift does not count, since Shift+A is a capital A everywhere else. The
 * same rule as `check_global_chord` in jotra-core.
 */
export function worksFromAnyApp(binding: string): boolean {
  const parts = chordParts(binding.trim());
  const key = parts[parts.length - 1] ?? "";
  if (KEY_THAT_TYPES_NOTHING.test(key)) return true;
  return parts.slice(0, -1).some((part) => HOLDING_MODIFIERS.has(part));
}

/** Whether the OS holds two chords as one shortcut on `platform`, the test
 * `settle` in src-tauri turns the second of them down on. A chord the OS
 * cannot be asked for matches nothing. */
export function sameChord(a: string, b: string, platform: Platform): boolean {
  const left = osShortcut(a, platform);
  return left !== null && left === osShortcut(b, platform);
}

/** Why the window's chord was not taken: Rust registers only these keys. */
export const UNUSABLE_GLOBAL_KEY =
  "Jotra can't use that key. Pick a letter, number, punctuation, arrow, Space or F1–F24 key.";
