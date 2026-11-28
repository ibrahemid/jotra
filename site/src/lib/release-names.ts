export interface ReleaseAssets {
  version: string;
  platforms: {
    mac: { dmg: string; pkg: string };
    win: { msi: string };
    linux: { deb: string; appImage: string };
  };
}

export interface ReleaseNames {
  assetPrefix: string;
  command: string;
  cask: string;
  appBundle: string;
  macExecutable: string;
  desktopFile: string;
  mcpServer: string;
  urlScheme: string;
  notesDirVariable: string;
  notesFolder: string;
  dataFolder: string;
  indexFile: string;
  newFilePrefix: string;
  macPkgFile: string;
  isCurrentName: boolean;
}

export interface ReleaseCommands {
  brewTrust: string;
  brewInstallCask: string;
  brewInstall: string;
  verifyPkg: string;
  mcpAdd: string;
  mcpConfig: string;
  xdgMimeDefault: string;
  cliExamples: string;
  pipeWithTitle: string;
}

export class ReleaseNamesError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ReleaseNamesError';
  }
}

const CURRENT_PREFIX = 'Jotra';
const ASSET_PREFIX = /^([A-Za-z]+)_/;

export function isShippedAsset(href: string): boolean {
  return href !== '' && href !== '#';
}

function getAssetFile(href: string): string {
  const file = href.slice(href.lastIndexOf('/') + 1);
  if (file === '') throw new ReleaseNamesError(`release asset URL ${href} names no file`);
  return file;
}

function getAssetPrefix(release: ReleaseAssets): string {
  const { mac, win, linux } = release.platforms;
  const shipped = [mac.pkg, mac.dmg, win.msi, linux.deb, linux.appImage].filter(isShippedAsset);
  const prefixes = [
    ...new Set(
      shipped.map((href) => {
        const file = getAssetFile(href);
        const match = ASSET_PREFIX.exec(file);
        if (!match?.[1]) throw new ReleaseNamesError(`release asset ${file} does not start with an app name and an underscore`);
        return match[1];
      }),
    ),
  ];
  const [prefix] = prefixes;
  if (prefix === undefined) throw new ReleaseNamesError(`release ${release.version} lists no asset to name the app from`);
  if (prefixes.length > 1) {
    throw new ReleaseNamesError(`release ${release.version} assets name more than one app: ${prefixes.join(', ')}`);
  }
  return prefix;
}

export function getReleaseNames(release: ReleaseAssets): ReleaseNames {
  const assetPrefix = getAssetPrefix(release);
  const lower = assetPrefix.toLowerCase();
  const pkg = release.platforms.mac.pkg;
  return {
    assetPrefix,
    command: lower,
    cask: lower,
    appBundle: `${assetPrefix}.app`,
    macExecutable: `/Applications/${assetPrefix}.app/Contents/MacOS/${lower}`,
    desktopFile: `${assetPrefix}.desktop`,
    mcpServer: lower,
    urlScheme: lower,
    notesDirVariable: `${assetPrefix.toUpperCase()}_NOTES_DIR`,
    notesFolder: `~/${assetPrefix}`,
    dataFolder: `~/.${lower}`,
    indexFile: `${lower}.db`,
    newFilePrefix: `${lower}-`,
    macPkgFile: isShippedAsset(pkg) ? getAssetFile(pkg) : `${assetPrefix}_${release.version}_universal.pkg`,
    isCurrentName: assetPrefix === CURRENT_PREFIX,
  };
}

export function getOfflineCondition(names: ReleaseNames): string {
  return names.isCurrentName ? 'Until you switch on Chat, Rewrite or Dictation, or load a web image' : 'Until Chat or Rewrite is on';
}

export function getReleaseCommands(names: ReleaseNames): ReleaseCommands {
  const brewTrust = 'brew trust ibrahemid/writ';
  const brewInstallCask = `brew install --cask ibrahemid/writ/${names.cask}`;
  const pipeWithTitle = `make test 2>&1 | ${names.command} --title tests`;
  const cliExamples = [
    [`${names.command} todo.txt`, '# open a file'],
    [`${names.command} .`, '# open the folder'],
    [`pbpaste | ${names.command}`, '# stdin becomes a new file'],
  ]
    .map(([run = '', note = '']) => `${run.padEnd(26)}${note}`)
    .concat(pipeWithTitle)
    .join('\n');
  return {
    brewTrust,
    brewInstallCask,
    brewInstall: `${brewTrust} && ${brewInstallCask}`,
    verifyPkg: `shasum -a 256 ${names.macPkgFile}`,
    mcpAdd: `claude mcp add --scope user ${names.mcpServer} -- "${names.macExecutable}" mcp`,
    mcpConfig: [
      '{',
      '  "mcpServers": {',
      `    ${JSON.stringify(names.mcpServer)}: {`,
      `      "command": ${JSON.stringify(names.macExecutable)},`,
      '      "args": ["mcp"]',
      '    }',
      '  }',
      '}',
    ].join('\n'),
    xdgMimeDefault: ['text/markdown', 'text/plain'].map((type) => `xdg-mime default ${names.desktopFile} ${type}`).join('\n'),
    cliExamples,
    pipeWithTitle,
  };
}
