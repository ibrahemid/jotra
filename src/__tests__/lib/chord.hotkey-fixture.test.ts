import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { osShortcut, sameChord } from "../../lib/chord";
import type { Platform } from "../../lib/platform";
import HOTKEY_TOKENS from "../../lib/hotkey-tokens.json";

// crates/jotra-core/tests/hotkey_tests.rs and src-tauri/src/hotkey/mod.rs read the same table and fixture.
interface HostCases {
  same: [string, string][];
  different: [string, string][];
}

interface ChordCases {
  mac: HostCases;
  other: HostCases;
  spelledKeys: [string, string][];
  unparsed: string[];
}

type HotkeyMod = keyof typeof HOTKEY_TOKENS.modifiers;
type Host = keyof typeof HOTKEY_TOKENS.held;

const cases: ChordCases = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../crates/jotra-core/tests/fixtures/hotkey-chords.json"),
    "utf8",
  ),
);

const HOSTS: [Host, Platform][] = [
  ["mac", "mac"],
  ["other", "win"],
  ["other", "linux"],
];

const LETTERS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("");
const DIGITS = "0123456789".split("");

describe.each(HOSTS)("a chord the OS holds, read as src-tauri reads it (%s, %s)", (host, platform) => {
  const lacking: readonly string[] = HOTKEY_TOKENS.keysTheOsLacks[host];
  const held = (chord: string, key: string) => (lacking.includes(key) ? null : chord);

  it("holds each modifier spelling as the table says, in any case", () => {
    for (const [name, spellings] of Object.entries(HOTKEY_TOKENS.modifiers)) {
      const flag = HOTKEY_TOKENS.held[host][name as HotkeyMod];
      for (const spelling of spellings) {
        const capitalised = spelling.charAt(0).toUpperCase() + spelling.slice(1);
        for (const written of [spelling, spelling.toUpperCase(), capitalised]) {
          expect(osShortcut(`${written}+F13`, platform), written).toBe(`${flag}+F13`);
        }
      }
    }
  });

  it("reads each key spelling as its name, in any case", () => {
    for (const [name, spellings] of Object.entries(HOTKEY_TOKENS.keys)) {
      for (const spelling of spellings) {
        for (const written of [spelling, spelling.toLowerCase()]) {
          expect(osShortcut(`Alt+${written}`, platform), written).toBe(`Alt+${name}`);
        }
      }
    }
    for (const [token, name] of cases.spelledKeys) {
      expect(osShortcut(`Alt+${token}`, platform), token).toBe(held(`Alt+${name}`, name));
    }
  });

  it("reads every letter, digit and function key the OS has", () => {
    for (const letter of LETTERS) {
      expect(osShortcut(`Alt+${letter.toLowerCase()}`, platform)).toBe(`Alt+Key${letter}`);
    }
    for (const digit of DIGITS) {
      expect(osShortcut(`Alt+${digit}`, platform)).toBe(`Alt+Digit${digit}`);
    }
    for (let n = 1; n <= 24; n++) {
      const key = `F${n}`;
      expect(osShortcut(`Alt+${key}`, platform), key).toBe(held(`Alt+${key}`, key));
    }
  });

  it("matches the chords the OS holds as one shortcut, and only those", () => {
    for (const [a, b] of cases[host].same) {
      expect(sameChord(a, b, platform), `${a} and ${b}`).toBe(true);
    }
    for (const [a, b] of cases[host].different) {
      expect(sameChord(a, b, platform), `${a} and ${b}`).toBe(false);
    }
  });

  it("reads nothing from a chord the parser refuses", () => {
    for (const chord of cases.unparsed) {
      expect(osShortcut(chord, platform), JSON.stringify(chord)).toBeNull();
      expect(sameChord(chord, chord, platform)).toBe(false);
    }
  });
});
