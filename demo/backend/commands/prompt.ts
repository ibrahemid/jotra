import type { PackSource } from "../../../src/services/tauri";
import { sniffsBinary } from "../classify";
import { MAX_HOST_NOTE_BYTES, PackBuilder } from "../context-pack";
import { countUtf8Bytes } from "../history";
import { refuse, type Answer, type CommandTable, type DemoState } from "../state";
import { NOTES_ROOT } from "../vfs";

/** context_pack::ContextPackError::OutsideFolder. */
class PackOutsideFolderError extends Error {
  constructor(readonly path: string) {
    super(`${path} is outside the folder`);
    this.name = "PackOutsideFolderError";
  }
}

const trimTrailingSlashes = (path: string) => path.replace(/\/+$/, "");
const isInsideNotes = (path: string) => path.startsWith(`${NOTES_ROOT}/`);

export function createPromptCommands(state: DemoState): CommandTable {
  const { folder, index } = state;

  /** context_pack::pack_paths: the files `source` names, in the pack's order. */
  const listPackPaths = (source: PackSource): string[] => {
    switch (source.kind) {
      case "linked": {
        const key = trimTrailingSlashes(source.path);
        if (!isInsideNotes(key)) throw new PackOutsideFolderError(source.path);
        const paths = [key];
        for (const link of index.getFacts(key).links) {
          if (link.to_path !== null && !paths.includes(link.to_path)) paths.push(link.to_path);
        }
        return paths;
      }
      case "tag": {
        const tag = source.tag.trim().replace(/^#+/, "");
        return tag ? index.listPathsForTag(tag) : [];
      }
      case "folder": {
        const key = trimTrailingSlashes(source.path);
        if (key !== NOTES_ROOT && !isInsideNotes(key)) throw new PackOutsideFolderError(source.path);
        return index.listPaths().filter((path) => path.startsWith(`${key}/`));
      }
    }
  };

  /** context_pack::build_context_pack: each file read only while the pack has room. */
  const buildPack = (source: PackSource, live: { path: string; text: string }[]): Answer<"promptContextPack"> => {
    const paths = listPackPaths(source);
    const liveText = new Map(live.map((tab) => [trimTrailingSlashes(tab.path), tab.text]));
    const pack = new PackBuilder();
    for (const key of paths) {
      if (pack.isFull()) {
        pack.omit();
        continue;
      }
      const shown = isInsideNotes(key) ? key.slice(NOTES_ROOT.length + 1) : key;
      const text = liveText.get(key);
      if (text !== undefined) {
        if (countUtf8Bytes(text) > MAX_HOST_NOTE_BYTES) pack.skip(shown, "too_large");
        else pack.add(shown, text);
        continue;
      }
      if (!folder.has(key)) {
        pack.skip(shown, "unreadable");
        continue;
      }
      const bytes = state.readFileBytes(key);
      if (bytes.length > MAX_HOST_NOTE_BYTES) pack.skip(shown, "too_large");
      else if (sniffsBinary(bytes)) pack.skip(shown, "not_text");
      else pack.add(shown, folder.read(key));
    }
    return pack.finish();
  };

  return {
    prompt_context_pack: (args) => {
      try {
        return buildPack(args.source as PackSource, (args.live as { path: string; text: string }[] | undefined) ?? []);
      } catch (error) {
        if (error instanceof PackOutsideFolderError) return refuse(error.message);
        throw error;
      }
    },
  };
}
