import { describe, it, expect, beforeAll } from 'vitest';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ELEMENT_NODE, parse, walkSync, type ElementNode, type Node } from 'ultrahtml';

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const DIST = join(ROOT, 'dist');

function pages(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return entry === 'demo' ? [] : pages(full);
    return entry.endsWith('.html') ? [full] : [];
  });
}

const isKeycap = (node: Node | undefined): node is ElementNode =>
  node?.type === ELEMENT_NODE && node.name === 'kbd' && /(^|\s)kc(\s|$)/.test(node.attributes.class ?? '');

beforeAll(() => {
  expect(existsSync(DIST), 'dist/ is missing. This suite runs after `pnpm build`.').toBe(true);
});

describe('keycaps as built', () => {
  it('hold every chord of two or more keys in one span.chord, so it never breaks between keys', () => {
    const loose: string[] = [];
    for (const file of pages(DIST)) {
      walkSync(parse(readFileSync(file, 'utf8')), (node) => {
        if (node.type !== ELEMENT_NODE) return;
        const children: Node[] = node.children;
        children.forEach((child, i) => {
          if (!isKeycap(child) || !isKeycap(children[i + 1])) return;
          const inChord = node.name === 'span' && /(^|\s)chord(\s|$)/.test(node.attributes.class ?? '');
          if (!inChord) loose.push(`${relative(DIST, file)}: ${child.children.map((c: { value?: string }) => c.value ?? '').join('')}`);
        });
      });
    }
    expect(loose).toEqual([]);
  });
});
