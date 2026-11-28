import { type EditorState, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  WidgetType,
  type Command,
  type DecorationSet,
  type PluginValue,
  type ViewUpdate,
} from "@codemirror/view";
import {
  codeFolding,
  foldCode,
  foldEffect,
  foldGutter,
  foldable,
  foldedRanges,
  syntaxTree,
  unfoldCode,
  unfoldEffect,
} from "@codemirror/language";
import { autoDirectionOf } from "./bidi";
import type { Platform } from "../lib/platform";

const SVG_NS = "http://www.w3.org/2000/svg";

/** The chevron a fold control draws: pointing down while open, right once folded. */
function chevron(): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("class", "cm-fold-chevron");
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("d", "M4.5 6.25 8 9.75l3.5-3.5");
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "1.5");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  svg.appendChild(path);
  return svg;
}

/** A fold command: a palette entry with a chord of its own on macOS and elsewhere. */
export interface FoldCommand {
  id: string;
  label: string;
  keywords: string[];
  mac: string;
  other: string;
  run: Command;
}

/**
 * Fold and unfold at the cursor. Command-Option-[ and ] on macOS; elsewhere
 * Control-Shift-[ and ], because Control-Alt is AltGr on Windows and types
 * the bracket itself on several European layouts. Registered commands, so the
 * palette lists them and Settings, Shortcuts rebinds them.
 */
export const FOLD_COMMANDS: readonly FoldCommand[] = [
  {
    id: "editor.fold",
    label: "Fold at the cursor",
    keywords: ["collapse"],
    mac: "CmdOrCtrl+Alt+[",
    other: "CmdOrCtrl+Shift+[",
    run: foldCode,
  },
  {
    id: "editor.unfold",
    label: "Unfold at the cursor",
    keywords: ["expand"],
    mac: "CmdOrCtrl+Alt+]",
    other: "CmdOrCtrl+Shift+]",
    run: unfoldCode,
  },
];

/** The chord a fold command registers with on `platform`. */
export function foldChord(command: FoldCommand, platform: Platform): string {
  return platform === "mac" ? command.mac : command.other;
}

/**
 * What folds are: the state field that holds them and the placeholder a
 * folded range shows. Outside every compartment, so the folds of a note
 * survive a switch between Inline and Source and a switch of tabs. The keys
 * are {@link FOLD_COMMANDS}, which the command registry dispatches.
 */
export const folding: Extension = codeFolding();

/** The fold gutter a source file reads with, beside its line numbers. */
export const codeFoldGutter: Extension = foldGutter({
  markerDOM(open) {
    const marker = document.createElement("span");
    marker.className = "cm-fold-marker";
    if (!open) marker.dataset.folded = "true";
    marker.appendChild(chevron());
    return marker;
  },
});

/** One heading that can fold, and whether it is folded. */
export interface FoldToggle {
  /** Start of the line the control sits on. */
  line: number;
  /** The range the fold hides. */
  from: number;
  to: number;
  folded: boolean;
}

// A heading folds its section. A fenced block, a paragraph or a list folds
// from the keyboard: in Inline a fenced block's opening line is off screen,
// so a control on it would never be seen.
const TOGGLE_NODES = /^(ATXHeading[1-6]|SetextHeading[12])$/;

// Containers a heading can sit inside. The walk descends into these only, so
// a screen of prose costs one step per block.
const CONTAINERS = new Set(["Document", "Blockquote", "BulletList", "OrderedList", "ListItem"]);

/** The fold that starts on `line`, when one does. */
function foldStartingOn(
  folded: ReturnType<typeof foldedRanges>,
  line: { from: number; to: number },
): { from: number; to: number } | null {
  for (const cursor = folded.iter(line.from); cursor.value !== null; cursor.next()) {
    if (cursor.from > line.to) break;
    if (cursor.from >= line.from) return { from: cursor.from, to: cursor.to };
  }
  return null;
}

/** The fold controls for the headings in `[from, to)`. */
export function foldTogglesIn(state: EditorState, from: number, to: number): FoldToggle[] {
  const toggles: FoldToggle[] = [];
  const folded = foldedRanges(state);
  syntaxTree(state).iterate({
    from,
    to,
    enter: (node) => {
      if (TOGGLE_NODES.test(node.name)) {
        const line = state.doc.lineAt(node.from);
        const fold = foldStartingOn(folded, line);
        if (fold !== null) {
          toggles.push({ line: line.from, from: fold.from, to: fold.to, folded: true });
        } else {
          const range = foldable(state, line.from, line.to);
          if (range !== null) {
            toggles.push({ line: line.from, from: range.from, to: range.to, folded: false });
          }
        }
        return false;
      }
      return CONTAINERS.has(node.name);
    },
  });
  return toggles;
}

class FoldToggleWidget extends WidgetType {
  constructor(
    readonly toggle: FoldToggle,
    readonly rtl: boolean,
  ) {
    super();
  }

  override eq(other: FoldToggleWidget): boolean {
    return (
      other.rtl === this.rtl &&
      other.toggle.folded === this.toggle.folded &&
      other.toggle.from === this.toggle.from &&
      other.toggle.to === this.toggle.to
    );
  }

  toDOM(view: EditorView): HTMLElement {
    const anchor = document.createElement("span");
    anchor.className = "cm-md-fold";
    if (this.toggle.folded) anchor.dataset.folded = "true";
    if (this.rtl) anchor.dataset.dir = "rtl";
    anchor.setAttribute("aria-hidden", "true");
    const control = document.createElement("span");
    control.className = "cm-md-fold-control";
    control.title = view.state.phrase(this.toggle.folded ? "Unfold section" : "Fold section");
    control.appendChild(chevron());
    anchor.appendChild(control);
    const { from, to, folded } = this.toggle;
    control.addEventListener("mousedown", (event) => {
      if (event.button !== 0) return;
      // The caret stays where it is: moving it onto the heading would take
      // the control with it before the click lands.
      event.preventDefault();
      view.dispatch({
        effects: folded ? unfoldEffect.of({ from, to }) : foldEffect.of({ from, to }),
      });
    });
    return anchor;
  }

  // The control takes its own mousedown; everything else the editor handles.
  override ignoreEvent(event: Event): boolean {
    return event.type === "mousedown";
  }
}

function toggleDecorations(view: EditorView): DecorationSet {
  const ranges = [];
  for (const { from, to } of view.visibleRanges) {
    for (const toggle of foldTogglesIn(view.state, from, to)) {
      const rtl = autoDirectionOf(view.state.doc.lineAt(toggle.line).text) === "rtl";
      ranges.push(
        Decoration.widget({ widget: new FoldToggleWidget(toggle, rtl), side: -1 }).range(
          toggle.line,
        ),
      );
    }
  }
  return Decoration.set(ranges, true);
}

class ProseFoldToggles implements PluginValue {
  decorations: DecorationSet;

  constructor(view: EditorView) {
    this.decorations = toggleDecorations(view);
  }

  update(update: ViewUpdate) {
    if (
      update.docChanged ||
      update.viewportChanged ||
      syntaxTree(update.startState) !== syntaxTree(update.state) ||
      foldedRanges(update.startState) !== foldedRanges(update.state)
    ) {
      this.decorations = toggleDecorations(update.view);
    }
  }
}

/**
 * The prose fold control: a chevron in the margin before a heading, drawn
 * while the pointer is on its line or while its section is folded.
 * Prose keeps an empty margin and no gutter (ADR-030 decision 3), so the
 * control sits in the margin beside the line instead of in a column of its
 * own.
 */
export const proseFoldToggles: Extension = ViewPlugin.fromClass(ProseFoldToggles, {
  decorations: (plugin) => plugin.decorations,
});

/** The fold controls a file gets: a gutter for code, margin controls for Markdown. */
export function foldChromeFor(kind: "code" | "markdown" | "none"): Extension {
  if (kind === "code") return codeFoldGutter;
  if (kind === "markdown") return proseFoldToggles;
  return [];
}
