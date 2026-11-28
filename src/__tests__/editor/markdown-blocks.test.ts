import { describe, it, expect, afterEach, vi } from "vitest";
import { Compartment, EditorSelection, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { ensureSyntaxTree, foldEffect } from "@codemirror/language";
import { MARKDOWN_EXTENSIONS } from "../../editor/markdown-math";
import {
  blockRenderField,
  DIAGRAM_RUNTIME_CAPTION,
  diagramFailureCaption,
  diagramKey,
  drawnBlocks,
  markdownBlockField,
  markdownBlocks,
  markdownBlocksIn,
  mathKey,
  measuredHeight,
  setBlockRenders,
  setBlockWindow,
  tableModelsBuilt,
} from "../../editor/markdown-blocks";
import { TYPING_PAUSE_MS } from "../../editor/markdown-images";
import { folding } from "../../editor/folding";
import { BlockRenderError, type BlockRenderer } from "../../editor/block-renderer";

const NEVER: BlockRenderer = {
  renderMath: () => new Promise(() => {}),
  renderDiagram: () => new Promise(() => {}),
};

const TABLE = "| Name | **Count** | Note |\n|:--|--:|:-:|\n| `a` | 2 | $x^2$ |\n| b |  | [[Plan\\|the plan]] |";
const DIAGRAM = "```mermaid\ngraph TD\n  A --> B\n```";
const MATH = "$$\nE = mc^2\n$$";
const DOC = `# Notes\n\nIntro line.\n\n${TABLE}\n\nBetween.\n\n${MATH}\n\nMore.\n\n${DIAGRAM}\n\nEnd.\n`;

function language() {
  return markdown({ base: markdownLanguage, extensions: MARKDOWN_EXTENSIONS });
}

/** A state whose block field has scanned the whole parsed document. */
function stateWith(doc: string, cursor = doc.length, extra: unknown[] = []): EditorState {
  let state = EditorState.create({
    doc,
    selection: EditorSelection.cursor(cursor),
    extensions: [language(), markdownBlocks(NEVER), ...extra] as never,
  });
  ensureSyntaxTree(state, doc.length, 5000);
  state = state.update({ effects: setBlockWindow.of({ from: 0, to: doc.length }) }).state;
  return state;
}

function drawnTexts(state: EditorState): string[] {
  return drawnBlocks(state).map((range) => state.doc.sliceString(range.from, range.to));
}

describe("the blocks Inline draws", () => {
  it("finds a table, display math and a Mermaid diagram, each by whole lines", () => {
    const state = stateWith(DOC);
    const blocks = markdownBlocksIn(state, 0, state.doc.length);
    expect(blocks.map((b) => b.kind)).toEqual(["table", "math", "diagram"]);
    expect(blocks.map((b) => state.doc.sliceString(b.from, b.to))).toEqual([TABLE, MATH, DIAGRAM]);
    expect(blocks[1].source).toBe("E = mc^2");
    expect(blocks[2].source).toBe("graph TD\n  A --> B");
  });

  it("leaves other fences, an empty diagram and a block in a quote as source", () => {
    const doc = "```js\nlet a = 1;\n```\n\n```mermaid\n```\n\n> | a | b |\n> |---|---|\n> | 1 | 2 |\n";
    const state = stateWith(doc);
    expect(markdownBlocksIn(state, 0, doc.length)).toEqual([]);
  });

  it("reads a table's cells, alignment, empty cells and wikilinks", () => {
    const state = stateWith(DOC);
    const [table] = markdownBlocksIn(state, 0, state.doc.length);
    const model = table.table!();
    expect(model.align).toEqual(["left", "right", "center"]);
    expect(model.head).toEqual([
      [{ kind: "text", text: "Name" }],
      [{ kind: "strong", runs: [{ kind: "text", text: "Count" }] }],
      [{ kind: "text", text: "Note" }],
    ]);
    expect(model.rows[0][0]).toEqual([{ kind: "code", text: "a" }]);
    expect(model.rows[0][2]).toEqual([
      { kind: "math", tex: "x^2", display: false, key: mathKey("x^2", false) },
    ]);
    expect(model.rows[1][1]).toEqual([]);
    expect(model.rows[1][2]).toEqual([{ kind: "text", text: "the plan" }]);
    expect(model.math).toEqual([mathKey("x^2", false)]);
  });
});

/** A parsed state with no block field, so nothing builds a table model unasked. */
function parsed(doc: string): EditorState {
  const state = EditorState.create({ doc, extensions: [language()] as never });
  ensureSyntaxTree(state, doc.length, 5000);
  // The finished parse reaches the state with the next transaction.
  return state.update({}).state;
}

function bigTable(rows: number, tag: string, note = ""): string {
  const body = Array.from({ length: rows }, (_, i) => `| ${tag}${i} | ${i * 7} | note ${i}${note} |`);
  return ["| Key | Value | Note |", "|---|--:|---|", ...body].join("\n");
}

describe("table models", () => {
  it("builds none while the caret is typing inside a large table", () => {
    const table = bigTable(400, "r");
    const doc = `intro\n\n${table}\n\nend\n`;
    const inside = doc.indexOf("| r200 |") + 2;
    let state = stateWith(doc, inside);
    const before = tableModelsBuilt();
    let at = inside;
    for (const char of "typed") {
      state = state.update({ changes: { from: at, insert: char }, selection: EditorSelection.cursor(at + 1) }).state;
      at++;
    }
    expect(drawnTexts(state)).toEqual([]);
    expect(tableModelsBuilt() - before).toBe(0);
    // Leaving the table draws it, from one model.
    state = state.update({ selection: EditorSelection.cursor(0) }).state;
    expect(drawnTexts(state)).toHaveLength(1);
    expect(tableModelsBuilt() - before).toBe(1);
  });

  it("keeps a table drawn again ahead of the ones drawn since", () => {
    const tables = Array.from({ length: 513 }, (_, i) => `| lru${i} | n |\n|---|---|\n| ${i} | x |`);
    const state = parsed(`${tables.join("\n\n")}\n`);
    const blocks = () => markdownBlocksIn(state, 0, state.doc.length);
    const first = blocks();
    expect(first).toHaveLength(513);
    for (const block of first.slice(0, 512)) block.table!();
    const before = tableModelsBuilt();
    blocks()[0].table!();
    expect(tableModelsBuilt() - before).toBe(0);
    first[512].table!();
    expect(tableModelsBuilt() - before).toBe(1);
    blocks()[0].table!();
    expect(tableModelsBuilt() - before).toBe(1);
  });

  it("lets go of old models once the tables held pass a total size", () => {
    const long = " words".repeat(14);
    const one = bigTable(5000, "a", long);
    const two = bigTable(5000, "b", long);
    expect(one.length + two.length).toBeGreaterThan(1 << 20);
    const state = parsed(`${one}\n\n${two}\n`);
    const [a, b] = markdownBlocksIn(state, 0, state.doc.length);
    expect(b).toBeDefined();
    a.table!();
    b.table!();
    const before = tableModelsBuilt();
    markdownBlocksIn(state, 0, state.doc.length)[0].table!();
    expect(tableModelsBuilt() - before).toBe(1);
  });
});

describe("source on the block the caret is in", () => {
  it("draws every block while the caret is outside them", () => {
    const state = stateWith(DOC, DOC.indexOf("Intro"));
    expect(drawnTexts(state)).toEqual([TABLE, MATH, DIAGRAM]);
  });

  it("a table widget yields to its source when the caret enters it", () => {
    let state = stateWith(DOC, DOC.indexOf("Intro"));
    state = state.update({ selection: EditorSelection.cursor(DOC.indexOf("| b |") + 2) }).state;
    expect(drawnTexts(state)).toEqual([MATH, DIAGRAM]);
    // The caret on the line under it gives it back.
    state = state.update({ selection: EditorSelection.cursor(DOC.indexOf("Between")) }).state;
    expect(drawnTexts(state)).toEqual([TABLE, MATH, DIAGRAM]);
  });

  it("counts a caret at either edge of a block as inside it", () => {
    let state = stateWith(DOC, DOC.indexOf(TABLE));
    expect(drawnTexts(state)).not.toContain(TABLE);
    state = state.update({ selection: EditorSelection.cursor(DOC.indexOf(TABLE) + TABLE.length) }).state;
    expect(drawnTexts(state)).not.toContain(TABLE);
  });

  it("shows a block's source where a fold starts inside it", () => {
    let state = stateWith(DOC, DOC.indexOf("Intro"), [folding]);
    const fence = DOC.indexOf(DIAGRAM);
    const firstLineEnd = state.doc.lineAt(fence).to;
    state = state.update({
      effects: foldEffect.of({ from: firstLineEnd, to: fence + DIAGRAM.length }),
    }).state;
    expect(drawnTexts(state)).toEqual([TABLE, MATH]);
  });

  it("keys a diagram by the theme it is drawn in", () => {
    expect(diagramKey("graph TD", "light")).not.toBe(diagramKey("graph TD", "dark"));
    expect(mathKey("x", true)).not.toBe(mathKey("x", false));
  });
});

const views: EditorView[] = [];

afterEach(() => {
  while (views.length > 0) views.pop()!.destroy();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function mount(doc: string, renderer: BlockRenderer, cursor = 0): EditorView {
  const view = new EditorView({
    state: EditorState.create({
      doc,
      selection: EditorSelection.cursor(cursor),
      extensions: [language(), markdownBlocks(renderer)] as never,
    }),
    parent: document.body,
  });
  ensureSyntaxTree(view.state, doc.length, 5000);
  view.dispatch({ effects: setBlockWindow.of({ from: 0, to: doc.length }) });
  views.push(view);
  return view;
}

async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 40));
}

describe("drawing the blocks", () => {
  it("asks once per expression and diagram, and draws what comes back", async () => {
    const renderMath = vi.fn(async (tex: string) => `<span class="katex">${tex}</span>`);
    const renderDiagram = vi.fn(async () => `<svg class="drawn"><g></g></svg>`);
    const doc = `${MATH}\n\n${DIAGRAM}\n\nAgain $$E = mc^2$$ inline.\n\n${MATH}\n`;
    const view = mount(doc, { renderMath, renderDiagram }, doc.length);
    view.dispatch({ selection: EditorSelection.cursor(doc.indexOf("Again")) });
    await settle();
    expect(renderMath).toHaveBeenCalledTimes(1);
    expect(renderMath).toHaveBeenCalledWith("E = mc^2", true);
    expect(renderDiagram).toHaveBeenCalledTimes(1);
    expect(renderDiagram).toHaveBeenCalledWith("graph TD\n  A --> B", "light");
    const math = view.dom.querySelector(".cm-md-math-block[data-status='ready'] .katex");
    expect(math?.textContent).toBe("E = mc^2");
    expect(view.dom.querySelector(".cm-md-diagram[data-status='ready'] svg.drawn")).not.toBeNull();
  });

  it("puts nothing that runs into the editor, whatever a renderer hands back", async () => {
    const hostile =
      '<svg onload="alert(1)"><script>alert(2)</script><a href="javascript:alert(3)"><text>t</text></a></svg>' +
      '<img src="x" onerror="alert(4)">';
    const view = mount(`${MATH}\n\n$x$ inline\n\n${DIAGRAM}\n\nend\n`, {
      renderMath: async () => hostile,
      renderDiagram: async () => hostile,
    }, 0);
    view.dispatch({ selection: EditorSelection.cursor(view.state.doc.length) });
    await settle();
    expect(view.dom.querySelector(".cm-md-diagram[data-status='ready'] svg")).not.toBeNull();
    expect(view.dom.querySelector("script")).toBeNull();
    for (const element of Array.from(view.dom.querySelectorAll(".cm-md-block *"))) {
      for (const attribute of Array.from(element.attributes)) {
        expect(attribute.name.startsWith("on"), attribute.name).toBe(false);
        expect(attribute.value.includes("javascript:"), attribute.value).toBe(false);
      }
    }
  });

  it("names the ids of each copy of one drawing apart, with its references kept", async () => {
    const svg =
      '<svg id="chart" aria-labelledby="chart-title"><title id="chart-title">t</title>' +
      '<style>#chart .edge{stroke:red}</style><defs><marker id="arrow"></marker></defs>' +
      '<path class="edge" marker-end="url(#arrow)"></path><use href="#arrow"></use></svg>';
    const doc = `${DIAGRAM}\n\nbetween\n\n${DIAGRAM}\n\nend\n`;
    const view = mount(doc, { renderMath: NEVER.renderMath, renderDiagram: async () => svg }, doc.indexOf("between"));
    await settle();
    const drawings = Array.from(view.dom.querySelectorAll(".cm-md-diagram[data-status='ready'] svg"));
    expect(drawings).toHaveLength(2);
    const ids = Array.from(view.dom.querySelectorAll("[id]")).map((element) => element.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const drawing of drawings) {
      const own = (id: string) => drawing.querySelector(`[id="${id}"]`);
      const marker = /^url\(#(.+)\)$/.exec(drawing.querySelector("path")!.getAttribute("marker-end")!)![1];
      expect(own(marker)?.localName).toBe("marker");
      expect(own(drawing.querySelector("use")!.getAttribute("href")!.slice(1))?.localName).toBe("marker");
      expect(own(drawing.getAttribute("aria-labelledby")!)?.localName).toBe("title");
      expect(drawing.querySelector("style")!.textContent).toBe(`#${drawing.id} .edge{stroke:red}`);
    }
  });

  it("draws a table from its text, with no renderer involved", () => {
    const view = mount(`intro\n\n${TABLE}\n`, NEVER);
    const cells = Array.from(view.dom.querySelectorAll(".cm-md-table-widget td")).map(
      (cell) => cell.textContent,
    );
    expect(cells).toEqual(["a", "2", "x^2", "b", "", "the plan"]);
    const header = view.dom.querySelector(".cm-md-table-widget th:nth-child(2)") as HTMLElement;
    expect(header.style.textAlign).toBe("right");
    expect(header.querySelector("strong")?.textContent).toBe("Count");
  });

  it("puts the caret in the cell a click lands on, which brings the source back", () => {
    const doc = `intro\n\n${TABLE}\n`;
    const view = mount(doc, NEVER);
    const cells = Array.from(view.dom.querySelectorAll(".cm-md-table-widget td"));
    const two = cells.find((cell) => cell.textContent === "2")!;
    two.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    expect(view.state.selection.main.head).toBe(doc.indexOf("| 2 |") + 2);
    expect(view.dom.querySelector(".cm-md-table-widget")).toBeNull();
    // An empty cell takes the caret just past its pipe.
    view.dispatch({ selection: EditorSelection.cursor(0) });
    const empty = Array.from(view.dom.querySelectorAll(".cm-md-table-widget td"))[4];
    empty.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    expect(view.state.selection.main.head).toBe(doc.indexOf("| b |") + 6);
  });

  it("shows a failed diagram's source and the line its error names", async () => {
    const failing: BlockRenderer = {
      renderMath: NEVER.renderMath,
      renderDiagram: () => Promise.reject(Object.assign(new Error("Parse error on line 2"), { line: 2 })),
    };
    const view = mount(`intro\n\n${DIAGRAM}\n`, failing);
    await settle();
    const frame = view.dom.querySelector(".cm-md-diagram[data-status='failed']");
    expect(frame?.querySelector(".cm-md-block-source")?.textContent).toBe("graph TD\n  A --> B");
    expect(frame?.querySelector(".cm-md-block-caption")?.textContent).toBe(diagramFailureCaption(2));
  });

  it("says when the renderer did not load, and asks again once the wait is over", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    let attempts = 0;
    const flaky: BlockRenderer = {
      renderMath: NEVER.renderMath,
      renderDiagram: async () => {
        attempts++;
        if (attempts === 1) throw new BlockRenderError("diagram", "the diagram runtime did not load", null, "runtime");
        return `<svg class="drawn"><g></g></svg>`;
      },
    };
    const doc = `intro\n\n${DIAGRAM}\n\nend\n`;
    const view = mount(doc, flaky);
    await vi.advanceTimersByTimeAsync(40);
    const caption = view.dom.querySelector(".cm-md-diagram[data-status='failed'] .cm-md-block-caption");
    expect(caption?.textContent).toBe(DIAGRAM_RUNTIME_CAPTION);
    expect(caption?.textContent).not.toBe(diagramFailureCaption(null));
    // An edit inside the wait does not ask again.
    view.dispatch({ changes: { from: 0, insert: "a" } });
    await vi.advanceTimersByTimeAsync(TYPING_PAUSE_MS + 40);
    expect(attempts).toBe(1);
    await vi.advanceTimersByTimeAsync(5000);
    view.dispatch({ changes: { from: 0, insert: "b" } });
    await vi.advanceTimersByTimeAsync(TYPING_PAUSE_MS + 40);
    expect(attempts).toBe(2);
    expect(view.dom.querySelector(".cm-md-diagram[data-status='ready'] svg.drawn")).not.toBeNull();
  });

  it("asks once for a diagram whose source has an error", async () => {
    const renderDiagram = vi.fn(() => Promise.reject(new BlockRenderError("diagram", "Parse error on line 1", 1)));
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const view = mount(`intro\n\n${DIAGRAM}\n\nend\n`, { renderMath: NEVER.renderMath, renderDiagram });
    await vi.advanceTimersByTimeAsync(40);
    await vi.advanceTimersByTimeAsync(10_000);
    view.dispatch({ changes: { from: 0, insert: "a" } });
    await vi.advanceTimersByTimeAsync(TYPING_PAUSE_MS + 40);
    expect(renderDiagram).toHaveBeenCalledTimes(1);
    expect(view.dom.querySelector(".cm-md-block-caption")?.textContent).toBe(diagramFailureCaption(1));
  });

  it("holds a diagram's space across a theme switch, from its height in the other theme", async () => {
    const DRAWN = 183;
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      const height = this.classList.contains("cm-md-diagram") ? DRAWN : 0;
      return { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: height, width: 0, height, toJSON: () => ({}) } as DOMRect;
    });
    const source = "graph LR\n  Theme --> Switch";
    const doc = `intro\n\n\`\`\`mermaid\n${source}\n\`\`\`\n\nend\n`;
    const theme = new Compartment();
    const view = new EditorView({
      state: EditorState.create({
        doc,
        extensions: [
          language(),
          theme.of(EditorView.darkTheme.of(false)),
          markdownBlocks({
            renderMath: NEVER.renderMath,
            renderDiagram: (_source, polarity) =>
              polarity === "light" ? Promise.resolve(`<svg class="drawn"><g></g></svg>`) : new Promise(() => {}),
          }),
        ] as never,
      }),
      parent: document.body,
    });
    views.push(view);
    ensureSyntaxTree(view.state, doc.length, 5000);
    view.dispatch({ effects: setBlockWindow.of({ from: 0, to: doc.length }) });
    await settle();
    expect(measuredHeight(diagramKey(source, "light"))).toBe(DRAWN);
    expect(measuredHeight(diagramKey(source, "dark"))).toBeUndefined();
    view.dispatch({ effects: theme.reconfigure(EditorView.darkTheme.of(true)) });
    const pending = view.dom.querySelector<HTMLElement>(".cm-md-diagram[data-status='pending']");
    expect(pending?.style.minHeight).toBe(`${DRAWN}px`);
  });

  it("records drawings in one transaction per batch", async () => {
    const view = mount(`$a$ $b$ $c$\n\nend\n`, {
      renderMath: async (tex) => `<b>${tex}</b>`,
      renderDiagram: NEVER.renderDiagram,
    }, 12);
    const seen: number[] = [];
    const original = view.dispatch.bind(view);
    vi.spyOn(view, "dispatch").mockImplementation((...args: Parameters<EditorView["dispatch"]>) => {
      const spec = args[0] as { effects?: unknown };
      const effects = Array.isArray(spec.effects) ? spec.effects : spec.effects ? [spec.effects] : [];
      for (const effect of effects) {
        if ((effect as { is?: (t: unknown) => boolean }).is?.(setBlockRenders)) seen.push(1);
      }
      original(...args);
    });
    await settle();
    expect(seen.length).toBe(1);
    expect(view.state.field(blockRenderField).size).toBe(3);
  });

  it("holds the drawn ranges in the field", () => {
    const view = mount(`intro\n\n${TABLE}\n`, NEVER);
    expect(view.state.field(markdownBlockField).drawn).toHaveLength(1);
  });
});

describe("the Inline layer together", () => {
  it("draws the blocks beside the fences and the typography, and gives a table back on caret entry", async () => {
    const { markdownInlineExtension } = await import("../../editor/markdown-typography");
    const doc = `# Title\n\nSome **bold** and $x$ math.\n\n${TABLE}\n\n\`\`\`ts\nlet a = 1;\n\`\`\`\n\n${MATH}\n\n${DIAGRAM}\n\nEnd.\n`;
    const view = new EditorView({
      state: EditorState.create({
        doc,
        selection: EditorSelection.cursor(doc.length),
        extensions: [
          language(),
          markdownInlineExtension(
            {
              resolve: () => new Promise(() => {}),
              download: () => new Promise(() => {}),
              loadRemote: () => new Promise(() => {}),
              loadsRemoteImages: () => false,
            },
            {
              renderMath: async (tex) => `<span class="katex">${tex}</span>`,
              renderDiagram: async () => "<svg><g></g></svg>",
            },
          ),
        ] as never,
      }),
      parent: document.body,
    });
    views.push(view);
    ensureSyntaxTree(view.state, doc.length, 5000);
    view.dispatch({ effects: setBlockWindow.of({ from: 0, to: doc.length }) });
    await settle();
    expect(view.dom.querySelector(".cm-md-table-widget")).not.toBeNull();
    expect(view.dom.querySelector(".cm-md-math-block[data-status='ready']")).not.toBeNull();
    expect(view.dom.querySelector(".cm-md-diagram[data-status='ready'] svg")).not.toBeNull();
    expect(view.dom.querySelector(".cm-md-math-inline[data-status='ready']")?.textContent).toBe("x");

    view.dispatch({ selection: EditorSelection.cursor(doc.indexOf("| b |") + 2) });
    expect(view.dom.querySelector(".cm-md-table-widget")).toBeNull();
    const lines = Array.from(view.dom.querySelectorAll(".cm-line.cm-md-table")).map((l) => l.textContent);
    expect(lines).toContain("| b |  | [[Plan\\|the plan]] |");
  });
});
