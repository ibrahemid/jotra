import { describe, it, expect } from 'vitest';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  getOfflineCondition,
  getReleaseCommands,
  getReleaseNames,
  isShippedAsset,
  ReleaseNamesError,
  type ReleaseAssets,
} from '../lib/release-names';

const SITE = process.cwd();
const ROOT = join(SITE, '..');
const read = (...parts: string[]): string => readFileSync(join(ROOT, ...parts), 'utf8');
const DOWNLOAD = 'https://github.com/ibrahemid/jotra/releases/download/v1.1.0';

function releaseOf(version: string, files: { pkg: string; dmg: string; msi: string; deb: string; appImage: string }): ReleaseAssets {
  return {
    version,
    platforms: {
      mac: { pkg: files.pkg, dmg: files.dmg },
      win: { msi: files.msi },
      linux: { deb: files.deb, appImage: files.appImage },
    },
  };
}

const CURRENT = releaseOf('1.1.0', {
  pkg: `${DOWNLOAD}/Jotra_1.1.0_universal.pkg`,
  dmg: `${DOWNLOAD}/Jotra_1.1.0_universal.dmg`,
  msi: `${DOWNLOAD}/Jotra_1.1.0_x64_en-US.msi`,
  deb: `${DOWNLOAD}/Jotra_1.1.0_amd64.deb`,
  appImage: `${DOWNLOAD}/Jotra_1.1.0_amd64.AppImage`,
});

function pageSources(): { path: string; text: string }[] {
  const walk = (dir: string): string[] =>
    readdirSync(join(SITE, dir), { withFileTypes: true }).flatMap((entry) =>
      entry.isDirectory() ? walk(join(dir, entry.name)) : /\.(astro|ts)$/.test(entry.name) ? [join(dir, entry.name)] : [],
    );
  return [...walk(join('src', 'pages')), ...walk(join('src', 'components')), join('src', 'data', 'docs-topics.ts')].map((path) => ({
    path,
    text: readFileSync(join(SITE, path), 'utf8'),
  }));
}

describe('release names for a release published as Jotra', () => {
  it('derive every name from the asset prefix', () => {
    expect(getReleaseNames(CURRENT)).toEqual({
      assetPrefix: 'Jotra',
      command: 'jotra',
      cask: 'jotra',
      appBundle: 'Jotra.app',
      macExecutable: '/Applications/Jotra.app/Contents/MacOS/jotra',
      desktopFile: 'Jotra.desktop',
      mcpServer: 'jotra',
      urlScheme: 'jotra',
      notesDirVariable: 'JOTRA_NOTES_DIR',
      notesFolder: '~/Jotra',
      dataFolder: '~/.jotra',
      indexFile: 'jotra.db',
      newFilePrefix: 'jotra-',
      macPkgFile: 'Jotra_1.1.0_universal.pkg',
      isCurrentName: true,
    });
  });

  it('match the names the app on main uses', () => {
    const names = getReleaseNames(CURRENT);
    expect(read('crates', 'jotra-core', 'src', 'user_variables.rs')).toContain(`"${names.notesDirVariable}"`);
    expect(read('crates', 'jotra-core', 'src', 'notes', 'mod.rs')).toContain(`pub const DEFAULT_NOTES_FOLDER: &str = "${names.notesFolder.slice(2)}";`);
    expect(read('crates', 'jotra-core', 'src', 'notes', 'mod.rs')).toContain(`const MINTED_PREFIX: &str = "${names.newFilePrefix}";`);
    expect(read('crates', 'jotra-core', 'src', 'data_folder.rs')).toContain(`pub const DATA_FOLDER: &str = "${names.dataFolder.slice(2)}";`);
    expect(read('crates', 'jotra-core', 'src', 'data_folder.rs')).toContain(`pub const INDEX_FILE: &str = "${names.indexFile}";`);
    expect(read('src-tauri', 'tauri.conf.json')).toContain(`"productName": "${names.assetPrefix}"`);
    expect(existsSync(join(ROOT, 'packaging', 'homebrew', 'Casks', `${names.cask}.rb`))).toBe(true);
  });

  it('render each command a page shows', () => {
    expect(getReleaseCommands(getReleaseNames(CURRENT))).toEqual({
      brewTrust: 'brew trust ibrahemid/writ',
      brewInstallCask: 'brew install --cask ibrahemid/writ/jotra',
      brewInstall: 'brew trust ibrahemid/writ && brew install --cask ibrahemid/writ/jotra',
      verifyPkg: 'shasum -a 256 Jotra_1.1.0_universal.pkg',
      mcpAdd: 'claude mcp add --scope user jotra -- "/Applications/Jotra.app/Contents/MacOS/jotra" mcp',
      mcpConfig: [
        '{',
        '  "mcpServers": {',
        '    "jotra": {',
        '      "command": "/Applications/Jotra.app/Contents/MacOS/jotra",',
        '      "args": ["mcp"]',
        '    }',
        '  }',
        '}',
      ].join('\n'),
      xdgMimeDefault: 'xdg-mime default Jotra.desktop text/markdown\nxdg-mime default Jotra.desktop text/plain',
      cliExamples: [
        'jotra todo.txt            # open a file',
        'jotra .                   # open the folder',
        'pbpaste | jotra           # stdin becomes a new file',
        'make test 2>&1 | jotra --title tests',
      ].join('\n'),
      pipeWithTitle: 'make test 2>&1 | jotra --title tests',
    });
  });

  it('name what makes the app contact a host other than GitHub', () => {
    expect(getOfflineCondition(getReleaseNames(CURRENT))).toBe('Until you switch on Chat, Rewrite or Dictation, or load a web image');
  });

  it('render a config the JSON reader accepts', () => {
    const { mcpConfig } = getReleaseCommands(getReleaseNames(CURRENT));
    expect(JSON.parse(mcpConfig)).toEqual({
      mcpServers: { jotra: { command: '/Applications/Jotra.app/Contents/MacOS/jotra', args: ['mcp'] } },
    });
  });

  it('name the app from the files a release shipped when one is missing', () => {
    const partial = releaseOf('1.1.0', {
      pkg: '#',
      dmg: '',
      msi: `${DOWNLOAD}/Jotra_1.1.0_x64_en-US.msi`,
      deb: '#',
      appImage: '#',
    });
    const names = getReleaseNames(partial);
    expect(names.command).toBe('jotra');
    expect(names.macPkgFile).toBe('Jotra_1.1.0_universal.pkg');
  });

  it('refuse a release with no shipped file', () => {
    const empty = releaseOf('1.1.0', { pkg: '#', dmg: '#', msi: '', deb: '#', appImage: '#' });
    expect(() => getReleaseNames(empty)).toThrow(ReleaseNamesError);
    expect(() => getReleaseNames(empty)).toThrow('release 1.1.0 lists no asset to name the app from');
  });

  it('refuse an asset whose name carries no app name', () => {
    const unnamed = releaseOf('1.1.0', { ...CURRENT.platforms.mac, msi: `${DOWNLOAD}/SHA256SUMS.txt`, deb: '#', appImage: '#' });
    expect(() => getReleaseNames(unnamed)).toThrow('release asset SHA256SUMS.txt does not start with an app name and an underscore');
  });

  it('refuse an asset URL that names no file', () => {
    const bare = releaseOf('1.1.0', { ...CURRENT.platforms.mac, msi: `${DOWNLOAD}/`, deb: '#', appImage: '#' });
    expect(() => getReleaseNames(bare)).toThrow(`release asset URL ${DOWNLOAD}/ names no file`);
  });

  it('count a file as shipped unless the sync wrote nothing or a placeholder', () => {
    expect(isShippedAsset(`${DOWNLOAD}/Jotra_1.1.0_amd64.deb`)).toBe(true);
    expect(isShippedAsset('#')).toBe(false);
    expect(isShippedAsset('')).toBe(false);
  });
});

describe('pages that show a release name', () => {
  const CURRENT_LITERALS: RegExp[] = [
    /Jotra\.app\b/,
    /Jotra\.desktop\b/,
    /JOTRA_NOTES_DIR/,
    /<code>jotra\b/,
    /\bjotra:\/\//,
    /~\/Jotra\b/,
    /~\/\.jotra\b/,
    /\bjotra\.db\b/,
    /\| jotra\b/,
    /--cask \S+\/jotra\b/,
    /\bJotra_(?:\d|\{|\$)/,
    /\bjotra (?:mcp|todo|path|\.)/,
    /\bthe jotra command\b/i,
  ];

  it('take each one from the release names, never from a literal', () => {
    const hits = pageSources().flatMap(({ path, text }) =>
      CURRENT_LITERALS.filter((pattern) => pattern.test(text)).map((pattern) => `${path} ${pattern}`),
    );
    expect(hits).toEqual([]);
  });

  it('show each command from the release commands', () => {
    const source = (...parts: string[]): string => readFileSync(join(SITE, 'src', ...parts), 'utf8');
    expect(source('components', 'DownloadRows.astro')).toContain('command: commands.brewInstall,');
    expect(source('components', 'DownloadRows.astro')).toContain('{getOfflineCondition(names)}, Jotra contacts only GitHub, to check for updates');
    expect(source('pages', 'download.astro')).toContain('<pre><code>{commands.verifyPkg}</code></pre>');
    expect(source('pages', 'docs', 'install.astro')).toContain('<pre><code>{commands.brewTrust}\n{commands.brewInstallCask}\n');
    expect(source('pages', 'docs', 'cli.astro')).toContain('<pre><code>{commands.cliExamples}</code></pre>');
    const agent = source('pages', 'guides', 'agent-output.astro');
    for (const field of ['mcpAdd', 'mcpConfig', 'pipeWithTitle']) {
      expect(agent).toContain(`<pre><code>{commands.${field}}</code></pre>`);
    }
    expect(source('pages', 'guides', 'default-md-app.astro')).toContain('<pre><code>{commands.xdgMimeDefault}</code></pre>');
  });
});
