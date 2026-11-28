import type { LineEnding } from "../../src/types/buffer";
import { ENCODING_CHOICES, encodingName } from "../../src/lib/text-encoding";

const BOM_SUFFIX = " BOM";
export const UTF_8 = "UTF-8";

/** jotra_core::text_encoding: the byte order marks an encoding can start with. */
const MARKS: Record<string, readonly number[]> = {
  "UTF-8": [0xef, 0xbb, 0xbf],
  "UTF-16LE": [0xff, 0xfe],
  "UTF-16BE": [0xfe, 0xff],
};

// A page decodes every WHATWG encoding but encodes only UTF-8, so the other
// encoders here are the page's decoders read backwards, with the WHATWG
// encoders' own exceptions.
const MULTIBYTE = new Set(["shift_jis", "euc-jp", "iso-2022-jp", "gbk", "gb18030", "big5", "euc-kr"]);

/** jotra_core::text_encoding::MAX_NAMED_CHARACTERS. */
const MAX_NAMED_CHARACTERS = 8;

/** A label `TextEncoding::from_label` reads as no encoding. */
export class UnknownEncodingError extends Error {
  constructor(readonly label: string) {
    super(`${JSON.stringify(label)} is not an encoding`);
    this.name = "UnknownEncodingError";
  }
}

/** StorageError::NotInEncoding: bytes that are not text in the encoding they are read in. */
export class NotInEncodingError extends Error {
  constructor(readonly encoding: string) {
    super(`the file is not ${encoding} text`);
    this.name = "NotInEncodingError";
  }
}

/** jotra_core::text_encoding::EncodingLossy: characters the encoding has no bytes for. */
export class EncodingLossyError extends Error {
  constructor(
    readonly encoding: string,
    readonly characters: readonly string[],
  ) {
    super(`${encoding} has no bytes for ${characters.map(formatCodePoint).join(" ")}`);
    this.name = "EncodingLossyError";
  }
}

function formatCodePoint(character: string): string {
  return `U+${(character.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, "0")}`;
}

/** TextEncoding::from_label: the encoding's name, with " BOM" kept where the encoding has a mark. */
export function resolveEncoding(label: string): string | null {
  let canonical: string;
  try {
    canonical = new TextDecoder(encodingName(label).trim()).encoding;
  } catch {
    return null;
  }
  const name = ENCODING_CHOICES.find((choice) => choice.name.toLowerCase() === canonical)?.name ?? canonical;
  if (!label.endsWith(BOM_SUFFIX)) return name;
  return name in MARKS ? `${name}${BOM_SUFFIX}` : null;
}

/** TextEncoding::with_bom_of: `encoding`, marked when `bytes` start with its byte order mark. */
export function withBomOf(encoding: string, bytes: Uint8Array): string {
  const name = encodingName(encoding);
  const mark = MARKS[name];
  const starts = mark !== undefined && bytes.length >= mark.length && mark.every((byte, i) => bytes[i] === byte);
  return starts ? `${name}${BOM_SUFFIX}` : name;
}

/** TextEncoding::decode: the text `bytes` hold in `encoding`. */
export function decodeIn(bytes: Uint8Array, encoding: string): string {
  try {
    return new TextDecoder(encodingName(encoding), { fatal: true }).decode(bytes);
  } catch (error) {
    if (error instanceof TypeError) throw new NotInEncodingError(encoding);
    throw error;
  }
}

const singleByteTables = new Map<string, Map<string, number>>();

/** Each character a single-byte encoding holds, to the byte it holds it as. */
function getSingleByteTable(name: string): Map<string, number> {
  let table = singleByteTables.get(name);
  if (!table) {
    const decoder = new TextDecoder(name);
    table = new Map();
    for (let byte = 0xff; byte >= 0; byte--) {
      const character = decoder.decode(Uint8Array.of(byte));
      if (character !== "�") table.set(character, byte);
    }
    singleByteTables.set(name, table);
  }
  return table;
}

function noteUnmappable(unmappable: string[], character: string): void {
  if (unmappable.length < MAX_NAMED_CHARACTERS && !unmappable.includes(character)) unmappable.push(character);
}

function encodeSingleByte(text: string, encoding: string): Uint8Array {
  const table = getSingleByteTable(encodingName(encoding));
  const out: number[] = [];
  const unmappable: string[] = [];
  for (const character of text) {
    const byte = table.get(character);
    if (byte !== undefined) out.push(byte);
    else noteUnmappable(unmappable, character);
  }
  if (unmappable.length > 0) throw new EncodingLossyError(encoding, unmappable);
  return Uint8Array.from(out);
}

/** The one code point `bytes` decode to, or null for an error or for more than one. */
function decodeCodePoint(decoder: TextDecoder, bytes: Uint8Array): number | null {
  const text = decoder.decode(bytes);
  const codePoint = text.codePointAt(0);
  if (codePoint === undefined || codePoint === 0xfffd) return null;
  return text.length === String.fromCodePoint(codePoint).length ? codePoint : null;
}

/** The Big5 code points the WHATWG encoder writes at their last pointer rather than their first. */
const BIG5_LAST_POINTER = new Set([0x2550, 0x255e, 0x2561, 0x256a, 0x5341, 0x5345]);

/** Shift_JIS pointers its encoder never writes: the IBM duplicates at 8272 to 8835, and the user-defined area. */
function isShiftJisWritten(lead: number, trail: number): boolean {
  const pointer = (lead - (lead < 0xa0 ? 0x81 : 0xc1)) * 188 + trail - (trail < 0x7f ? 0x40 : 0x41);
  return pointer < 8272 || pointer > 10715;
}

/** The two-byte sequences each encoder writes from: the lead and trail bytes its index covers. */
const TWO_BYTE_RANGES: Record<string, (lead: number, trail: number) => boolean> = {
  shift_jis: (lead, trail) => (lead <= 0x9f || lead >= 0xe0) && lead <= 0xfc && isShiftJisWritten(lead, trail),
  "euc-jp": (lead, trail) => (lead === 0x8e ? trail >= 0xa1 && trail <= 0xdf : lead >= 0xa1 && trail >= 0xa1),
  gbk: (_lead, trail) => trail !== 0x7f,
  gb18030: (_lead, trail) => trail !== 0x7f,
  big5: (lead) => lead >= 0xa1,
  "euc-kr": (_lead, trail) => trail >= 0x41,
};

/** The private-use code points the gb18030 encoder still writes as the two bytes that now read as standard characters. */
const GB18030_PRIVATE_USE_COMPAT: readonly (readonly [number, number, number])[] = [
  [0xe78d, 0xa6, 0xd9],
  [0xe78e, 0xa6, 0xda],
  [0xe78f, 0xa6, 0xdb],
  [0xe790, 0xa6, 0xdc],
  [0xe791, 0xa6, 0xdd],
  [0xe792, 0xa6, 0xde],
  [0xe793, 0xa6, 0xdf],
  [0xe794, 0xa6, 0xec],
  [0xe795, 0xa6, 0xed],
  [0xe796, 0xa6, 0xf3],
  [0xe81e, 0xfe, 0x59],
  [0xe826, 0xfe, 0x61],
  [0xe82b, 0xfe, 0x66],
  [0xe82c, 0xfe, 0x67],
  [0xe832, 0xfe, 0x6d],
  [0xe843, 0xfe, 0x7e],
  [0xe854, 0xfe, 0x90],
  [0xe864, 0xfe, 0xa0],
];

/** The gb18030 four-byte pointer for U+FFFD, which a decode error also gives and the table reading skips. */
const GB18030_REPLACEMENT_POINTER = 39417;

const multibyteTables = new Map<string, Map<number, readonly number[]>>();

/** Each code point a multibyte encoding writes, to the bytes it writes it as. */
function getMultibyteTable(name: string): Map<number, readonly number[]> {
  const cached = multibyteTables.get(name);
  if (cached) return cached;
  const decoder = new TextDecoder(name);
  const table = new Map<number, readonly number[]>();
  for (let byte = 0; byte < 0x80; byte++) table.set(byte, [byte]);
  if (name === "shift_jis") {
    table.set(0x80, [0x80]);
    for (let byte = 0xa1; byte <= 0xdf; byte++) table.set(byte - 0xa1 + 0xff61, [byte]);
  }
  const isWritten = TWO_BYTE_RANGES[name];
  for (let lead = 0x81; lead <= 0xfe; lead++) {
    for (let trail = 0x40; trail <= 0xfe; trail++) {
      if (!isWritten(lead, trail)) continue;
      const codePoint = decodeCodePoint(decoder, Uint8Array.of(lead, trail));
      if (codePoint === null) continue;
      if (table.has(codePoint) && !(name === "big5" && BIG5_LAST_POINTER.has(codePoint))) continue;
      table.set(codePoint, [lead, trail]);
    }
  }
  if (name === "shift_jis" || name === "euc-jp") {
    table.set(0xa5, [0x5c]);
    table.set(0x203e, [0x7e]);
    const minus = table.get(0xff0d);
    if (minus) table.set(0x2212, minus);
  }
  if (name === "gbk" || name === "gb18030") {
    table.delete(0xe5e5);
    for (const [codePoint, lead, trail] of GB18030_PRIVATE_USE_COMPAT) {
      if (!table.has(codePoint)) table.set(codePoint, [lead, trail]);
    }
  }
  if (name === "gbk") table.set(0x20ac, [0x80]);
  if (name === "gb18030") {
    table.set(0xfffd, gb18030FourBytes(GB18030_REPLACEMENT_POINTER));
    for (let pointer = 0; pointer <= 39419; pointer++) {
      const bytes = gb18030FourBytes(pointer);
      const codePoint = decodeCodePoint(decoder, Uint8Array.from(bytes));
      if (codePoint !== null && !table.has(codePoint)) table.set(codePoint, bytes);
    }
  }
  multibyteTables.set(name, table);
  return table;
}

/** A gb18030 four-byte pointer as its bytes. */
function gb18030FourBytes(pointer: number): number[] {
  return [
    Math.floor(pointer / 12600) + 0x81,
    Math.floor((pointer % 12600) / 1260) + 0x30,
    Math.floor((pointer % 1260) / 10) + 0x81,
    (pointer % 10) + 0x30,
  ];
}

function encodeMultibyte(text: string, encoding: string, name: string): Uint8Array {
  if (name === "iso-2022-jp") return encodeIso2022Jp(text, encoding);
  const table = getMultibyteTable(name);
  const out: number[] = [];
  const unmappable: string[] = [];
  for (const character of text) {
    const codePoint = character.codePointAt(0) ?? 0;
    const bytes =
      table.get(codePoint) ??
      (name === "gb18030" && codePoint > 0xffff ? gb18030FourBytes(codePoint - 0x10000 + 189000) : undefined);
    if (bytes) out.push(...bytes);
    else noteUnmappable(unmappable, character);
  }
  if (unmappable.length > 0) throw new EncodingLossyError(encoding, unmappable);
  return Uint8Array.from(out);
}

const ESCAPE_ASCII = [0x1b, 0x28, 0x42];
const ESCAPE_ROMAN = [0x1b, 0x28, 0x4a];
const ESCAPE_JIS0208 = [0x1b, 0x24, 0x42];

/** The full-width katakana the ISO-2022-JP encoder writes for a half-width one. */
function toFullWidthKatakana(codePoint: number): number {
  if (codePoint === 0xff9e) return 0x309b;
  if (codePoint === 0xff9f) return 0x309c;
  return String.fromCodePoint(codePoint).normalize("NFKC").codePointAt(0) ?? codePoint;
}

let jis0208Table: Map<number, readonly number[]> | undefined;

/** JIS X 0208 as ISO-2022-JP writes it: the EUC-JP two-byte table, each byte less 0x80. */
function getJis0208Table(): Map<number, readonly number[]> {
  if (jis0208Table) return jis0208Table;
  jis0208Table = new Map();
  for (const [codePoint, bytes] of getMultibyteTable("euc-jp")) {
    if (bytes.length === 2 && bytes[0] !== 0x8e) jis0208Table.set(codePoint, [bytes[0] - 0x80, bytes[1] - 0x80]);
  }
  return jis0208Table;
}

/** The WHATWG ISO-2022-JP encoder: ASCII, Roman and JIS X 0208 runs, each opened by its escape. */
function encodeIso2022Jp(text: string, encoding: string): Uint8Array {
  const jis0208 = getJis0208Table();
  const out: number[] = [];
  const unmappable: string[] = [];
  let mode = "ascii" as "ascii" | "roman" | "jis0208";
  const enter = (next: typeof mode, escape: readonly number[]) => {
    if (mode !== next) out.push(...escape);
    mode = next;
  };
  for (const character of text) {
    let codePoint = character.codePointAt(0) ?? 0;
    if (codePoint === 0x0e || codePoint === 0x0f || codePoint === 0x1b) {
      noteUnmappable(unmappable, "�");
      continue;
    }
    if (codePoint < 0x80) {
      if (mode !== "roman" || codePoint === 0x5c || codePoint === 0x7e) enter("ascii", ESCAPE_ASCII);
      out.push(codePoint);
      continue;
    }
    if (codePoint === 0xa5 || codePoint === 0x203e) {
      enter("roman", ESCAPE_ROMAN);
      out.push(codePoint === 0xa5 ? 0x5c : 0x7e);
      continue;
    }
    if (codePoint === 0x2212) codePoint = 0xff0d;
    if (codePoint >= 0xff61 && codePoint <= 0xff9f) codePoint = toFullWidthKatakana(codePoint);
    const bytes = jis0208.get(codePoint);
    if (!bytes) {
      noteUnmappable(unmappable, character);
      continue;
    }
    enter("jis0208", ESCAPE_JIS0208);
    out.push(...bytes);
  }
  enter("ascii", ESCAPE_ASCII);
  if (unmappable.length > 0) throw new EncodingLossyError(encoding, unmappable);
  return Uint8Array.from(out);
}

function encodeUtf16(text: string, littleEndian: boolean): Uint8Array {
  const out = new Uint8Array(text.length * 2);
  const view = new DataView(out.buffer);
  for (let i = 0; i < text.length; i++) view.setUint16(i * 2, text.charCodeAt(i), littleEndian);
  return out;
}

/** TextEncoding::encode: `text` as the bytes a file in `encoding` holds, its byte order mark first. */
export function encodeIn(text: string, encoding: string): Uint8Array {
  const name = encodingName(encoding);
  let body: Uint8Array;
  if (name === "UTF-16LE" || name === "UTF-16BE") {
    body = encodeUtf16(text, name === "UTF-16LE");
  } else if (name === UTF_8 || (/^[\x00-\x7f]*$/.test(text) && name.toLowerCase() !== "iso-2022-jp")) {
    body = new TextEncoder().encode(text);
  } else if (MULTIBYTE.has(name.toLowerCase())) {
    body = encodeMultibyte(text, encoding, name.toLowerCase());
  } else {
    body = encodeSingleByte(text, encoding);
  }
  const mark = encoding.endsWith(BOM_SUFFIX) ? (MARKS[name] ?? []) : [];
  if (mark.length === 0) return body;
  const out = new Uint8Array(mark.length + body.length);
  out.set(mark);
  out.set(body, mark.length);
  return out;
}

/** LineEnding::detect: CRLF when it ends more lines than LF alone does. */
export function detectLineEnding(text: string): LineEnding {
  const crlf = text.match(/\r\n/g)?.length ?? 0;
  const lf = (text.match(/\n/g)?.length ?? 0) - crlf;
  return crlf > lf ? "crlf" : "lf";
}
