import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const SITE = process.cwd();
const PAGE = readFileSync(join(SITE, 'src', 'pages', 'privacy.astro'), 'utf8');
const ROOT = join(SITE, '..');
const MCP_CLIENTS = readFileSync(join(ROOT, 'crates', 'jotra-core', 'src', 'mcp_clients.rs'), 'utf8');
const CHAT = readFileSync(join(ROOT, 'crates', 'jotra-ai', 'src', 'chat.rs'), 'utf8');
const OPEN_CONTEXT = readFileSync(join(ROOT, 'crates', 'jotra-core', 'src', 'open_context.rs'), 'utf8');
const NOTE_HOST = readFileSync(join(ROOT, 'crates', 'jotra-core', 'src', 'notes', 'host', 'mod.rs'), 'utf8');
const CLIENT_CARDS = readFileSync(join(ROOT, 'src', 'components', 'SettingsModal', 'apps', 'ClientCards.tsx'), 'utf8');
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

  it('says Add runs the program\'s own mcp add or hands it a link or a bundle, and edits no configuration', () => {
    expect(MCP_CLIENTS).toContain("Jotra never edits another program's configuration.");
    expect(CLIENT_CARDS).toMatch(/data-action="mcp-card-add"[\s\S]*?>\s*Add\s*<\/Button>/);
    expect(prose).toContain(
      "Add on a program's card runs that program's own <code>mcp add</code>, opens the install link the program handles, or hands it an extension bundle it installs itself; Jotra never edits another program's configuration.",
    );
  });

  it('gives the size and the count past which selected text handed to a program is left out', () => {
    const [, megabytes, selections] =
      /Selected text past (\d+)&nbsp;MB in all, or past (\d+) selections, is left out, and the answer says it was cut\./.exec(prose) ?? [];
    expect(OPEN_CONTEXT).toContain('pub const MAX_SELECTED_BYTES: usize = crate::notes::host::MAX_NOTE_BYTES as usize;');
    expect(NOTE_HOST).toContain(`pub const MAX_NOTE_BYTES: u64 = ${megabytes} * 1024 * 1024;`);
    expect(OPEN_CONTEXT).toContain(`pub const MAX_SELECTIONS: usize = ${selections};`);
  });

  it('says a chat edit outside the folder is recorded under its full path, or its name once its tab closed', () => {
    expect(CHAT).toMatch(/key: file\.to_string_lossy\(\)\.into_owned\(\),\s*file,\s*scope: ContextScope::OpenTab/);
    expect(CHAT).toMatch(/\.map\(\|context\| context\.key\)\s*\.unwrap_or_else\(\|_\| file_name_only\(path\)\);\s*record_proposal\(/);
    expect(prose).toContain(
      "A chat edit to a file outside your folder, applied or discarded, is recorded under that file's full path, or its file name alone when it is discarded after its tab closed.",
    );
  });
});
