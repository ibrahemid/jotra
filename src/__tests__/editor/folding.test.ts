import { describe, it, expect, afterEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { javascript } from "@codemirror/lang-javascript";
import { ensureSyntaxTree, foldEffect, foldedRanges } from "@codemirror/language";
import {
  FOLD_COMMANDS,
  foldChord,
  foldChromeFor,
  foldTogglesIn,
  folding,
} from "../../editor/folding";
import { registerEditorCommands } from "../../editor/editor-commands";
import { getCommand } from "../../commands/registry";
import { handleKeyDown, setKeybindingOverrides } from "../../commands/keybindings";

const DOC = [
  "# One",
  "",
  "First body.",
  "",
  "```sh",
  "# a comment, not a heading",
  "```",
  "",
  "## Two",
  "",
  "Second body.",
  "",
  "# Three",
  "",
  "Third body.",
].join("\n");

function markdownState(doc = DOC): EditorState {
  const state = EditorState.create({
    doc,
    extensions: [markdown({ base: markdownLanguage }), folding],
  });
  ensureSyntaxTree(state, state.doc.length, 5000);
  return state;
}

const views: EditorView[] = [];

function mount(state: EditorState, extra: unknown[] = []): EditorView {
  const view = new EditorView({
    state: EditorState.create({
      doc: state.doc,
      extensions: [markdown({ base: markdownLanguage }), folding, ...extra] as never,
    }),
    parent: document.body,
  });
  ensureSyntaxTree(view.state, view.state.doc.length, 5000);
  views.push(view);
  return view;
}

afterEach(() => {
  while (views.length > 0) views.pop()!.destroy();
});

describe("heading fold controls", () => {
  it("offers one per heading, folding its section, and none for a # line in a fence", () => {
    const state = markdownState();
    const toggles = foldTogglesIn(state, 0, state.doc.length);
    expect(toggles.map((t) => state.doc.lineAt(t.line).text)).toEqual(["# One", "## Two", "# Three"]);
    const one = toggles[0];
    // The section of "# One" runs through "## Two" to the line before "# Three".
    expect(one.from).toBe(state.doc.line(1).to);
    expect(state.doc.sliceString(one.from, one.to)).toContain("Second body.");
    expect(state.doc.sliceString(one.from, one.to)).not.toContain("Third body.");
    expect(toggles.every((t) => !t.folded)).toBe(true);
  });

  it("reports a folded section as folded, with the range it hides", () => {
    let state = markdownState();
    const [one] = foldTogglesIn(state, 0, state.doc.length);
    state = state.update({ effects: foldEffect.of({ from: one.from, to: one.to }) }).state;
    const after = foldTogglesIn(state, 0, state.doc.length);
    expect(after[0]).toMatchObject({ folded: true, from: one.from, to: one.to });
  });

  it("folds and unfolds a section from the control in the margin", () => {
    const view = mount(markdownState(), [foldChromeFor("markdown")]);
    const control = view.dom.querySelector(".cm-md-fold-control");
    expect(control).toBeInstanceOf(HTMLElement);
    control!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    expect(foldedRanges(view.state).size).toBe(1);
    const folded = view.dom.querySelector(".cm-md-fold[data-folded] .cm-md-fold-control");
    expect(folded).toBeInstanceOf(HTMLElement);
    folded!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    expect(foldedRanges(view.state).size).toBe(0);
  });

  it("names the section as what the control folds and unfolds", () => {
    const view = mount(markdownState(), [foldChromeFor("markdown")]);
    const control = view.dom.querySelector<HTMLElement>(".cm-md-fold-control");
    expect(control?.title).toBe("Fold section");
    control!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    const folded = view.dom.querySelector<HTMLElement>(".cm-md-fold[data-folded] .cm-md-fold-control");
    expect(folded?.title).toBe("Unfold section");
  });

  it("marks the control of a right-to-left heading so it hangs on the right and points left once folded", () => {
    const view = mount(markdownState("# مرحبا\n\nنص.\n\n# Two\n\nBody."), [foldChromeFor("markdown")]);
    const anchors = () => Array.from(view.dom.querySelectorAll<HTMLElement>(".cm-md-fold"));
    expect(anchors().map((a) => a.dataset.dir ?? null)).toEqual(["rtl", null]);
    anchors()[0].querySelector(".cm-md-fold-control")!.dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }),
    );
    expect(anchors().map((a) => [a.dataset.dir ?? null, a.dataset.folded ?? null])).toEqual([
      ["rtl", "true"],
      [null, null],
    ]);
    view.dispatch({ changes: { from: 2, to: 7, insert: "Hello" } });
    expect(anchors()[0].dataset.dir).toBeUndefined();
  });

  it("styles a right-to-left control by the attribute, which the build does not lower", () => {
    const css = readFileSync(resolve(process.cwd(), "src/components/Editor/cm-markdown-typography.css"), "utf8");
    const rule = (selector: string) => {
      const open = css.indexOf(`${selector} {`);
      const close = open < 0 ? -1 : css.indexOf("}", open);
      return close < 0 ? "" : css.slice(open + selector.length + 2, close);
    };
    expect(rule('.cm-editor .cm-md-fold[data-dir="rtl"] .cm-md-fold-control')).toMatch(/right: auto;[\s\S]*left: 0\.3em;/);
    expect(rule('.cm-editor .cm-md-fold[data-folded][data-dir="rtl"] .cm-fold-chevron')).toContain("rotate(90deg)");
    expect(css).not.toMatch(/:dir\(|inset-inline/);
  });
});

describe("fold chrome per kind of file", () => {
  it("gives a source file a fold gutter and prose none", () => {
    const code = new EditorView({
      state: EditorState.create({
        doc: "function f() {\n  return 1;\n}\n",
        extensions: [javascript(), folding, foldChromeFor("code")],
      }),
      parent: document.body,
    });
    views.push(code);
    expect(code.dom.querySelector(".cm-foldGutter")).toBeInstanceOf(HTMLElement);
    const prose = mount(markdownState(), [foldChromeFor("markdown")]);
    expect(prose.dom.querySelector(".cm-foldGutter")).toBeNull();
    expect(foldChromeFor("none")).toEqual([]);
  });
});

function keydown(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
}

describe("fold commands", () => {
  it("fold at the cursor without Control-Alt, which is AltGr on Windows", () => {
    expect(FOLD_COMMANDS.map((fold) => [fold.id, foldChord(fold, "mac"), foldChord(fold, "win")])).toEqual([
      ["editor.fold", "CmdOrCtrl+Alt+[", "CmdOrCtrl+Shift+["],
      ["editor.unfold", "CmdOrCtrl+Alt+]", "CmdOrCtrl+Shift+]"],
    ]);
    expect(foldChord(FOLD_COMMANDS[0], "linux")).toBe("CmdOrCtrl+Shift+[");
  });

  it("are palette entries that fold and unfold the section at the cursor", () => {
    const view = mount(markdownState());
    const dispose = registerEditorCommands(() => view);
    try {
      expect(getCommand("editor.fold")).toMatchObject({ label: "Fold at the cursor", scope: "editor" });
      expect(getCommand("editor.unfold")).toMatchObject({ label: "Unfold at the cursor", scope: "editor" });
      expect(getCommand("editor.fold")!.execute()).toBe(true);
      expect(foldedRanges(view.state).size).toBe(1);
      expect(getCommand("editor.unfold")!.execute()).toBe(true);
      expect(foldedRanges(view.state).size).toBe(0);
    } finally {
      dispose();
    }
    expect(getCommand("editor.fold")).toBeUndefined();
  });

  it("leave the keys to the command registry, so the editor answers no chord of its own", () => {
    const view = mount(markdownState());
    view.contentDOM.dispatchEvent(
      keydown({ key: "{", code: "BracketLeft", keyCode: 219, ctrlKey: true, shiftKey: true }),
    );
    view.contentDOM.dispatchEvent(
      keydown({ key: "“", code: "BracketLeft", keyCode: 219, metaKey: true, altKey: true }),
    );
    expect(foldedRanges(view.state).size).toBe(0);
  });

  it("answer a rebound chord and drop the one they had", () => {
    const view = mount(markdownState());
    setKeybindingOverrides({ "editor.fold": "CmdOrCtrl+Alt+K" });
    const dispose = registerEditorCommands(() => view);
    try {
      view.focus();
      const before = keydown({ key: "{", code: "BracketLeft", ctrlKey: true, shiftKey: true });
      expect(handleKeyDown(before)).toBe(false);
      expect(foldedRanges(view.state).size).toBe(0);
      const after = keydown({ key: "k", code: "KeyK", ctrlKey: true, altKey: true });
      expect(handleKeyDown(after)).toBe(true);
      expect(foldedRanges(view.state).size).toBe(1);
    } finally {
      dispose();
      setKeybindingOverrides({});
    }
  });
});
