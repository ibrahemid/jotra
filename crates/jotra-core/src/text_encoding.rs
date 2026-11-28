//! Which text encoding a file's bytes are in, and how to read and write them
//! in it (ADR-049).
//!
//! The encoding is decided once, when a file is first read: a byte order mark,
//! then the shape of UTF-16 without one, then the NUL sniff that calls a file
//! binary, then UTF-8, then chardetng's guess among the legacy encodings. It is
//! kept beside the line ending, the editor works in UTF-8 whatever the file
//! holds, and a save writes the file back in its own encoding or not at all.

use std::borrow::Cow;
use std::fmt;

use encoding_rs::{EncoderResult, Encoding, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::file_ops::BINARY_CHECK_BYTES;

/// How much of a file chardetng is given to guess from. A guess settles well
/// before this; the cap keeps a 500 MiB file from being scored byte by byte.
pub const DETECTION_SAMPLE_BYTES: usize = 1024 * 1024;

/// How many of the characters a lossy save would drop are named in its
/// refusal.
pub const MAX_NAMED_CHARACTERS: usize = 8;

const UTF_8_BOM: &[u8] = b"\xEF\xBB\xBF";
const UTF_16LE_BOM: &[u8] = b"\xFF\xFE";
const UTF_16BE_BOM: &[u8] = b"\xFE\xFF";
const BOM_SUFFIX: &str = " BOM";

/// A text encoding as a file holds it: the encoding, and whether the file
/// starts with its byte order mark.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TextEncoding {
    encoding: &'static Encoding,
    bom: bool,
}

impl fmt::Debug for TextEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

impl fmt::Display for TextEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

impl TextEncoding {
    /// UTF-8 without a byte order mark: what every file Jotra creates is in.
    #[must_use]
    pub fn utf8() -> Self {
        Self {
            encoding: UTF_8,
            bom: false,
        }
    }

    /// Windows-1252. Every byte decodes to a character and back to itself, so
    /// it is the encoding a file is read in when the guess cannot decode it.
    /// ISO-8859-1 is read as this encoding, as the WHATWG table does.
    #[must_use]
    pub fn windows_1252() -> Self {
        Self {
            encoding: WINDOWS_1252,
            bom: false,
        }
    }

    /// The encoding a label names: the encoding's name or any of its WHATWG
    /// labels, optionally followed by ` BOM` for UTF-8 and UTF-16.
    ///
    /// `None` for a label nothing answers to, for the replacement encoding,
    /// and for ` BOM` on an encoding that has none.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        let (name, bom) = match label.strip_suffix(BOM_SUFFIX) {
            Some(name) => (name, true),
            None => (label, false),
        };
        let encoding = Encoding::for_label_no_replacement(name.trim().as_bytes())?;
        let candidate = Self { encoding, bom };
        (!bom || !candidate.bom_bytes().is_empty()).then_some(candidate)
    }

    /// The encoding's WHATWG name (`UTF-8`, `UTF-16LE`, `windows-1252`,
    /// `Shift_JIS`, `GBK`).
    #[must_use]
    pub fn name(self) -> &'static str {
        self.encoding.name()
    }

    /// Whether the file starts with the encoding's byte order mark.
    #[must_use]
    pub fn has_bom(self) -> bool {
        self.bom
    }

    /// The value as the row and the wire carry it: the name, followed by
    /// ` BOM` when the file has one. [`Self::from_label`] reads it back.
    #[must_use]
    pub fn label(self) -> String {
        if self.bom {
            format!("{}{BOM_SUFFIX}", self.encoding.name())
        } else {
            self.encoding.name().to_string()
        }
    }

    /// This encoding with the byte order mark `bytes` start with, or without
    /// one when they start with none.
    ///
    /// A file another program rewrote with or without the mark keeps that
    /// choice from the next read on.
    #[must_use]
    pub fn with_bom_of(self, bytes: &[u8]) -> Self {
        let mark = self.mark();
        Self {
            encoding: self.encoding,
            bom: !mark.is_empty() && bytes.starts_with(mark),
        }
    }

    /// The bytes of this encoding's byte order mark, empty for an encoding
    /// that has none.
    fn mark(self) -> &'static [u8] {
        if self.encoding == UTF_8 {
            UTF_8_BOM
        } else if self.encoding == UTF_16LE {
            UTF_16LE_BOM
        } else if self.encoding == UTF_16BE {
            UTF_16BE_BOM
        } else {
            &[]
        }
    }

    /// The mark a save writes ahead of the text: the encoding's own when the
    /// file has one, else none.
    fn bom_bytes(self) -> &'static [u8] {
        if self.bom {
            self.mark()
        } else {
            &[]
        }
    }

    /// `bytes` without this encoding's byte order mark, when they start with
    /// one.
    fn body(self, bytes: &[u8]) -> &[u8] {
        let mark = self.mark();
        if !mark.is_empty() && bytes.starts_with(mark) {
            &bytes[mark.len()..]
        } else {
            bytes
        }
    }

    /// Reads `bytes` as text in this encoding, without the byte order mark.
    ///
    /// # Errors
    ///
    /// [`NotInEncoding`] when any byte sequence has no character in this
    /// encoding. Nothing is replaced: text read with a replacement character
    /// standing in for bytes would write that character back over them.
    pub fn decode(self, bytes: &[u8]) -> Result<Cow<'_, str>, NotInEncoding> {
        self.encoding
            .decode_without_bom_handling_and_without_replacement(self.body(bytes))
            .ok_or(NotInEncoding { encoding: self })
    }

    /// [`Self::decode`] for bytes the caller is done with.
    ///
    /// UTF-8 bytes with no mark are the text: they are validated once and
    /// handed over as it, with no copy. Any other bytes are decoded into new
    /// text and handed back beside it.
    ///
    /// # Errors
    ///
    /// [`NotInEncoding`], as [`Self::decode`] answers it.
    pub fn decode_owned(self, bytes: Vec<u8>) -> Result<OwnedText, NotInEncoding> {
        if self.encoding == UTF_8 && !bytes.starts_with(UTF_8_BOM) {
            return String::from_utf8(bytes)
                .map(OwnedText::Bytes)
                .map_err(|_| NotInEncoding { encoding: self });
        }
        let text = self.decode(&bytes)?.into_owned();
        Ok(OwnedText::Decoded { text, bytes })
    }

    /// Whether the text `prefix` declares in this encoding, after its byte
    /// order mark, holds no NUL character and, for UTF-16, no surrogate
    /// without its pair short of one cut off at the end.
    fn declared_body_is_text(self, prefix: &[u8]) -> bool {
        let body = self.body(prefix);
        if self.encoding != UTF_16LE && self.encoding != UTF_16BE {
            return !body.contains(&0);
        }
        let little = self.encoding == UTF_16LE;
        utf16_reads_as_text(body, little, |character| character != '\0')
    }

    /// Reads `bytes` as text in this encoding, with U+FFFD for any sequence
    /// that has no character. Only for a digest or a display, never for text
    /// that may be saved.
    #[must_use]
    pub fn decode_lossy(self, bytes: &[u8]) -> Cow<'_, str> {
        self.encoding
            .decode_without_bom_handling(self.body(bytes))
            .0
    }

    /// The bytes `text` is in this encoding, the byte order mark first when
    /// the file has one.
    ///
    /// # Errors
    ///
    /// [`EncodingLossy`], naming up to [`MAX_NAMED_CHARACTERS`] characters in
    /// the order they first appear, when the encoding has no bytes for some of
    /// `text`. Nothing is substituted for them.
    pub fn encode(self, text: &str) -> Result<Cow<'_, [u8]>, EncodingLossy> {
        let mark = self.bom_bytes();
        if self.encoding == UTF_16LE || self.encoding == UTF_16BE {
            return Ok(Cow::Owned(self.encode_utf16(text, mark)));
        }
        if self.encoding == UTF_8 || (text.is_ascii() && self.encoding.is_ascii_compatible()) {
            if mark.is_empty() {
                return Ok(Cow::Borrowed(text.as_bytes()));
            }
            let mut out = Vec::with_capacity(mark.len() + text.len());
            out.extend_from_slice(mark);
            out.extend_from_slice(text.as_bytes());
            return Ok(Cow::Owned(out));
        }
        self.encode_legacy(text).map(Cow::Owned)
    }

    /// UTF-16 by hand: encoding_rs decodes it but encodes into UTF-8 instead,
    /// as the web does.
    fn encode_utf16(self, text: &str, mark: &[u8]) -> Vec<u8> {
        let little = self.encoding == UTF_16LE;
        let mut out = Vec::with_capacity(mark.len() + text.len() * 2);
        out.extend_from_slice(mark);
        for unit in text.encode_utf16() {
            let bytes = if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            out.extend_from_slice(&bytes);
        }
        out
    }

    fn encode_legacy(self, text: &str) -> Result<Vec<u8>, EncodingLossy> {
        let mut encoder = self.encoding.new_encoder();
        let mut out = Vec::with_capacity(text.len());
        let mut unmappable: Vec<char> = Vec::new();
        let mut rest = text;
        loop {
            let needed = encoder
                .max_buffer_length_from_utf8_without_replacement(rest.len())
                .unwrap_or(rest.len().saturating_mul(4));
            out.reserve(needed.max(16));
            let (result, read) =
                encoder.encode_from_utf8_to_vec_without_replacement(rest, &mut out, true);
            rest = &rest[read..];
            match result {
                EncoderResult::InputEmpty => break,
                EncoderResult::OutputFull => {}
                EncoderResult::Unmappable(character) => {
                    if unmappable.len() < MAX_NAMED_CHARACTERS && !unmappable.contains(&character) {
                        unmappable.push(character);
                    }
                }
            }
        }
        if unmappable.is_empty() {
            Ok(out)
        } else {
            Err(EncodingLossy {
                encoding: self,
                characters: unmappable,
            })
        }
    }

    /// The digest that answers "does the editor's document still say what
    /// this file says" ([`crate::hash::comparison_digest_hex`]), for a file in
    /// this encoding.
    ///
    /// The editor holds the decoded text, so the file is decoded before it is
    /// hashed. A UTF-8 file with no mark is hashed as it is, which is the
    /// digest every UTF-8 note has always had.
    #[must_use]
    pub fn comparison_digest_hex(self, bytes: &[u8]) -> String {
        if self.encoding == UTF_8 && !bytes.starts_with(UTF_8_BOM) {
            return crate::hash::comparison_digest_hex(bytes);
        }
        crate::hash::comparison_digest_hex(self.decode_lossy(bytes).as_bytes())
    }

    /// [`Self::comparison_digest_hex`] for bytes whose SHA-256 is `sha256`.
    ///
    /// A UTF-8 file with no mark and no `\r` is hashed as it is, so that digest
    /// is the answer and the bytes are not hashed a second time.
    #[must_use]
    pub fn comparison_digest_hex_of_hashed(
        self,
        bytes: &[u8],
        sha256: crate::hash::Sha256Digest,
    ) -> String {
        if self.encoding == UTF_8 && !bytes.starts_with(UTF_8_BOM) && !bytes.contains(&b'\r') {
            return crate::hash::digest_hex(sha256);
        }
        self.comparison_digest_hex(bytes)
    }
}

impl Serialize for TextEncoding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.label())
    }
}

impl<'de> Deserialize<'de> for TextEncoding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let label = String::deserialize(deserializer)?;
        Self::from_label(&label)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown text encoding {label:?}")))
    }
}

/// Bytes that are not text in the encoding they were read in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the file is not {encoding} text")]
pub struct NotInEncoding {
    /// The encoding the bytes were read in.
    pub encoding: TextEncoding,
}

/// Text holding characters its file's encoding has no bytes for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{encoding} has no bytes for {}", code_points(characters))]
pub struct EncodingLossy {
    /// The encoding the file is in.
    pub encoding: TextEncoding,
    /// The characters it cannot hold, in the order they first appear, at most
    /// [`MAX_NAMED_CHARACTERS`].
    pub characters: Vec<char>,
}

/// `characters` as code points (`U+1F600 U+4E2D`), the form a refusal names
/// them in.
#[must_use]
pub fn code_points(characters: &[char]) -> String {
    characters
        .iter()
        .map(|character| format!("U+{:04X}", u32::from(*character)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text read from a file, and the encoding it was read in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedText<'a> {
    /// The encoding the bytes were found in.
    pub encoding: TextEncoding,
    /// The text, without the byte order mark.
    pub text: Cow<'a, str>,
    /// `true` when U+FFFD stands in the text for bytes that are not text in
    /// `encoding`: a UTF-8 file with a damaged sequence. Such text is for
    /// reading and indexing only; written back, it would replace those bytes.
    pub replaced: bool,
}

/// Text decoded from bytes the caller handed over ([`TextEncoding::decode_owned`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedText {
    /// The bytes were UTF-8 with no mark, and are the text.
    Bytes(String),
    /// The text decoded from `bytes`, which differ from it.
    Decoded {
        /// The text, without the byte order mark.
        text: String,
        /// The bytes it was decoded from.
        bytes: Vec<u8>,
    },
}

/// Whether a file whose first bytes are `prefix` is binary: a NUL byte when
/// they declare no encoding, a NUL character or a broken surrogate in the text
/// after the mark when they declare one ([`declared`]).
///
/// A byte order mark says how to read what follows it, not that what follows
/// is text. UTF-16 without a mark is only declared when its prefix already
/// reads as text.
#[must_use]
pub fn sniffs_binary(prefix: &[u8]) -> bool {
    match declared(prefix) {
        None => prefix.contains(&0),
        Some(encoding) if encoding.bom => !encoding.declared_body_is_text(prefix),
        Some(_) => false,
    }
}

/// The encoding the first bytes of a file declare: a byte order mark, or the
/// shape of UTF-16 without one. `None` when they declare nothing, which is
/// when the NUL sniff and the rest of [`detect`] decide.
#[must_use]
pub fn declared(prefix: &[u8]) -> Option<TextEncoding> {
    if let Some((encoding, _)) = Encoding::for_bom(prefix) {
        return Some(TextEncoding {
            encoding,
            bom: true,
        });
    }
    utf16_without_bom(prefix).map(|encoding| TextEncoding {
        encoding,
        bom: false,
    })
}

/// Whether `prefix` reads as UTF-16 with no byte order mark.
///
/// Text that is mostly Latin script has a zero in the high byte of nearly
/// every unit and almost never in the low one; the side the zeros fall on is
/// the byte order. The prefix must then decode to text: no NUL, no unpaired
/// surrogate short of one cut off at the end, no control character but tab,
/// line feed, form feed, carriage return and escape. Binary data with
/// scattered zeros fails one of the three. UTF-16 that is mostly CJK has too
/// few zeros to tell and is left to the rest of the detection; Reopen with
/// encoding is the way to it.
fn utf16_without_bom(prefix: &[u8]) -> Option<&'static Encoding> {
    let sample = &prefix[..prefix.len().min(BINARY_CHECK_BYTES)];
    let units = sample.len() / 2;
    if units == 0 {
        return None;
    }
    let (pairs, _) = sample.as_chunks::<2>();
    let (mut even_zero, mut odd_zero) = (0usize, 0usize);
    for [even, odd] in pairs {
        even_zero += usize::from(*even == 0);
        odd_zero += usize::from(*odd == 0);
    }
    let mostly = |zeros: usize| zeros * 10 >= units * 4;
    let rarely = |zeros: usize| zeros * 20 <= units;
    let little = if mostly(odd_zero) && rarely(even_zero) {
        true
    } else if mostly(even_zero) && rarely(odd_zero) {
        false
    } else {
        return None;
    };
    utf16_reads_as_text(sample, little, is_text_character).then_some(if little {
        UTF_16LE
    } else {
        UTF_16BE
    })
}

/// Whether `sample` decodes as UTF-16 in the byte order `little` names into
/// characters `is_text` accepts, with no surrogate missing its pair short of a
/// high one cut off at the end.
fn utf16_reads_as_text(sample: &[u8], little: bool, is_text: impl Fn(char) -> bool) -> bool {
    let (pairs, _) = sample.as_chunks::<2>();
    let code_units = pairs.iter().map(|pair| {
        if little {
            u16::from_le_bytes(*pair)
        } else {
            u16::from_be_bytes(*pair)
        }
    });
    let mut decoded = char::decode_utf16(code_units).peekable();
    while let Some(unit) = decoded.next() {
        let is_last = decoded.peek().is_none();
        match unit {
            Ok(character) if is_text(character) => {}
            Err(error) if is_last && (0xD800..0xDC00).contains(&error.unpaired_surrogate()) => {}
            _ => return false,
        }
    }
    true
}

fn is_text_character(character: char) -> bool {
    !character.is_control() || matches!(character, '\t' | '\n' | '\u{0C}' | '\r' | '\u{1B}')
}

/// The encoding a file that is not binary is in: what its first bytes
/// declare, else UTF-8 when the whole of it is UTF-8 or it holds more UTF-8
/// sequences of two or more bytes than damaged ones ([`reads_as_damaged_utf8`]),
/// else chardetng's guess.
#[must_use]
pub fn detect(bytes: &[u8]) -> TextEncoding {
    if let Some(declared) = declared(bytes) {
        return declared;
    }
    let valid_up_to = Encoding::utf8_valid_up_to(bytes);
    if valid_up_to == bytes.len() {
        return TextEncoding::utf8();
    }
    let sample = &bytes[..bytes.len().min(DETECTION_SAMPLE_BYTES)];
    if reads_as_damaged_utf8(sample, valid_up_to.min(sample.len())) {
        return TextEncoding::utf8();
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(sample, sample.len() == bytes.len());
    TextEncoding {
        encoding: detector.guess(None, chardetng::Utf8Detection::Deny),
        bom: false,
    }
}

/// Whether `sample`, which is UTF-8 up to `valid_up_to` and not at it, is a
/// UTF-8 file with damage rather than a file in another encoding: more of its
/// sequences of two or more bytes are UTF-8 than are broken.
///
/// The accented letters of a legacy encoding are single high bytes, each a
/// broken UTF-8 sequence, and they almost never fall into a whole one. A UTF-8
/// file with one bad byte keeps the rest of its non-ASCII text whole.
fn reads_as_damaged_utf8(sample: &[u8], valid_up_to: usize) -> bool {
    let leads = |valid: &[u8]| valid.iter().filter(|byte| **byte >= 0xC0).count();
    let mut sequences = leads(&sample[..valid_up_to]);
    let mut broken = 0usize;
    let mut rest = &sample[valid_up_to..];
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                sequences += leads(valid.as_bytes());
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                sequences += leads(valid);
                let Some(length) = error.error_len() else {
                    break;
                };
                broken += 1;
                rest = &after[length..];
            }
        }
    }
    sequences > broken
}

/// Reads a file's bytes as text in the encoding [`detect`] finds.
///
/// A legacy guess that cannot decode every byte gives way to Windows-1252,
/// which can, so the text is never read with a character standing in for
/// bytes and a save never writes one over them. A UTF-8 file with damaged
/// sequences stays UTF-8, read with U+FFFD for them and marked
/// [`DecodedText::replaced`]: the open records it as UTF-8, so the tab's
/// strict read refuses it rather than showing it in a guessed encoding. The
/// open reads through here, and so does every reader of a file Jotra has no
/// row for ([`decode_file`]), so they all see the same text.
#[must_use]
pub fn decode_detected(bytes: &[u8]) -> DecodedText<'_> {
    let encoding = detect(bytes);
    match encoding.decode(bytes) {
        Ok(text) => DecodedText {
            encoding,
            text,
            replaced: false,
        },
        Err(_) if encoding.encoding == UTF_8 => DecodedText {
            encoding,
            text: encoding.decode_lossy(bytes),
            replaced: true,
        },
        Err(_) => {
            let fallback = TextEncoding::windows_1252();
            DecodedText {
                encoding: fallback,
                text: fallback.decode_lossy(bytes),
                replaced: false,
            }
        }
    }
}

/// [`decode_detected`] for bytes the caller is done with: text that is
/// already UTF-8 takes over their allocation rather than being copied, which
/// for a file in the large tiers is the difference between holding it once
/// and holding it twice.
#[must_use]
pub fn decode_detected_owned(mut bytes: Vec<u8>) -> (TextEncoding, String) {
    let (encoding, decoded) = {
        let decoded = decode_detected(&bytes);
        let text = match decoded.text {
            Cow::Owned(text) => Some(text),
            Cow::Borrowed(_) => None,
        };
        (decoded.encoding, text)
    };
    if let Some(text) = decoded {
        return (encoding, text);
    }
    // Borrowed text is the bytes after the encoding's mark, and is UTF-8.
    let mark = bytes.len() - encoding.body(&bytes).len();
    bytes.drain(..mark);
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
    (encoding, text)
}

/// Reads a file's bytes as text ([`decode_detected`]), or `None` for bytes
/// the open shows as binary ([`crate::file_ops::is_binary_bytes`]).
///
/// The one reader for text Jotra has no row for: the notes index the `jotra`
/// verbs answer from, the MCP server's `read_note`, and text piped to `jotra`.
#[must_use]
pub fn decode_file(bytes: &[u8]) -> Option<DecodedText<'_>> {
    (!crate::file_ops::is_binary_bytes(bytes)).then(|| decode_detected(bytes))
}
