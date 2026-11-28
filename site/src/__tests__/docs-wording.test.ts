import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { NODE_CAP } from '../../../src/lib/graph/folder-graph';

const SITE = process.cwd();
const ROOT = join(SITE, '..');
const read = (...parts: string[]): string => readFileSync(join(ROOT, ...parts), 'utf8');
const page = (...parts: string[]): string => read('site', 'src', 'pages', ...parts).replace(/\s+/g, ' ');

describe('docs and guide sentences checked against the app', () => {
  it('says nothing in the Obsidian folder is copied, since Move… carries Jotra\'s own files into it', () => {
    const guide = page('guides', 'obsidian-vault-files.astro');
    expect(guide).not.toContain('nothing is copied or converted');
    expect(guide).toContain('nothing in it is copied or converted');
  });

  it('gives the Graph cap and the cut of a group larger than it', () => {
    const cap = NODE_CAP.toLocaleString('en-US');
    expect(page('docs', 'apps.astro')).toContain(
      `Past ${cap} files it draws the largest group of files that link to each other, cut to the ${cap} with the most links when that group is larger`,
    );
  });

  it('says Rewrite asks before sending the whole file, as the privacy page does', () => {
    const apps = page('docs', 'apps.astro');
    expect(apps).not.toContain('Only the selection is sent');
    expect(apps).toContain('with nothing selected, Jotra asks before sending the whole file.');
    expect(page('privacy.astro')).toContain('With nothing selected, Jotra asks before sending the whole file.');
  });

  it('names every path install.sh writes in the Uninstall steps', () => {
    const script = read('install.sh');
    const variable = (name: string): string => {
      const value = script.match(new RegExp(`^${name}="\\$\\{HOME\\}([^"]+)"$`, 'm'))?.[1];
      if (value === undefined) throw new Error(`install.sh no longer sets ${name} under HOME`);
      return `~${value}`;
    };
    const command = script.match(/^LEGACY_COMMAND="([^"]+)"$/m)?.[1];
    expect(command).toBeDefined();
    expect(script).toContain('install_bin "${TMP}/command-wrapper" "${BIN_DIR}/jotra"');
    const download = page('download.astro');
    for (const path of [
      variable('LIB_DIR'),
      variable('LEGACY_LIB_DIR'),
      `${variable('BIN_DIR')}/jotra`,
      `${variable('BIN_DIR')}/${command}`,
    ]) {
      expect(download, path).toContain(`<code>${path}</code>`);
    }
  });

  it('says a write to a folder JOTRA_NOTES_DIR names keeps no version', () => {
    expect(read('crates', 'jotra-core', 'src', 'user_variables.rs')).toContain('"JOTRA_NOTES_DIR"');
    expect(read('crates', 'jotra-mcp', 'src', 'tools.rs')).toContain('folder has no key there and keeps no version');
    const guide = page('guides', 'agent-output.astro');
    expect(guide).not.toContain('keeps the text every write replaces');
    expect(guide).toContain(
      'A program that starts the server with <code>{names.notesDirVariable}</code> naming another folder writes there with no version kept.',
    );
  });

  it('quotes the Markdown opens in setting by the title Settings shows', () => {
    const title = read('src', 'settings', 'index.ts').match(/id: "preview\.layout_md", section: "editor", title: "([^"]+)"/)?.[1];
    expect(title).toBe('Markdown opens in');
    const sentence = `Settings, Editor, “${title}” sets which one a Markdown file opens in.`;
    expect(page('docs', 'markdown.astro')).toContain(sentence);
    expect(read('docs', 'FEATURES.md')).toContain(sentence);
  });

  it('names Quick capture in docs/FEATURES.md with its default chord on each system', () => {
    const label = read('src', 'components', 'ShortcutEditor', 'any-app-chord.ts').match(/CAPTURE_ROW_LABEL = "([^"]+)"/)?.[1];
    expect(label).toBe('Quick capture');
    const chord = read('crates', 'jotra-core', 'src', 'config', 'mod.rs').match(
      /fn default_hotkey_capture\(\) -> String \{\s*"([^"]+)"/,
    )?.[1];
    expect(chord).toBe('CmdOrCtrl+Shift+Alt+Space');
    const mac = String(chord).replace('CmdOrCtrl', 'Cmd').replace('Alt', 'Option');
    const others = String(chord).replace('CmdOrCtrl', 'Ctrl');
    expect(read('docs', 'FEATURES.md')).toContain(
      `- ${label}, \`${mac}\` (\`${others}\` on Windows and Linux), opens a small window over any app. Escape adds what you typed to the end of Today's File;`,
    );
  });

  it('lists Compare and Undo of Revert To in docs/FEATURES.md by the labels the panel shows', () => {
    const panel = read('src', 'components', 'NoteHistory', 'NoteHistoryPanel.tsx');
    expect(panel).toMatch(/data-action="compare-version"[\s\S]*?>\s*Compare with current text\s*</);
    expect(panel).toMatch(/data-action="undo-restore"[\s\S]*?>\s*Undo\s*</);
    expect(read('docs', 'FEATURES.md')).toContain(
      '- In Revert To, Compare with current text shows how a version differs from the file as it is now, and Undo, after a restore, puts back the text the restore replaced.',
    );
  });
});
