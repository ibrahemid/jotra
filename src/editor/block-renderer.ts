import mermaidRuntimeUrl from "mermaid/dist/mermaid.min.js?url";
import { logFailure } from "../lib/log";
import { typesetMath, type MathAnswer } from "./math-render";

/** Which of the two diagram themes a drawing is made in. */
export type BlockTheme = "light" | "dark";

/**
 * What draws the blocks Inline renders: KaTeX for math and Mermaid for
 * diagrams, the runtimes and versions the preview ships. An answer is the
 * renderer's markup as it came out: it reaches the page only through
 * `sanitizedFragment` (editor/sanitize-markup.ts).
 */
export interface BlockRenderer {
  renderMath(tex: string, display: boolean): Promise<string>;
  renderDiagram(source: string, theme: BlockTheme): Promise<string>;
}

/**
 * Why a block could not be drawn: its source has an error, or the renderer
 * itself did not load, which a later attempt may get past.
 */
export type BlockFailureReason = "source" | "runtime";

/** A block that could not be drawn. `line` is the diagram line an error names. */
export class BlockRenderError extends Error {
  constructor(
    readonly kind: "math" | "diagram",
    message: string,
    readonly line: number | null = null,
    readonly reason: BlockFailureReason = "source",
  ) {
    super(message);
    this.name = "BlockRenderError";
  }
}

// ─── Math ──────────────────────────────────────────────────────────────────

// Singleton state — Jotra is single-window
let mathStyles: Promise<unknown> | null = null;

/** KaTeX's stylesheet and fonts, fetched the first time math is drawn. */
function loadMathStyles(): void {
  mathStyles ??= import("katex/dist/katex.min.css").catch(() => {
    mathStyles = null;
    logFailure("the math stylesheet could not be loaded");
  });
}

async function typesetOnPage(tex: string, display: boolean): Promise<string> {
  let katex: typeof import("katex").default;
  try {
    katex = (await import("katex")).default;
  } catch {
    throw new BlockRenderError("math", "the math renderer did not load", null, "runtime");
  }
  return typesetMath(katex, tex, display);
}

interface MathJob {
  tex: string;
  display: boolean;
  resolve(markup: string): void;
  reject(error: unknown): void;
}

/**
 * Typesets in a worker, so a screen of math costs the page nothing while it
 * is drawn. A webview that cannot start the worker, or a worker that fails,
 * hands its work to the page.
 */
class MathTypesetter {
  private worker: Worker | null | undefined;
  private nextId = 0;
  private readonly waiting = new Map<number, MathJob>();

  typeset(tex: string, display: boolean): Promise<string> {
    loadMathStyles();
    const worker = this.worker === undefined ? this.start() : this.worker;
    if (worker === null) return typesetOnPage(tex, display);
    return new Promise<string>((resolve, reject) => {
      const id = ++this.nextId;
      this.waiting.set(id, { tex, display, resolve, reject });
      worker.postMessage({ id, tex, display });
    });
  }

  private start(): Worker | null {
    if (typeof Worker === "undefined") {
      this.worker = null;
      return null;
    }
    try {
      const worker = new Worker(new URL("./math.worker.ts", import.meta.url));
      worker.onmessage = (event: MessageEvent<MathAnswer>) => this.answer(event.data);
      worker.onerror = () => this.fallBack();
      this.worker = worker;
    } catch {
      logFailure("math is typeset on the page: the worker could not start");
      this.worker = null;
    }
    return this.worker;
  }

  private answer(answer: MathAnswer): void {
    const job = this.waiting.get(answer.id);
    if (!job) return;
    this.waiting.delete(answer.id);
    if ("markup" in answer) job.resolve(answer.markup);
    else job.reject(new BlockRenderError("math", answer.error));
  }

  private fallBack(): void {
    logFailure("math is typeset on the page: the worker stopped");
    this.worker?.terminate();
    this.worker = null;
    const jobs = [...this.waiting.values()];
    this.waiting.clear();
    for (const job of jobs) typesetOnPage(job.tex, job.display).then(job.resolve, job.reject);
  }
}

// ─── Diagrams ──────────────────────────────────────────────────────────────

interface MermaidApi {
  initialize(config: Record<string, unknown>): void;
  parse(text: string): Promise<unknown>;
  render(id: string, text: string): Promise<{ svg: string }>;
}

function mermaidIn(scope: Window): MermaidApi | undefined {
  return (scope as unknown as { mermaid?: MermaidApi }).mermaid;
}

// The app's own reset, so text is measured in the frame as the editor shows it.
const FRAME_RESET = "*,*::before,*::after{margin:0;padding:0;box-sizing:border-box}";

// Singleton state — Jotra is single-window
let drawingFrame: HTMLIFrameElement | null = null;

/**
 * The window diagrams are drawn in: a frame with no address of its own, so it
 * keeps the app's origin and policy, laid out out of sight and never removed.
 * Mermaid builds a drawing in the document it runs in, so the raw markup lands
 * in this frame's document, never in the editor's, which receives only what
 * `sanitizedFragment` keeps of it.
 */
export function diagramWindow(): Window {
  const held = drawingFrame?.contentWindow;
  if (held) return held;
  const frame = document.createElement("iframe");
  frame.setAttribute("aria-hidden", "true");
  frame.tabIndex = -1;
  frame.style.cssText =
    "position:fixed;left:-10000px;top:0;width:1200px;height:900px;border:0;visibility:hidden;pointer-events:none";
  document.body.appendChild(frame);
  const scope = frame.contentWindow;
  if (!scope) {
    frame.remove();
    throw new BlockRenderError("diagram", "the diagram frame did not open", null, "runtime");
  }
  const reset = scope.document.createElement("style");
  reset.textContent = FRAME_RESET;
  scope.document.head.appendChild(reset);
  drawingFrame = frame;
  return scope;
}

/** How long the drawer answers at once, after the runtime failed to load, before it fetches it again. */
const RUNTIME_RELOAD_MS = 1000;

function nextTask(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function errorLine(error: unknown): number | null {
  const message = error instanceof Error ? error.message : String(error);
  const match = /\bline (\d+)/i.exec(message);
  return match ? Number(match[1]) : null;
}

/**
 * Draws diagrams one at a time, each in a task of its own, so a document of
 * diagrams never holds the page for longer than one of them takes. Mermaid
 * lays a diagram out by measuring text in a document, which a worker does
 * not have: this is the one part of a block's drawing that runs on the
 * page's thread, in `diagramWindow`.
 *
 * The runtime is the file the preview loads (`mermaid.min.js`), fetched into
 * that window the first time a diagram is drawn, and set up as the preview
 * sets it up: `securityLevel: "strict"` and a theme for each polarity.
 */
class DiagramDrawer {
  private runtime: Promise<MermaidApi> | null = null;
  private queue: Promise<unknown> = Promise.resolve();
  private theme: BlockTheme | null = null;
  private drawn = 0;
  private failedLoadAt = Number.NEGATIVE_INFINITY;

  draw(source: string, theme: BlockTheme): Promise<string> {
    const job = this.queue.then(nextTask).then(() => this.drawNow(source, theme));
    this.queue = job.catch(() => undefined);
    return job;
  }

  private async drawNow(source: string, theme: BlockTheme): Promise<string> {
    const mermaid = await this.load();
    if (this.theme !== theme) {
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        // A directive in the file must not be able to set CSS in the drawing.
        secure: ["secure", "securityLevel", "startOnLoad", "maxTextSize", "suppressErrorRendering", "maxEdges", "themeCSS", "themeVariables", "fontFamily"],
        theme: theme === "dark" ? "dark" : "default",
      });
      this.theme = theme;
    }
    const id = `jotra-diagram-${++this.drawn}`;
    try {
      await mermaid.parse(source);
      const { svg } = await mermaid.render(id, source);
      return svg;
    } catch (error) {
      throw new BlockRenderError(
        "diagram",
        error instanceof Error ? error.message : String(error),
        errorLine(error),
      );
    } finally {
      // A failed render can leave its scratch element in the frame.
      const scratch = diagramWindow().document;
      scratch.getElementById(`d${id}`)?.remove();
      scratch.getElementById(id)?.remove();
    }
  }

  private load(): Promise<MermaidApi> {
    // The diagrams queued behind a failed load fail with it, not one fetch each.
    if (this.runtime === null && Date.now() - this.failedLoadAt < RUNTIME_RELOAD_MS) {
      return Promise.reject(new BlockRenderError("diagram", "the diagram runtime did not load", null, "runtime"));
    }
    this.runtime ??= new Promise<MermaidApi>((resolve, reject) => {
      const scope = diagramWindow();
      const present = mermaidIn(scope);
      if (present) {
        resolve(present);
        return;
      }
      const script = scope.document.createElement("script");
      script.src = new URL(mermaidRuntimeUrl, document.baseURI).href;
      script.async = true;
      script.addEventListener("load", () => {
        const loaded = mermaidIn(scope);
        if (loaded) {
          resolve(loaded);
          return;
        }
        script.remove();
        reject(new BlockRenderError("diagram", "the diagram runtime loaded without its API", null, "runtime"));
      });
      script.addEventListener("error", () => {
        script.remove();
        reject(new BlockRenderError("diagram", "the diagram runtime did not load", null, "runtime"));
      });
      scope.document.head.appendChild(script);
    }).catch((error: unknown) => {
      this.runtime = null;
      this.failedLoadAt = Date.now();
      logFailure("the diagram runtime could not be loaded");
      throw error;
    });
    return this.runtime;
  }
}

// ─── The renderer ──────────────────────────────────────────────────────────

/** How many drawings are kept, across every tab. */
const KEPT_DRAWINGS = 256;

/**
 * A renderer whose answers are kept by source: the same expression or
 * diagram in another tab, or after an undo, is drawn once. A failure is not
 * kept, so the next request draws again.
 */
export function createBlockRenderer(): BlockRenderer {
  const math = new MathTypesetter();
  const diagrams = new DiagramDrawer();
  const kept = new Map<string, Promise<string>>();

  function keep(key: string, draw: () => Promise<string>): Promise<string> {
    const held = kept.get(key);
    if (held) {
      kept.delete(key);
      kept.set(key, held);
      return held;
    }
    const drawing = draw();
    kept.set(key, drawing);
    drawing.catch(() => kept.delete(key));
    if (kept.size > KEPT_DRAWINGS) {
      const oldest = kept.keys().next().value;
      if (oldest !== undefined) kept.delete(oldest);
    }
    return drawing;
  }

  return {
    renderMath: (tex, display) =>
      keep(`math\u0000${display ? "d" : "i"}\u0000${tex}`, () => math.typeset(tex, display)),
    renderDiagram: (source, theme) =>
      keep(`diagram\u0000${theme}\u0000${source}`, () => diagrams.draw(source, theme)),
  };
}

// Singleton state — Jotra is single-window
let shared: BlockRenderer | null = null;

/** The renderer every editor in the window shares. */
export function sharedBlockRenderer(): BlockRenderer {
  shared ??= createBlockRenderer();
  return shared;
}
