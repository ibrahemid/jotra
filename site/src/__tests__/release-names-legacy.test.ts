import { describe, it, expect } from 'vitest';
import { getOfflineCondition, getReleaseCommands, getReleaseNames, type ReleaseAssets } from '../lib/release-names';

const DOWNLOAD = 'https://github.com/ibrahemid/writ/releases/download/v1.0.1';

const PUBLISHED: ReleaseAssets = {
  version: '1.0.1',
  platforms: {
    mac: { pkg: `${DOWNLOAD}/Writ_1.0.1_universal.pkg`, dmg: `${DOWNLOAD}/Writ_1.0.1_universal.dmg` },
    win: { msi: `${DOWNLOAD}/Writ_1.0.1_x64_en-US.msi` },
    linux: { deb: `${DOWNLOAD}/Writ_1.0.1_amd64.deb`, appImage: `${DOWNLOAD}/Writ_1.0.1_amd64.AppImage` },
  },
};

describe('release names for a release published as Writ', () => {
  it('derive every name from the asset prefix', () => {
    expect(getReleaseNames(PUBLISHED)).toEqual({
      assetPrefix: 'Writ',
      command: 'writ',
      cask: 'writ',
      appBundle: 'Writ.app',
      macExecutable: '/Applications/Writ.app/Contents/MacOS/writ',
      desktopFile: 'Writ.desktop',
      mcpServer: 'writ',
      urlScheme: 'writ',
      notesDirVariable: 'WRIT_NOTES_DIR',
      notesFolder: '~/Writ',
      dataFolder: '~/.writ',
      indexFile: 'writ.db',
      newFilePrefix: 'writ-',
      macPkgFile: 'Writ_1.0.1_universal.pkg',
      isCurrentName: false,
    });
  });

  it('render each command a page shows', () => {
    expect(getReleaseCommands(getReleaseNames(PUBLISHED))).toEqual({
      brewTrust: 'brew trust ibrahemid/writ',
      brewInstallCask: 'brew install --cask ibrahemid/writ/writ',
      brewInstall: 'brew trust ibrahemid/writ && brew install --cask ibrahemid/writ/writ',
      verifyPkg: 'shasum -a 256 Writ_1.0.1_universal.pkg',
      mcpAdd: 'claude mcp add --scope user writ -- "/Applications/Writ.app/Contents/MacOS/writ" mcp',
      mcpConfig: [
        '{',
        '  "mcpServers": {',
        '    "writ": {',
        '      "command": "/Applications/Writ.app/Contents/MacOS/writ",',
        '      "args": ["mcp"]',
        '    }',
        '  }',
        '}',
      ].join('\n'),
      xdgMimeDefault: 'xdg-mime default Writ.desktop text/markdown\nxdg-mime default Writ.desktop text/plain',
      cliExamples: [
        'writ todo.txt             # open a file',
        'writ .                    # open the folder',
        'pbpaste | writ            # stdin becomes a new file',
        'make test 2>&1 | writ --title tests',
      ].join('\n'),
      pipeWithTitle: 'make test 2>&1 | writ --title tests',
    });
  });

  it('name what makes the app contact a host other than GitHub', () => {
    expect(getOfflineCondition(getReleaseNames(PUBLISHED))).toBe('Until Chat or Rewrite is on');
  });

  it('refuse a release whose assets name two apps', () => {
    const mixed: ReleaseAssets = {
      ...PUBLISHED,
      platforms: { ...PUBLISHED.platforms, win: { msi: `${DOWNLOAD}/Jotra_1.0.1_x64_en-US.msi` } },
    };
    expect(() => getReleaseNames(mixed)).toThrow('release 1.0.1 assets name more than one app: Writ, Jotra');
  });
});
