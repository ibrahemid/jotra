// Elements that run, load or submit something, and the SVG animation
// elements, which can rewrite an attribute such as `href` after this pass.
const BLOCKED_ELEMENTS = new Set([
  "script",
  "iframe",
  "frame",
  "frameset",
  "object",
  "embed",
  "applet",
  "link",
  "meta",
  "base",
  "form",
  "input",
  "button",
  "textarea",
  "select",
  "audio",
  "video",
  "source",
  "track",
  "portal",
  "animate",
  "animatemotion",
  "animatetransform",
  "set",
  "discard",
]);

const URL_ATTRIBUTES = new Set([
  "href",
  "xlink:href",
  "src",
  "action",
  "formaction",
  "background",
  "poster",
  "data",
  "ping",
]);

// A fragment link inside the drawing, or an inline raster.
const SAFE_URL = /^(#|data:image\/(png|gif|jpe?g|webp);)/i;

// CSS that fetches: a url() other than a fragment of the drawing, the
// functions that take a bare string as an image, an import.
const CSS_FETCH = /url\s*\((?!\s*["']?#)|image-set\s*\(|image\s*\(|src\s*\(|@import|expression\s*\(/;
const CSS_URL = /url\s*\(/;
const CSS_ESCAPE = /\\(?:([0-9a-f]{1,6})[ \t\n\r\f]?|([\s\S]))/gi;

function decodeCssEscapes(text: string): string {
  // CSS turns CR LF, CR and FF into one LF before it reads escapes, so a hex
  // escape followed by CR LF swallows the pair as its one whitespace.
  const normalized = text.replace(/\r\n|\r|\f/g, "\n");
  return normalized.replace(CSS_ESCAPE, (_escape, hex: string | undefined, char: string | undefined) => {
    if (hex === undefined) return char ?? "";
    const code = Number.parseInt(hex, 16);
    const invalid = code === 0 || code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff);
    return invalid ? "\uFFFD" : String.fromCodePoint(code);
  });
}

/**
 * Whether CSS text could load anything, judged on the text as a CSS parser
 * reads it, escapes decoded. Text that fails is dropped whole, never
 * rewritten. A decoded escape can pass for a quote, so a url() in text with
 * escapes is refused even when it seems to name a fragment.
 */
function fetchesFromCss(text: string): boolean {
  const decoded = decodeCssEscapes(text).toLowerCase();
  return CSS_FETCH.test(decoded) || (text.includes("\\") && CSS_URL.test(decoded));
}

// Attributes no engine reads as CSS or as a URL: text a reader or a screen
// reader is shown, which may say "url(" without loading anything.
function isTextOnly(attr: string): boolean {
  return attr === "title" || attr === "alt" || attr.startsWith("aria-");
}

function clean(parent: Element | DocumentFragment): void {
  for (const element of Array.from(parent.children)) {
    const name = element.localName.toLowerCase();
    if (BLOCKED_ELEMENTS.has(name)) {
      element.remove();
      continue;
    }
    for (const attribute of Array.from(element.attributes)) {
      const attr = attribute.name.toLowerCase();
      if (
        attr.startsWith("on") ||
        attr === "srcset" ||
        (URL_ATTRIBUTES.has(attr) && !SAFE_URL.test(attribute.value.trim())) ||
        (!isTextOnly(attr) && fetchesFromCss(attribute.value))
      ) {
        element.removeAttribute(attribute.name);
      }
    }
    if (name === "style") {
      // The stylesheet is the element's own text nodes; with an element among
      // them, textContent is not that text.
      if (element.children.length > 0 || fetchesFromCss(element.textContent ?? "")) element.remove();
      continue;
    }
    clean(element);
  }
}

const FRAGMENT_URL = /url\(\s*(["']?)#([^"')\s]+)\1\s*\)/g;
const ID_REFERENCE_LISTS = new Set(["aria-labelledby", "aria-describedby", "aria-controls", "aria-owns"]);

function everyElementIn(root: DocumentFragment): Element[] {
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

function escapedForPattern(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Gives every id in the fragment `suffix`, and every reference to one of them
 * the same: a fragment link, a `url(#…)` in an attribute or a stylesheet, an
 * id selector, an ARIA id list. Two copies of one drawing then name their
 * markers, clips and styles apart.
 */
function suffixIds(root: DocumentFragment, suffix: string): void {
  const elements = everyElementIn(root);
  const renamed = new Map<string, string>();
  for (const element of elements) {
    const id = element.getAttribute("id");
    if (id === null || id === "") continue;
    const next = `${id}${suffix}`;
    renamed.set(id, next);
    element.setAttribute("id", next);
  }
  if (renamed.size === 0) return;
  const swapUrls = (text: string) =>
    text.replace(FRAGMENT_URL, (whole, quote: string, id: string) => {
      const next = renamed.get(id);
      return next === undefined ? whole : `url(${quote}#${next}${quote})`;
    });
  const ids = [...renamed.keys()].sort((a, b) => b.length - a.length).map(escapedForPattern);
  const selector = new RegExp(`#(${ids.join("|")})(?![\\w-])`, "g");
  const swapSelectors = (text: string) => text.replace(selector, (_whole, id: string) => `#${renamed.get(id)}`);
  for (const element of elements) {
    for (const attribute of Array.from(element.attributes)) {
      const attr = attribute.name.toLowerCase();
      const value = attribute.value;
      let next = value;
      if ((attr === "href" || attr === "xlink:href") && value.startsWith("#")) {
        const target = renamed.get(value.slice(1));
        if (target !== undefined) next = `#${target}`;
      } else if (ID_REFERENCE_LISTS.has(attr)) {
        next = value
          .split(/\s+/)
          .map((token) => renamed.get(token) ?? token)
          .join(" ");
      } else if (value.includes("url(")) {
        next = swapUrls(value);
      }
      if (next !== value) element.setAttribute(attribute.name, next);
    }
    if (element.localName.toLowerCase() === "style" && element.children.length === 0) {
      // An id selector and a `url(#…)` both read `#id` here.
      element.textContent = swapSelectors(element.textContent ?? "");
    }
  }
}

/**
 * Markup a renderer produced, parsed into an inert fragment with everything
 * that could run, load or navigate taken out. The editor's window can call
 * the app, so what goes into it is held to less than the preview's frame
 * allows: no script, no event handler, no link out of the drawing, and no
 * stylesheet or attribute whose CSS fetches anything.
 *
 * With `idSuffix`, every id in the markup and every reference to it carries
 * the suffix, so the same drawing can be in the document twice. The ids are
 * renamed before the cleaning pass, which judges the text as inserted.
 *
 * The fragment is what gets inserted, never markup serialized back out of it:
 * a second parse of the cleaned text could read it differently from the
 * first.
 */
export function sanitizedFragment(markup: string, idSuffix?: string): DocumentFragment {
  const template = document.createElement("template");
  template.innerHTML = markup;
  if (idSuffix !== undefined) suffixIds(template.content, idSuffix);
  clean(template.content);
  return template.content;
}
