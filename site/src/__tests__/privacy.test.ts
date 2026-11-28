import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { MAX_SELECTED_UNITS } from '../../../src/lib/open-context';

const SITE = process.cwd();
const PAGE = readFileSync(join(SITE, 'src', 'pages', 'privacy.astro'), 'utf8');
const ROOT = join(SITE, '..');
const MCP_CLIENTS = readFileSync(join(ROOT, 'crates', 'jotra-core', 'src', 'mcp_clients.rs'), 'utf8');
const CHAT = readFileSync(join(ROOT, 'crates', 'jotra-ai', 'src', 'chat.rs'), 'utf8');
const prose = PAGE.replace(/\s+/g, ' ');
const REMOTE_IMAGES_ADR = readFileSync(
  join(SITE, '..', 'docs', 'adr', '057-remote-images-load-on-request.md'),
  'utf8',
);

function captured(text: string, pattern: RegExp, source: string): string {
  const value = text.match(pattern)?.[1];
  if (value === undefined) throw new Error(`${source} has no match for ${pattern}`);
  return value;
}

function attribute(name: string): string {
  return captured(PAGE, new RegExp(`\\b${name}="([^"]*)"`), 'privacy.astro');
}

describe('privacy page', () => {
  it('names every server the app contacts in the page description', () => {
    const description = attribute('description');
    expect(PAGE).toContain('load a web image');
    expect(description).toContain('GitHub');
    expect(description).toMatch(/web image/);
  });

  it('was reviewed no earlier than the decision that let the app fetch web images', () => {
    const reviewed = captured(attribute('lead'), /Reviewed (\d{4}-\d{2}-\d{2})\./, 'The privacy lead');
    const accepted = captured(REMOTE_IMAGES_ADR, /Accepted, (\d{4}-\d{2}-\d{2})\./, 'ADR-057');
    expect(reviewed >= accepted).toBe(true);
  });

  it('says Connect runs the program\'s own mcp add or hands it a link or a bundle, and edits no configuration', () => {
    expect(MCP_CLIENTS).toContain("Jotra never edits another program's configuration.");
    expect(prose).toContain(
      "Connect on a program's card runs that program's own <code>mcp add</code>, opens the install link the program handles, or hands it an extension bundle it installs itself; Jotra never edits another program's configuration.",
    );
  });

  it('gives the length past which a selection handed to a program is cut', () => {
    const millions = Number(captured(prose, /A selection longer than about (\d+) million characters is cut there/, 'privacy.astro'));
    expect(millions).toBe(Math.round(MAX_SELECTED_UNITS / 1_000_000));
  });

  it('says a chat edit outside the folder is recorded under its full path', () => {
    expect(CHAT).toMatch(/key: file\.to_string_lossy\(\)\.into_owned\(\),\s*file,\s*scope: ContextScope::OpenTab/);
    expect(prose).toContain(
      "A chat edit to a file outside your folder, applied or discarded, is recorded under that file's full path.",
    );
  });
});
