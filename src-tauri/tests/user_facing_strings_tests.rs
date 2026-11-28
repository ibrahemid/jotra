//! Guards the vocabulary ADR-028 §10 retires from user-visible strings.
//!
//! The scope is the Rust half: strings that reach a person as a message,
//! including the `#[error("…")]` text every `thiserror` enum carries, which is
//! what most IPC failures render. Type names, field names, comments, log
//! lines, SQL and JSON keys stay legal, so the scan reads string literals
//! through a small lexer rather than reading whole files.
//!
//! `src-tauri` carries no `regex` and no `walkdir`, and this guard is not a
//! reason to add one: the walk is `std::fs::read_dir` recursion over declared
//! roots and the match is a `str::find` with its own word boundaries.
//!
//! Known limit. A message assembled from a `const` or a variable escapes the
//! scan, because only the literal is read and the literal is where the word
//! would be.

use std::path::{Path, PathBuf};

/// ADR-028 §10, verbatim. The operator's four (vault, buffer, scratchpad,
/// second brain) are the first four.
const BANNED: &[&str] = &[
    "vault",
    "buffer",
    "scratchpad",
    "second brain",
    "render surface",
    "inbox",
    "reveal",
    "threshold",
    "refuse",
    "debounce",
    "source",
    "dialect",
    "FTS",
    "IPC",
    "sidecar",
    "MiB",
    "syntax highlighting",
    "typography",
    // ADR-041 §1 and decision C1: Jotra is a text app, so a message names a
    // file. `word_forms` inflects the stem, so "notes" is this entry too.
    "note",
];

/// Where the messages are written. The first element is the path from
/// `CARGO_MANIFEST_DIR` (the `src-tauri` crate root even from an integration
/// test, so a sibling crate is reached with `..`); the second is how the file
/// is named in an allowlist record, relative to the workspace root.
const SCANNED_ROOTS: &[(&str, &str)] = &[
    ("src", "src-tauri/src"),
    ("../crates/jotra-core/src", "crates/jotra-core/src"),
    ("../crates/jotra-storage/src", "crates/jotra-storage/src"),
    ("../crates/jotra-render/src", "crates/jotra-render/src"),
    ("../crates/jotra-cli/src", "crates/jotra-cli/src"),
    ("../crates/jotra-mcp/src", "crates/jotra-mcp/src"),
    ("../crates/jotra-ai/src", "crates/jotra-ai/src"),
    ("../crates/jotra-plugin/src", "crates/jotra-plugin/src"),
    ("../crates/jotra-lint/src", "crates/jotra-lint/src"),
];

/// Directories inside a scanned root that hold no user message.
const SKIPPED_DIRS: &[&str] = &["tests", "benches", "target", "fixtures"];

/// One string that already says a retired word, named by the item it is
/// written in and a piece of its text rather than by a line, so an edit
/// elsewhere in the file leaves the record standing. The list can only
/// shrink: a record whose string is gone fails the staleness test, so a
/// rename deletes its record in the same change. A record answers for exactly
/// one string: a second string in the same item that carries the same text
/// and word is a new violation, not a listed one.
struct AllowedString {
    file: &'static str,
    /// The innermost item around the string, as `Type::function` or `NAME`.
    item: &'static str,
    /// A piece of the string's text, as written in the source.
    text: &'static str,
    word: &'static str,
    note: &'static str,
}

const RUST_ALLOWLIST: &[AllowedString] = &[
    AllowedString {
        file: "crates/jotra-core/src/default_app.rs",
        item: "claimable_types::TYPES",
        text: "Source code",
        word: "source",
        note: "file-association group label, release 0.6",
    },
    AllowedString {
        file: "crates/jotra-core/src/errors.rs",
        item: "JotraError",
        text: "buffer not found",
        word: "buffer",
        note: "BufferNotFound message, release 0.6",
    },
    AllowedString {
        file: "crates/jotra-core/src/errors.rs",
        item: "JotraError",
        text: "buffer already exists",
        word: "buffer",
        note: "BufferAlreadyExists message, release 0.6",
    },
    AllowedString {
        file: "crates/jotra-core/src/errors.rs",
        item: "JotraError",
        text: "invalid buffer title",
        word: "buffer",
        note: "InvalidTitle message, release 0.6",
    },
    AllowedString {
        file: "crates/jotra-core/src/file_ops.rs",
        item: "format_bytes",
        text: " MiB",
        word: "MiB",
        note: "size formatter unit, release 0.6",
    },
    AllowedString {
        file: "crates/jotra-storage/src/database/queries.rs",
        item: "get_buffer",
        text: "buffer not found",
        word: "buffer",
        note: "row lookup error, release 0.6",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "SYSTEM_PROMPT_HEAD",
        text: "a notes editor",
        word: "note",
        note: "model-facing system prompt, not app copy",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "BODY_PLACEHOLDER",
        text: "the note's full text",
        word: "note",
        note: "model-facing body placeholder, not app copy",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "LEGACY_BODY_PLACEHOLDER",
        text: "new text of the note",
        word: "note",
        note: "the body placeholder older conversations were written with",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "EXAMPLE_PATH",
        text: "Notes/Example.md",
        word: "note",
        note: "example path inside the model-facing prompt",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "system_prompt",
        text: "To offer a change to an attached note",
        word: "note",
        note: "model-facing offer instructions, not app copy",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "system_prompt",
        text: "No note is attached",
        word: "note",
        note: "model-facing offer instructions, not app copy",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "system_prompt",
        text: "The block holds the note's entire text",
        word: "note",
        note: "model-facing offer instructions, not app copy",
    },
    AllowedString {
        file: "crates/jotra-core/src/chat/prompt.rs",
        item: "context_block",
        text: "<note{suffix} path=",
        word: "note",
        note: "attachment tag the model reads; renaming it breaks parsing",
    },
    AllowedString {
        file: "crates/jotra-core/src/polish.rs",
        item: "RETURN_ONLY",
        text: "explanation, notes, or code",
        word: "note",
        note: "model-facing rewrite prompt: \"notes\" means asides in the answer",
    },
    AllowedString {
        file: "crates/jotra-mcp/src/server.rs",
        item: "INSTRUCTIONS",
        text: "Reads and writes the notes",
        word: "note",
        note: "MCP server instructions, out of the copy sweep (decision C1)",
    },
    AllowedString {
        file: "crates/jotra-ai/src/chat.rs",
        item: "mints_nothing",
        text: "may not create notes",
        word: "note",
        note: "debug_assert text, never rendered",
    },
    AllowedString {
        file: "src-tauri/src/state.rs",
        item: "AppState::initialize",
        text: "notes migration finished",
        word: "note",
        note: "log line the prefix test misses inside a multi-line macro",
    },
    AllowedString {
        file: "src-tauri/src/state.rs",
        item: "AppState::run_startup_maintenance",
        text: "retired versions of notes",
        word: "note",
        note: "log line the prefix test misses inside a multi-line macro",
    },
];

/// Lines whose first token opens a log macro or an attribute, plus the lines a
/// multi-line one runs onto. A log line is not a user message. `#[error(…)]`
/// is the exception: it *is* the user message.
fn skipped_lines(src: &str) -> Vec<bool> {
    const SKIP_PREFIXES: &[&str] = &[
        "tracing::",
        "trace!",
        "debug!",
        "info!",
        "warn!",
        "error!",
        "#[",
        "#![",
    ];
    let lines: Vec<&str> = src.lines().collect();
    let mut skipped = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let head = lines[i].trim_start();
        let is_skip = SKIP_PREFIXES.iter().any(|p| head.starts_with(p))
            && !head.starts_with("#[error(")
            && !head.starts_with("#[doc");
        if is_skip {
            let mut depth = 0i32;
            let mut j = i;
            loop {
                skipped[j] = true;
                depth += delimiter_balance(lines[j]);
                if depth <= 0 || j + 1 >= lines.len() {
                    break;
                }
                j += 1;
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    skipped
}

fn delimiter_balance(line: &str) -> i32 {
    let mut depth = 0i32;
    for ch in line.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (idx, byte) in src.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn line_of(starts: &[usize], index: usize) -> usize {
    match starts.binary_search(&index) {
        Ok(pos) => pos + 1,
        Err(pos) => pos,
    }
}

fn is_ident_byte(byte: Option<u8>) -> bool {
    matches!(byte, Some(b) if b.is_ascii_alphanumeric() || b == b'_')
}

/// A string literal, with the line it opens on, whatever its line says before
/// it, and the item it is written in. The prefix is what tells a message from
/// a log argument.
struct Literal {
    at: usize,
    line: usize,
    value: String,
    prefix: String,
    item: String,
}

impl Literal {
    fn new(starts: &[usize], src: &str, open: usize, body_start: usize, body_end: usize) -> Self {
        let line = line_of(starts, open);
        Self {
            at: open,
            line,
            value: src[body_start..body_end].to_string(),
            prefix: src[starts[line - 1]..open].to_string(),
            item: String::new(),
        }
    }

    /// `true` when the literal is an argument to a log macro, wherever on the
    /// line the call sits: `Ok(n) => info!(n, "…")` is as much a log line as
    /// one that opens the line.
    fn is_log_argument(&self) -> bool {
        const MACROS: &[&str] = &[
            "tracing::",
            "log::",
            "trace!",
            "debug!",
            "info!",
            "warn!",
            "error!",
        ];
        MACROS.iter().any(|m| self.prefix.contains(m))
    }
}

/// One token of a Rust source: enough of the grammar to read the string
/// literals and the items they are written in.
enum Token {
    Ident { at: usize, name: String },
    Str(Literal),
    Punct { at: usize, ch: u8 },
}

impl Token {
    fn at(&self) -> usize {
        match self {
            Token::Ident { at, .. } | Token::Punct { at, .. } => *at,
            Token::Str(literal) => literal.at,
        }
    }

    fn is_punct(&self, wanted: u8) -> bool {
        matches!(self, Token::Punct { ch, .. } if *ch == wanted)
    }

    fn ident(&self) -> Option<&str> {
        match self {
            Token::Ident { name, .. } => Some(name),
            _ => None,
        }
    }
}

/// The tokens of a Rust source. Comments are stepped over, a char literal
/// is dropped and a lifetime is a `'` before its name; a string's escapes are
/// left as written, which is enough for a word match.
fn tokens(src: &str) -> Vec<Token> {
    let starts = line_starts(src);
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0usize;

    while i < n {
        let byte = bytes[i];

        if byte == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if byte == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // A raw string: r"…", r#"…"#, and any hash count above that.
        if byte == b'r' && !is_ident_byte(i.checked_sub(1).map(|p| bytes[p])) {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while j < n && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && bytes[j] == b'"' {
                let body_start = j + 1;
                let mut k = body_start;
                let end = loop {
                    if k >= n {
                        break n;
                    }
                    if bytes[k] == b'"' && bytes[k + 1..].iter().take(hashes).all(|b| *b == b'#') {
                        break k;
                    }
                    k += 1;
                };
                out.push(Token::Str(Literal::new(&starts, src, i, body_start, end)));
                i = (end + 1 + hashes).min(n);
                continue;
            }
        }

        // A char literal, not a lifetime: 'x', '→' or '\n'.
        if byte == b'\'' {
            if i + 1 < n && bytes[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < n && bytes[j] != b'\'' {
                    j += 1;
                }
                i = (j + 1).min(n);
                continue;
            }
            let width = src[i + 1..].chars().next().map_or(1, char::len_utf8);
            if bytes.get(i + 1 + width) == Some(&b'\'') {
                i += 2 + width;
                continue;
            }
            out.push(Token::Punct { at: i, ch: byte });
            i += 1;
            continue;
        }

        if byte == b'"' {
            let body_start = i + 1;
            let mut j = body_start;
            while j < n {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'"' {
                    break;
                }
                j += 1;
            }
            let end = j.min(n);
            out.push(Token::Str(Literal::new(&starts, src, i, body_start, end)));
            i = (end + 1).min(n);
            continue;
        }

        if is_ident_byte(Some(byte)) {
            let start = i;
            while i < n && is_ident_byte(Some(bytes[i])) {
                i += 1;
            }
            out.push(Token::Ident {
                at: start,
                name: src[start..i].to_string(),
            });
            continue;
        }

        if !byte.is_ascii_whitespace() {
            out.push(Token::Punct { at: i, ch: byte });
        }
        i += 1;
    }

    out
}

/// A declared item and the bytes it spans, its outer attributes included.
struct ItemSpan {
    path: String,
    start: usize,
    end: usize,
    is_test: bool,
}

/// An item whose name has been read and whose body has not opened yet.
struct Header {
    name: String,
    path: String,
    start: usize,
    depth: i32,
    is_test: bool,
    is_module: bool,
}

/// What a source declares: its items, and the modules it keeps in files of
/// their own behind `#[cfg(test)]`.
struct Items {
    spans: Vec<ItemSpan>,
    test_modules: Vec<String>,
}

/// Words that can stand between an outer attribute and the item keyword it
/// belongs to (`#[cfg(test)] pub(crate) unsafe fn`).
const ITEM_QUALIFIERS: &[&str] = &[
    "pub", "crate", "super", "self", "in", "async", "unsafe", "extern", "default",
];

/// The index of the bracket that closes the one at `open`.
fn matching_close(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::Punct {
                ch: b'(' | b'[' | b'{',
                ..
            } => depth += 1,
            Token::Punct {
                ch: b')' | b']' | b'}',
                ..
            } => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    tokens.len().saturating_sub(1)
}

fn is_cfg_test(attribute: &[Token]) -> bool {
    let words: Vec<&str> = attribute.iter().filter_map(Token::ident).collect();
    words == ["cfg", "test"]
}

/// The type an `impl` header is for: the last path segment after `for`, or of
/// the first type when there is no `for`, generics left out.
fn impl_target(tokens: &[Token], from: usize) -> Option<String> {
    let mut angle = 0i32;
    let mut name: Option<String> = None;
    for (index, token) in tokens.iter().enumerate().skip(from) {
        let previous = index.checked_sub(1).map(|p| &tokens[p]);
        match token {
            Token::Punct { ch: b'<', .. } => angle += 1,
            Token::Punct { ch: b'>', .. } if !previous.is_some_and(|t| t.is_punct(b'-')) => {
                angle -= 1;
            }
            Token::Punct {
                ch: b'{' | b';', ..
            } if angle == 0 => break,
            Token::Ident { name: word, .. } if angle == 0 => match word.as_str() {
                "for" => name = None,
                "where" => break,
                "dyn" | "mut" | "unsafe" | "const" => {}
                _ if name.is_none() || previous.is_some_and(|t| t.is_punct(b':')) => {
                    name = Some(word.clone());
                }
                _ => {}
            },
            _ => {}
        }
    }
    name
}

/// The name an item keyword at `index` declares, or `None` when the word is
/// not opening an item there (`'static`, `*const T`, `const fn`, `fn()`).
fn declared_name(tokens: &[Token], index: usize, keyword: &str) -> Option<String> {
    let next = tokens.get(index + 1).and_then(Token::ident);
    let previous = index.checked_sub(1).map(|p| &tokens[p]);
    match keyword {
        "fn" | "mod" | "enum" | "struct" | "trait" => next.map(str::to_string),
        "const" => {
            if previous.is_some_and(|t| t.is_punct(b'*')) {
                return None;
            }
            match next {
                Some("fn" | "unsafe" | "async" | "extern") | None => None,
                Some(name) => Some(name.to_string()),
            }
        }
        "static" => {
            if previous.is_some_and(|t| t.is_punct(b'\'')) {
                return None;
            }
            match next {
                Some("mut") => tokens
                    .get(index + 2)
                    .and_then(Token::ident)
                    .map(str::to_string),
                Some(name) => Some(name.to_string()),
                None => None,
            }
        }
        "impl" => impl_target(tokens, index + 1),
        "macro_rules" if tokens.get(index + 1).is_some_and(|t| t.is_punct(b'!')) => tokens
            .get(index + 2)
            .and_then(Token::ident)
            .map(str::to_string),
        _ => None,
    }
}

/// Every item a source declares, nested ones named by the path of the items
/// around them, and whether each sits behind `#[cfg(test)]`.
fn declared_items(tokens: &[Token]) -> Items {
    let mut spans: Vec<ItemSpan> = Vec::new();
    let mut test_modules: Vec<String> = Vec::new();
    let mut open: Vec<Header> = Vec::new();
    let mut header: Option<Header> = None;
    // Where the attributes before the next item start, and whether one of
    // them is `#[cfg(test)]`.
    let mut attributes: Option<(usize, bool)> = None;
    let mut depth = 0i32;
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        if let Some(pending) = header.as_ref() {
            if depth == pending.depth && (token.is_punct(b'{') || token.is_punct(b';')) {
                let pending = header.take().expect("a pending item");
                if token.is_punct(b'{') {
                    open.push(pending);
                    depth += 1;
                } else {
                    if pending.is_module && pending.is_test && depth == 0 {
                        test_modules.push(pending.name.clone());
                    }
                    spans.push(ItemSpan {
                        path: pending.path,
                        start: pending.start,
                        end: token.at() + 1,
                        is_test: pending.is_test,
                    });
                }
                index += 1;
                continue;
            }
        } else if token.is_punct(b'#') {
            let inner = tokens.get(index + 1).is_some_and(|t| t.is_punct(b'!'));
            let bracket = if inner { index + 2 } else { index + 1 };
            if tokens.get(bracket).is_some_and(|t| t.is_punct(b'[')) {
                let close = matching_close(tokens, bracket);
                if !inner {
                    let is_test = is_cfg_test(&tokens[bracket + 1..close]);
                    let (start, was_test) = attributes.unwrap_or((token.at(), false));
                    attributes = Some((start, was_test || is_test));
                }
                index = close + 1;
                continue;
            }
        } else if let Some(word) = token.ident() {
            if let Some(name) = declared_name(tokens, index, word) {
                let (start, attributed_test) = attributes.take().unwrap_or((token.at(), false));
                let parent = open.last();
                header = Some(Header {
                    path: match parent {
                        Some(parent) => format!("{}::{name}", parent.path),
                        None => name.clone(),
                    },
                    name,
                    start,
                    depth,
                    is_test: attributed_test || parent.is_some_and(|p| p.is_test),
                    is_module: word == "mod",
                });
            } else if !ITEM_QUALIFIERS.contains(&word) {
                attributes = None;
            }
        } else if !(token.is_punct(b'(') || token.is_punct(b')') || token.is_punct(b':')) {
            attributes = None;
        }

        match token {
            Token::Punct {
                ch: b'(' | b'[' | b'{',
                ..
            } => depth += 1,
            Token::Punct {
                ch: b')' | b']' | b'}',
                at,
            } => {
                depth -= 1;
                if let Some(item) = open.pop_if(|item| item.depth == depth) {
                    spans.push(ItemSpan {
                        path: item.path,
                        start: item.start,
                        end: at + 1,
                        is_test: item.is_test,
                    });
                }
            }
            _ => {}
        }
        index += 1;
    }

    Items {
        spans,
        test_modules,
    }
}

/// What the scan reads of one source: its string literals outside test code,
/// each with the item around it, and the test modules it keeps in other files.
struct SourceScan {
    literals: Vec<Literal>,
    test_modules: Vec<String>,
}

fn scan_source(src: &str) -> SourceScan {
    let tokens = tokens(src);
    let items = declared_items(&tokens);
    let mut literals = Vec::new();
    for token in tokens {
        let Token::Str(mut literal) = token else {
            continue;
        };
        let innermost = items
            .spans
            .iter()
            .filter(|span| span.start <= literal.at && literal.at < span.end)
            .max_by_key(|span| span.start);
        if innermost.is_some_and(|span| span.is_test) {
            continue;
        }
        literal.item = innermost.map(|span| span.path.clone()).unwrap_or_default();
        literals.push(literal);
    }
    SourceScan {
        literals,
        test_modules: items.test_modules,
    }
}

/// SQL is written in upper case here, and a message never is.
fn looks_like_sql(value: &str) -> bool {
    const SQL_WORDS: &[&str] = &[
        "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "REPLACE", "PRAGMA", "ALTER", "DROP",
        "WHERE", "FROM", "INTO", "VALUES", "JOIN", "TABLE", "INDEX", "VACUUM", "ATTACH",
    ];
    value
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|token| SQL_WORDS.contains(&token))
}

/// A message a person reads: several words, or one word that opens with a
/// capital the way a label does. A lower-case literal with no whitespace is a
/// key, an id, a path or a column name.
fn is_user_message(value: &str) -> bool {
    if !value.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let has_space = value.chars().any(char::is_whitespace);
    let opens_with_capital = value
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase());
    if !has_space && !opens_with_capital {
        return false;
    }
    !looks_like_sql(value)
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Every spelling of a banned word this guard answers to. A hyphen reads as a
/// space, so `syntax-highlighting` is the same violation as the phrase, and a
/// word ending in `e` inflects off its stem, so "refusing" and "debouncing"
/// count as much as "refused" and "debounced".
fn word_forms(word: &str) -> Vec<String> {
    let mut bases = vec![word.to_lowercase()];
    if word.eq_ignore_ascii_case("typography") {
        for extra in [
            "typographies",
            "typographic",
            "typographical",
            "typographically",
        ] {
            bases.push(extra.to_string());
        }
    }

    let mut forms = Vec::new();
    for base in &bases {
        let (head, last) = match base.rfind(' ') {
            Some(at) => (&base[..=at], &base[at + 1..]),
            None => ("", base.as_str()),
        };
        if let Some(stem) = last.strip_suffix('e') {
            for suffix in ["e", "es", "ed", "ing"] {
                forms.push(format!("{head}{stem}{suffix}"));
            }
        } else {
            for suffix in ["", "s", "es", "ed", "ing", "d"] {
                forms.push(format!("{head}{last}{suffix}"));
            }
        }
    }
    forms
}

/// The literal without its format placeholders.
///
/// `format!("{note} is not text.")` renders a file name, not the word: the
/// name inside the braces is an identifier the reader never sees, and matching
/// it reports a violation that does not exist. `{{` and `}}` are an escaped
/// brace rather than a placeholder and stay.
fn without_placeholders(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            b'{' => {
                // An unclosed brace swallows the rest, which is what the
                // formatter would refuse to compile in the first place.
                let end = value[i..].find('}').map_or(bytes.len(), |at| i + at + 1);
                out.push(' ');
                i = end;
            }
            _ => {
                let ch = value[i..].chars().next().expect("a char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// A case-insensitive whole-word match against every form of the word:
/// "Search buffers…", "refusing" and "syntax-highlighting" are the same
/// violations as "buffer", "refuse" and "syntax highlighting".
fn contains_banned_word(text: &str, word: &str) -> bool {
    let hay: String = without_placeholders(text)
        .to_lowercase()
        .chars()
        .map(|c| if c == '-' { ' ' } else { c })
        .collect();
    let bytes = hay.as_bytes();

    for needle in word_forms(word) {
        if needle.is_empty() {
            continue;
        }
        let mut from = 0usize;
        while let Some(rel) = hay[from..].find(&needle) {
            let start = from + rel;
            let end = start + needle.len();
            let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
            let after_ok = end == hay.len() || !is_word_byte(bytes[end]);
            if before_ok && after_ok {
                return true;
            }
            from = start + 1;
        }
    }
    false
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_rust_files(root: &Path, display: &str, out: &mut Vec<(String, PathBuf)>) {
    if root.is_file() {
        out.push((display.to_string(), root.to_path_buf()));
        return;
    }
    let entries = std::fs::read_dir(root)
        .unwrap_or_else(|err| panic!("read_dir {}: {err}", root.display()))
        .filter_map(Result::ok);
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if path.is_dir() {
            if SKIPPED_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            collect_rust_files(&path, &format!("{display}/{name}"), &mut found);
        } else if name.ends_with(".rs") {
            found.push((format!("{display}/{name}"), path));
        }
    }
    found.sort();
    out.extend(found);
}

/// The directory a file's child modules live in: its own directory for a
/// crate root or a `mod.rs`, a directory named for it otherwise.
fn module_dir(file: &Path) -> PathBuf {
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib" | "main" | "mod") | None => dir,
        Some(stem) => dir.join(stem),
    }
}

/// `true` when `file` is the module `#[cfg(test)] mod name;` in `parent`
/// declares, or a module inside it.
fn is_in_test_module(file: &Path, parent: &Path, name: &str) -> bool {
    let base = module_dir(parent);
    file == base.join(format!("{name}.rs")) || file.starts_with(base.join(name))
}

/// A source the scan reads, and what it found in it.
struct ScannedFile {
    display: String,
    path: PathBuf,
    scan: SourceScan,
}

/// Every scanned source, less the files that are test modules of another:
/// their strings are fixtures, such as the British spellings jotra-lint's
/// dictionary tests feed it. A file is a test module because its parent
/// declares it behind `#[cfg(test)]`, never because of its name.
fn scanned_sources() -> Vec<ScannedFile> {
    let mut files = Vec::new();
    for (relative, display) in SCANNED_ROOTS {
        let mut path = manifest_dir();
        for part in relative.split('/') {
            path.push(part);
        }
        collect_rust_files(&path, display, &mut files);
    }
    let scanned: Vec<ScannedFile> = files
        .into_iter()
        .map(|(display, path)| {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            ScannedFile {
                display,
                scan: scan_source(&src),
                path,
            }
        })
        .collect();
    let test_modules: Vec<(PathBuf, String)> = scanned
        .iter()
        .flat_map(|file| {
            file.scan
                .test_modules
                .iter()
                .map(|name| (file.path.clone(), name.clone()))
        })
        .collect();
    scanned
        .into_iter()
        .filter(|file| {
            !test_modules
                .iter()
                .any(|(parent, name)| is_in_test_module(&file.path, parent, name))
        })
        .collect()
}

/// A user message that says one of the words a guard looks for.
struct Offender {
    file: String,
    line: usize,
    item: String,
    value: String,
    word: &'static str,
}

impl Offender {
    fn describe(&self) -> String {
        let excerpt: String = self.value.chars().take(80).collect();
        format!(
            "{}:{} in `{}` says {:?}: {excerpt:?}",
            self.file, self.line, self.item, self.word
        )
    }
}

impl AllowedString {
    fn answers_for(&self, offender: &Offender) -> bool {
        self.file == offender.file
            && self.item == offender.item
            && self.word == offender.word
            && offender.value.contains(self.text)
    }

    fn describe(&self) -> String {
        format!(
            "{} in `{}`, {:?} in {:?} ({})",
            self.file, self.item, self.word, self.text, self.note
        )
    }
}

fn collect_offenders() -> Vec<Offender> {
    collect_matches(BANNED)
}

fn collect_matches(words: &[&'static str]) -> Vec<Offender> {
    let mut offenders = Vec::new();
    for file in scanned_sources() {
        let src = std::fs::read_to_string(&file.path)
            .unwrap_or_else(|err| panic!("read {}: {err}", file.path.display()));
        let skipped = skipped_lines(&src);
        for literal in &file.scan.literals {
            if skipped.get(literal.line - 1).copied().unwrap_or(false) {
                continue;
            }
            if literal.is_log_argument() || !is_user_message(&literal.value) {
                continue;
            }
            for word in words {
                if contains_banned_word(&literal.value, word) {
                    offenders.push(Offender {
                        file: file.display.clone(),
                        line: literal.line,
                        item: literal.item.clone(),
                        value: literal.value.clone(),
                        word,
                    });
                }
            }
        }
    }
    offenders
}

#[test]
fn banned_words_have_no_new_violations_in_rust_user_messages() {
    let offenders = collect_offenders();
    let mut new_violations: Vec<String> = offenders
        .iter()
        .filter(|offender| {
            !RUST_ALLOWLIST
                .iter()
                .any(|entry| entry.answers_for(offender))
        })
        .map(Offender::describe)
        .collect();
    for entry in RUST_ALLOWLIST {
        let answered: Vec<&Offender> = offenders
            .iter()
            .filter(|offender| entry.answers_for(offender))
            .collect();
        if answered.len() > 1 {
            new_violations.extend(answered.iter().map(|offender| {
                format!(
                    "{} (one record, {}, answers for {} strings)",
                    offender.describe(),
                    entry.describe(),
                    answered.len()
                )
            }));
        }
    }
    assert!(
        new_violations.is_empty(),
        "ADR-028 §10 retires this vocabulary from user-visible messages: {}",
        new_violations.join("\n")
    );
}

/// Decision OD-14: US spelling and one way of saying a failure ("Could not")
/// in every message, as the app's own strings do.
const OTHER_LOCALE_AND_VOICE: &[&str] = &[
    "licence",
    "colour",
    "behaviour",
    "organise",
    "recognise",
    "customise",
    "favourite",
    "centre",
    "cancelled",
    "couldn't",
];

#[test]
fn rust_user_messages_use_one_locale_and_one_voice() {
    let found: Vec<String> = collect_matches(OTHER_LOCALE_AND_VOICE)
        .iter()
        .map(Offender::describe)
        .collect();
    assert!(
        found.is_empty(),
        "user messages spell in US English and say \"Could not\": {}",
        found.join("\n")
    );
}

#[test]
fn rust_banned_words_allowlist_has_no_stale_entries() {
    let live = collect_offenders();
    let stale: Vec<String> = RUST_ALLOWLIST
        .iter()
        .filter(|entry| !live.iter().any(|offender| entry.answers_for(offender)))
        .map(AllowedString::describe)
        .collect();
    assert!(
        stale.is_empty(),
        "these allowlist records no longer match a live string; delete them in the same change: {}",
        stale.join(", ")
    );
}

/// The items and test code the scan sees in a source written to exercise
/// each way an item is declared.
const ITEM_FIXTURE: &str = r##"
#![doc = "Crate docs"]
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum Failure {
    #[error("Enum message")]
    Gone,
}

#[derive(Debug, thiserror::Error)]
#[error("Struct message")]
pub(crate) struct Lone;

pub const LABEL: &[(&str, &str)] = &[("Const message", "x")];
static NAME: &'static str = "Static message";

impl<'a, T: Clone> fmt::Display for Wrapper<'a, T> where T: Send {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pointer: *const u8 = std::ptr::null();
        let callback: fn() = || {};
        let arrow = '→';
        write!(f, "Impl message")
    }
}

pub(crate) async unsafe fn outer(limit: [u8; 2]) -> Result<(), String> {
    fn inner() -> &'static str {
        "Inner message"
    }
    Err("Outer message".to_string())
}

#[cfg(test)]
use std::path::Path;

fn after_a_test_use() -> &'static str {
    "After a test use"
}

#[cfg(test)]
pub(crate) mod helpers;

#[cfg(test)]
mod tests {
    fn fixture() -> &'static str {
        "Test message"
    }
}

fn after_the_tests() -> &'static str {
    "After the tests"
}
"##;

#[test]
fn each_literal_is_named_by_the_innermost_item_around_it() {
    let scan = scan_source(ITEM_FIXTURE);
    let named: Vec<(&str, &str)> = scan
        .literals
        .iter()
        .map(|literal| (literal.value.as_str(), literal.item.as_str()))
        .filter(|(value, _)| value.ends_with("message") || value.starts_with("After"))
        .collect();
    assert_eq!(
        named,
        [
            ("Enum message", "Failure"),
            ("Struct message", "Lone"),
            ("Const message", "LABEL"),
            ("Static message", "NAME"),
            ("Impl message", "Wrapper::fmt"),
            ("Inner message", "outer::inner"),
            ("Outer message", "outer"),
            ("After a test use", "after_a_test_use"),
            ("After the tests", "after_the_tests"),
        ]
    );
    assert_eq!(scan.test_modules, ["helpers"]);
}

#[test]
fn a_file_is_a_test_module_only_where_its_parent_declares_one() {
    let root = Path::new("crate").join("src");
    let lib = root.join("lib.rs");
    assert!(is_in_test_module(&root.join("tests.rs"), &lib, "tests"));
    assert!(is_in_test_module(
        &root.join("tests").join("cases.rs"),
        &lib,
        "tests"
    ));
    assert!(!is_in_test_module(&root.join("tests.rs"), &lib, "helpers"));

    let parent = root.join("watcher").join("handler.rs");
    let child = root.join("watcher").join("handler").join("tests.rs");
    assert!(is_in_test_module(&child, &parent, "tests"));
    assert!(!is_in_test_module(
        &root.join("watcher").join("tests.rs"),
        &parent,
        "tests"
    ));
}
