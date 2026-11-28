import { Text } from "@codemirror/state";
import type { ContextPack, PackSkipReason } from "../../src/services/tauri";
import { countText } from "../../src/editor/document-counts";
import { frontmatterEnd } from "../../src/lib/frontmatter";
import { estimateTokens } from "../../src/lib/token-estimate";
import { countUtf8Bytes } from "./history";

/** jotra_core::prompt::pack::MAX_PACK_FILES. */
export const MAX_PACK_FILES = 100;

/** jotra_core::prompt::pack::MAX_PACK_BYTES. */
export const MAX_PACK_BYTES = 4 * 1024 * 1024;

/** jotra_core::notes::host::MAX_NOTE_BYTES: the largest file read for a program or a prompt. */
export const MAX_HOST_NOTE_BYTES = 2 * 1024 * 1024;

function splitInclusive(text: string): string[] {
  const lines: string[] = [];
  let start = 0;
  for (let at = text.indexOf("\n"); at !== -1; at = text.indexOf("\n", start)) {
    lines.push(text.slice(start, at + 1));
    start = at + 1;
  }
  if (start < text.length) lines.push(text.slice(start));
  return lines;
}

interface CommentScan {
  isInComment: boolean;
}

function scanTextSegment(segment: string, segmentStart: number, fullInput: string, scan: CommentScan): string {
  let out = "";
  let rest = segment;
  let restStart = segmentStart;
  for (;;) {
    const open = rest.indexOf("<!--");
    if (open === -1) return out + rest;
    const afterOpen = open + 4;
    const close = rest.indexOf("-->", afterOpen);
    if (close === -1) {
      if (fullInput.includes("-->", restStart + afterOpen)) {
        scan.isInComment = true;
        return out + rest.slice(0, open);
      }
      return out + rest;
    }
    out += rest.slice(0, open);
    const resume = close + 3;
    restStart += resume;
    rest = rest.slice(resume);
  }
}

function stripCommentsOutsideFences(input: string): string {
  let out = "";
  let fence: string | null = null;
  const scan: CommentScan = { isInComment: false };
  let offset = 0;
  for (const line of splitInclusive(input)) {
    const lineStart = offset;
    offset += line.length;
    if (scan.isInComment) {
      const close = line.indexOf("-->");
      if (close !== -1) {
        scan.isInComment = false;
        const resume = close + 3;
        out += scanTextSegment(line.slice(resume), lineStart + resume, input, scan);
      }
      continue;
    }
    if (fence !== null) {
      out += line;
      if (line.trimStart().startsWith(fence)) fence = null;
      continue;
    }
    const trimmed = line.trimStart();
    if (trimmed.startsWith("```") || trimmed.startsWith("~~~")) {
      fence = trimmed.slice(0, 3);
      out += line;
      continue;
    }
    out += scanTextSegment(line, lineStart, input, scan);
  }
  return out;
}

function normalizeTrailing(input: string): string {
  let out = "";
  for (const line of splitInclusive(input)) {
    const ending = line.endsWith("\r\n") ? "\r\n" : line.endsWith("\n") ? "\n" : "";
    out += line.slice(0, line.length - ending.length).replace(/[ \t]+$/, "") + ending;
  }
  out = out.replace(/[\r\n]+$/, "");
  return out.trim() ? `${out}\n` : "";
}

/**
 * jotra_core::prompt::strip_for_prompt: the text without its frontmatter and
 * its HTML comments outside code fences, trailing blanks trimmed, ending in
 * one line break.
 */
export function stripForPrompt(input: string): string {
  return normalizeTrailing(stripCommentsOutsideFences(input.slice(frontmatterEnd(input))));
}

/** jotra_core::prompt::estimate_tokens. */
export function estimatePromptTokens(text: string): number {
  if (!text.trim()) return 0;
  return estimateTokens(countText(Text.of(text.split("\n"))));
}

const escapeAttribute = (value: string) => value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

function formatFileElement(path: string, prepared: string): string {
  const close = prepared && !prepared.endsWith("\n") ? "\n" : "";
  return `<file path="${escapeAttribute(path)}">\n${prepared}${close}</file>\n\n`;
}

/** jotra_core::prompt::pack::PackBuilder: a pack built one file at a time, in the order given. */
export class PackBuilder {
  private text = "";
  private textBytes = 0;
  private readonly files: ContextPack["files"] = [];
  private readonly skipped: ContextPack["skipped"] = [];
  private omitted = 0;
  private full = false;

  isFull(): boolean {
    return this.full;
  }

  add(path: string, text: string): void {
    if (this.full || this.files.length >= MAX_PACK_FILES) {
      this.omit();
      return;
    }
    const prepared = stripForPrompt(text);
    const element = formatFileElement(path, prepared);
    const elementBytes = countUtf8Bytes(element);
    if (this.textBytes + elementBytes > MAX_PACK_BYTES) {
      this.omit();
      return;
    }
    this.text += element;
    this.textBytes += elementBytes;
    this.files.push({ path, tokens: estimatePromptTokens(prepared) });
  }

  skip(path: string, reason: PackSkipReason): void {
    this.skipped.push({ path, reason });
  }

  omit(): void {
    this.full = true;
    this.omitted += 1;
  }

  finish(): ContextPack {
    const text = this.text.trimEnd();
    return {
      text,
      tokens: estimatePromptTokens(text),
      files: [...this.files],
      skipped: [...this.skipped],
      omitted: this.omitted,
    };
  }
}
