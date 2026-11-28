import { describe, it, expect, afterEach, vi } from "vitest";
import { BlockRenderError, createBlockRenderer, diagramWindow } from "../../editor/block-renderer";
import { sanitizedFragment } from "../../editor/sanitize-markup";

function everyElement(root: DocumentFragment): Element[] {
  const found: Element[] = [];
  const walk = (parent: Element | DocumentFragment) => {
    for (const child of Array.from(parent.children)) {
      found.push(child);
      walk(child);
    }
  };
  walk(root);
  return found;
}

const CSS_LOAD = /url\s*\((?!\s*["']?#)|image-set\s*\(|@import/i;

/** True when nothing in the fragment could run, load or navigate. */
function inert(fragment: DocumentFragment): boolean {
  return everyElement(fragment).every((element) => {
    const name = element.localName.toLowerCase();
    if (["script", "iframe", "object", "embed", "animate", "set", "link"].includes(name)) return false;
    if (name === "style" && CSS_LOAD.test(element.textContent ?? "")) return false;
    return Array.from(element.attributes).every((attribute) => {
      const attr = attribute.name.toLowerCase();
      if (attr.startsWith("on")) return false;
      if (CSS_LOAD.test(attribute.value)) return false;
      if (attr === "srcset") return false;
      if (["href", "xlink:href", "src", "action", "formaction"].includes(attr)) {
        return attribute.value.trim().startsWith("#");
      }
      return true;
    });
  });
}

const HOSTILE_SVG = [
  '<svg viewBox="0 0 10 10" onload="alert(1)">',
  "<style>.node { fill: red; }</style>",
  "<style>@import url(https://evil.example/x.css); .edge { fill: blue; }</style>",
  "<script>alert(1)</script>",
  '<a href="javascript:alert(1)"><text onclick="alert(2)">t</text></a>',
  '<a xlink:href="javascript:alert(3)"><animate attributeName="href" values="javascript:alert(4)"></animate></a>',
  '<use href="https://evil.example/sprite.svg#x"></use>',
  '<path marker-end="url(#arrow)" d="M0 0L1 1"></path>',
  '<foreignObject><div><img src="x" onerror="alert(5)">label</div></foreignObject>',
  "</svg>",
].join("");

describe("sanitizing what a renderer drew", () => {
  it("leaves nothing that runs, loads or navigates", () => {
    const clean = sanitizedFragment(HOSTILE_SVG);
    expect(inert(clean)).toBe(true);
    const text = new XMLSerializer().serializeToString(clean);
    expect(text).not.toContain("javascript:");
    expect(text).not.toContain("@import");
    expect(text).not.toContain("evil.example");
  });

  it("keeps the drawing itself", () => {
    const root = sanitizedFragment(HOSTILE_SVG);
    expect(root.querySelector("svg")?.getAttribute("viewBox")).toBe("0 0 10 10");
    expect(Array.from(root.querySelectorAll("style"), (style) => style.textContent)).toEqual([".node { fill: red; }"]);
    expect(root.querySelector("path")?.getAttribute("marker-end")).toBe("url(#arrow)");
    expect(root.querySelector("foreignObject div")?.textContent).toBe("label");
  });

  it.each([
    ["a url() in a style element", "<svg><style>rect{fill:url(https://example.invalid/p.png)}</style></svg>"],
    ["a url() in a fill", '<svg><rect fill="url(https://example.invalid/q#g)"></rect></svg>'],
    ["an import put back together by stripping it", "<svg><style>@im@import a;port url(https://example.invalid/i.css);</style></svg>"],
    ["an import of a bare string", '<svg><style>@import "https://example.invalid/s.css";</style></svg>'],
    ["an escaped url() in a style element", "<svg><style>rect{fill:\\75 rl(https://example.invalid/e.png)}</style></svg>"],
    ["an escaped import", '<svg><style>@\\69mport "https://example.invalid/e.css";</style></svg>'],
    ["an escaped url() in a fill", '<svg><rect fill="u\\72l(https://example.invalid/f.png)"></rect></svg>'],
    ["an image-set() in a style attribute", `<div style="background-image:image-set('https://example.invalid/a.png' 1x)"></div>`],
    ["an image-set() in a style element", '<svg><style>g{background:-webkit-image-set("https://example.invalid/b.png" 1x)}</style></svg>'],
    [
      "url() in filter, clip-path, mask and marker-start",
      '<svg><g filter="url(https://example.invalid/a#f)" clip-path="url(\'https://example.invalid/b#c\')" ' +
        'mask="url(&quot;https://example.invalid/c#m&quot;)" marker-start="url(https://example.invalid/d#s)"></g></svg>',
    ],
    ["a url() split by an element inside a style", "<svg><style>rect{fill:ur<g>x</g>l(https://example.invalid/k.png)}</style></svg>"],
    ["a hex escape followed by CR LF in a style element", "<svg><style>rect{fill:\\75\r\nrl(https://example.invalid/n.png)}</style></svg>"],
    ["a hex escape followed by CR LF in a fill", '<svg><rect fill="\\75&#13;&#10;rl(https://example.invalid/o.png)"></rect></svg>'],
    ["a hex escape followed by FF in a style attribute", '<svg><rect style="fill:\\75\frl(https://example.invalid/p.png)"></rect></svg>'],
    ["a srcset whose second candidate leaves", "<svg><foreignObject><img srcset=\"#a 1x, https://example.invalid/m.png 2x\"></foreignObject></svg>"],
  ])("lets no URL out through %s", (_case, markup) => {
    const clean = sanitizedFragment(markup);
    expect(inert(clean)).toBe(true);
    expect(new XMLSerializer().serializeToString(clean)).not.toContain("example.invalid");
  });

  it("takes no url() written with escapes at its word", () => {
    const root = sanitizedFragment("<svg><style>rect{fill:u\\72l(\\22 #g\\22 )}</style></svg>");
    expect(root.querySelector("style")).toBeNull();
    const odd = sanitizedFragment('<svg><rect fill="\\ffffff \\0 \\d800 red"></rect></svg>');
    expect(odd.querySelector("rect")?.getAttribute("fill")).toBe("\\ffffff \\0 \\d800 red");
  });

  it("drops a whole style element that loads, and keeps one that only points inside the drawing", () => {
    const root = sanitizedFragment(
      "<svg><style>.a{fill:red}@import url(https://example.invalid/x.css);.b{fill:blue}</style>" +
        "<style>.node rect{fill:url(#grad)} .edge{marker-end:url( '#arrow' )}</style></svg>",
    );
    expect(Array.from(root.querySelectorAll("style"), (style) => style.textContent)).toEqual([
      ".node rect{fill:url(#grad)} .edge{marker-end:url( '#arrow' )}",
    ]);
  });

  it("keeps text a title, alt or ARIA label shows, and still drops the same url() as CSS", () => {
    const root = sanitizedFragment(
      '<svg><rect title="see url(https://example.invalid/t)" aria-label="url(https://example.invalid/l)" ' +
        'aria-description="a url( in prose" fill="url(https://example.invalid/f)" ' +
        'style="fill:url(https://example.invalid/s)"></rect>' +
        '<foreignObject><img alt="url(https://example.invalid/a)" src="https://example.invalid/i.png"></foreignObject></svg>',
    );
    const rect = root.querySelector("rect")!;
    expect(rect.getAttribute("title")).toBe("see url(https://example.invalid/t)");
    expect(rect.getAttribute("aria-label")).toBe("url(https://example.invalid/l)");
    expect(rect.getAttribute("aria-description")).toBe("a url( in prose");
    expect(rect.hasAttribute("fill")).toBe(false);
    expect(rect.hasAttribute("style")).toBe(false);
    const image = root.querySelector("img")!;
    expect(image.getAttribute("alt")).toBe("url(https://example.invalid/a)");
    expect(image.hasAttribute("src")).toBe(false);
  });

  it("gives every id and its references a suffix, and lets nothing out through them", () => {
    const root = sanitizedFragment(
      '<svg id="a" aria-labelledby="t other"><title id="t">x</title><marker id="m"></marker>' +
        '<path marker-end="url(#m)" fill="url(#elsewhere)"></path><use href="#m"></use><use xlink:href="#a"></use>' +
        "<style>#a .n{stroke:url(#m)} #ab{fill:red}</style>" +
        '<rect id="r" fill="url(https://example.invalid/x#r)"></rect>' +
        "<style>#r{fill:url(https://example.invalid/y)}</style></svg>",
      "-d7",
    );
    expect(inert(root)).toBe(true);
    expect(new XMLSerializer().serializeToString(root)).not.toContain("example.invalid");
    expect(Array.from(root.querySelectorAll("[id]"), (element) => element.id)).toEqual(["a-d7", "t-d7", "m-d7", "r-d7"]);
    expect(root.querySelector("svg")?.getAttribute("aria-labelledby")).toBe("t-d7 other");
    expect(root.querySelector("path")?.getAttribute("marker-end")).toBe("url(#m-d7)");
    expect(root.querySelector("path")?.getAttribute("fill")).toBe("url(#elsewhere)");
    expect(Array.from(root.querySelectorAll("use"), (use) => use.getAttribute("href") ?? use.getAttribute("xlink:href"))).toEqual([
      "#m-d7",
      "#a-d7",
    ]);
    expect(Array.from(root.querySelectorAll("style"), (style) => style.textContent)).toEqual([
      "#a-d7 .n{stroke:url(#m-d7)} #ab{fill:red}",
    ]);
  });

  it("keeps references to the drawing's own filters, clips, masks and markers", () => {
    const root = sanitizedFragment(
      '<svg><g filter="url(#f)" clip-path="url(\'#c\')" mask="url(&quot;#m&quot;)" marker-start="url( #s )"></g></svg>',
    );
    const group = root.querySelector("g");
    expect(group?.getAttribute("filter")).toBe("url(#f)");
    expect(group?.getAttribute("clip-path")).toBe("url('#c')");
    expect(group?.getAttribute("mask")).toBe('url("#m")');
    expect(group?.getAttribute("marker-start")).toBe("url( #s )");
  });
});

describe("math", () => {
  it("typesets with KaTeX and refuses commands that reach outside the expression", async () => {
    const renderer = createBlockRenderer();
    const markup = await renderer.renderMath("x^2", false);
    expect(markup).toContain("katex");
    for (const tex of [
      "\\href{javascript:alert(1)}{x}",
      "\\url{javascript:alert(1)}",
      "\\htmlClass{x}{y} \\htmlData{onclick=alert(1)}{z}",
      "\\text{<script>alert(1)</script>}",
      "\\includegraphics{https://evil.example/a.png}",
    ]) {
      const hostile = sanitizedFragment(await renderer.renderMath(tex, true));
      expect(inert(hostile), tex).toBe(true);
      const text = new XMLSerializer().serializeToString(hostile);
      expect(text).not.toContain("<script");
      expect(text).not.toContain('href="javascript');
      expect(text).not.toContain('src="https://evil');
    }
  });

  it("draws one expression once however often it is asked for", () => {
    const renderer = createBlockRenderer();
    expect(renderer.renderMath("a+b", true)).toBe(renderer.renderMath("a+b", true));
    expect(renderer.renderMath("a+b", true)).not.toBe(renderer.renderMath("a+b", false));
  });
});

interface FakeMermaid {
  initialize: ReturnType<typeof vi.fn>;
  parse: ReturnType<typeof vi.fn>;
  render: ReturnType<typeof vi.fn>;
}

function installMermaid(fake: FakeMermaid): void {
  (diagramWindow() as unknown as { mermaid?: FakeMermaid }).mermaid = fake;
}

afterEach(() => {
  delete (diagramWindow() as unknown as { mermaid?: unknown }).mermaid;
  delete (window as unknown as { mermaid?: unknown }).mermaid;
  for (const script of Array.from(diagramWindow().document.querySelectorAll("script"))) script.remove();
  vi.restoreAllMocks();
});

async function scriptIn(scope: Document): Promise<HTMLScriptElement> {
  for (let tries = 0; tries < 100; tries++) {
    const script = scope.head.querySelector("script");
    if (script) return script;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error("no script was added");
}

const RAW_DRAWING = '<svg onload="alert(1)"><foreignObject><img src="x" onerror="alert(2)"></foreignObject></svg>';

describe("diagrams", () => {
  it("draws one at a time, strict, in the theme asked for", async () => {
    let drawing = 0;
    let overlapped = false;
    const fake: FakeMermaid = {
      initialize: vi.fn(),
      parse: vi.fn(async () => true),
      render: vi.fn(async (id: string) => {
        drawing++;
        if (drawing > 1) overlapped = true;
        await new Promise((resolve) => setTimeout(resolve, 5));
        drawing--;
        return { svg: `<svg id="${id}" onclick="alert(1)"><g></g></svg>` };
      }),
    };
    installMermaid(fake);
    const renderer = createBlockRenderer();
    const [light, dark] = await Promise.all([
      renderer.renderDiagram("graph TD\nA-->B", "light"),
      renderer.renderDiagram("graph TD\nA-->C", "dark"),
    ]);
    expect(overlapped).toBe(false);
    expect(fake.initialize).toHaveBeenNthCalledWith(1, expect.objectContaining({ securityLevel: "strict", theme: "default" }));
    expect(fake.initialize).toHaveBeenNthCalledWith(2, expect.objectContaining({ securityLevel: "strict", theme: "dark" }));
    expect(inert(sanitizedFragment(light))).toBe(true);
    expect(dark).toContain("<svg");
  });

  it("builds the raw drawing in a frame of its own, never in the editor's document", async () => {
    const onPage = vi.fn();
    (window as unknown as { mermaid?: unknown }).mermaid = { initialize: onPage, parse: onPage, render: onPage };
    const rawInEditor: boolean[] = [];
    const fake: FakeMermaid = {
      initialize: vi.fn(),
      parse: vi.fn(async () => true),
      render: vi.fn(async (id: string) => {
        // Mermaid lays the drawing out in a `d<id>` holder in its own document's body.
        const scope = diagramWindow().document;
        const holder = scope.createElement("div");
        holder.id = `d${id}`;
        holder.innerHTML = RAW_DRAWING;
        scope.body.appendChild(holder);
        rawInEditor.push(document.querySelector("[onload], [onerror]") !== null);
        return { svg: holder.innerHTML };
      }),
    };
    installMermaid(fake);
    const renderer = createBlockRenderer();
    const first = await renderer.renderDiagram("graph TD\nA-->B", "light");
    await renderer.renderDiagram("graph TD\nA-->C", "light");
    expect(onPage).not.toHaveBeenCalled();
    expect(fake.render).toHaveBeenCalledTimes(2);
    expect(rawInEditor).toEqual([false, false]);
    expect(diagramWindow().document.querySelector("[onload], [onerror]")).toBeNull();
    expect(first).toContain("onerror");
    expect(inert(sanitizedFragment(first))).toBe(true);
    const frames = document.querySelectorAll("iframe");
    expect(frames).toHaveLength(1);
    expect(frames[0].contentWindow).toBe(diagramWindow());
    expect(frames[0].getAttribute("aria-hidden")).toBe("true");
    expect(frames[0].hasAttribute("src")).toBe(false);
  });

  it("fetches the runtime into the frame, and says so when it does not load", async () => {
    const renderer = createBlockRenderer();
    const scope = diagramWindow().document;
    const editorScripts = document.head.querySelectorAll("script").length;
    const first = renderer.renderDiagram("graph TD\nA-->B", "light").catch((e: unknown) => e);
    const queued = renderer.renderDiagram("graph TD\nA-->C", "light").catch((e: unknown) => e);
    const script = await scriptIn(scope);
    expect(script.src).toMatch(/mermaid\.min\.js/);
    expect(document.head.querySelectorAll("script")).toHaveLength(editorScripts);
    script.dispatchEvent(new Event("error"));
    for (const failure of [await first, await queued]) {
      expect(failure).toBeInstanceOf(BlockRenderError);
      expect((failure as BlockRenderError).reason).toBe("runtime");
    }
    // The queued diagram failed with the load, not with a second fetch.
    expect(scope.head.querySelector("script")).toBeNull();

    const later = Date.now() + 2000;
    vi.spyOn(Date, "now").mockReturnValue(later);
    const retry = renderer.renderDiagram("graph TD\nA-->B", "light");
    const again = await scriptIn(scope);
    installMermaid({
      initialize: vi.fn(),
      parse: vi.fn(async () => true),
      render: vi.fn(async () => ({ svg: "<svg><g></g></svg>" })),
    });
    again.dispatchEvent(new Event("load"));
    expect(await retry).toBe("<svg><g></g></svg>");
    expect(document.head.querySelectorAll("script")).toHaveLength(editorScripts);
  });

  it("names the line a diagram's error is on", async () => {
    installMermaid({
      initialize: vi.fn(),
      parse: vi.fn(async () => {
        throw new Error("Parse error on line 3:\n...A-->\n---^");
      }),
      render: vi.fn(),
    });
    const renderer = createBlockRenderer();
    const failure = await renderer.renderDiagram("graph TD\nA-->B\nA-->", "light").catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(BlockRenderError);
    expect((failure as BlockRenderError).line).toBe(3);
    expect((failure as BlockRenderError).reason).toBe("source");
  });
});

describe("a renderer that did not load", () => {
  it("is told apart from an expression with an error", async () => {
    vi.resetModules();
    vi.doMock("katex", () => {
      throw new Error("the chunk did not load");
    });
    try {
      const fresh = await import("../../editor/block-renderer");
      const failure = await fresh.createBlockRenderer().renderMath("x", false).catch((e: unknown) => e);
      expect(failure).toBeInstanceOf(fresh.BlockRenderError);
      expect((failure as BlockRenderError).reason).toBe("runtime");
    } finally {
      vi.doUnmock("katex");
      vi.resetModules();
    }
  });
});
