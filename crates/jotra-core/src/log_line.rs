//! What a log line may say about where a file is (ADR-045).
//!
//! A log line names a file by its name, never by its full path. Error values
//! carry paths in their own text, so the log writer passes every line through
//! [`redact_paths`] rather than relying on each call site.

use std::borrow::Cow;

/// `line` with each absolute path cut down to its last component.
///
/// A path starts at a root (`/`, `~/`, `C:\`, `\\server` or `\\?\`) that
/// follows a space, a bracket, a quote, `=` or the start of the line. It runs
/// to the first place a log line ends one: a line break, a double quote or a
/// backtick, a closing quote or bracket, `", "`, `": "`, `"; "`, a `name=`
/// field, or the next path. A path holding spaces keeps them, so the text
/// after its file name stays.
pub fn redact_paths(line: &str) -> Cow<'_, str> {
    let starts = path_starts(line);
    if starts.is_empty() {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut copied = 0;
    for (index, &start) in starts.iter().enumerate() {
        if start < copied {
            continue;
        }
        let bound = starts.get(index + 1).copied().unwrap_or(line.len());
        let end = path_end(line, start, bound);
        out.push_str(&line[copied..start]);
        out.push_str(last_component(&line[start..end]));
        copied = end;
    }
    out.push_str(&line[copied..]);
    Cow::Owned(out)
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

fn opens_a_path(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | '"' | '\'' | '`' | '='),
    }
}

/// Byte offsets where an absolute path begins.
fn path_starts(line: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let mut starts = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (offset, c) = chars[i];
        let prev = i.checked_sub(1).and_then(at);
        if !opens_a_path(prev) {
            i += 1;
            continue;
        }
        let next = at(i + 1);
        let begins = match c {
            '/' => next.is_some_and(|n| !n.is_whitespace() && n != '/'),
            '~' => next.is_some_and(is_separator),
            '\\' => next == Some('\\') && at(i + 2).is_some_and(|n| !n.is_whitespace()),
            letter if letter.is_ascii_alphabetic() => {
                next == Some(':') && at(i + 2).is_some_and(is_separator)
            }
            _ => false,
        };
        if begins {
            starts.push(offset);
            i += 2;
        } else {
            i += 1;
        }
    }
    starts
}

/// Whether the character after a closing quote or bracket ends the path there.
fn closes(next: Option<char>) -> bool {
    next.is_none_or(|n| !(n.is_alphanumeric() || is_separator(n) || matches!(n, '_' | '-')))
}

/// Whether `rest`, which follows a space, opens a `name=` field.
fn opens_a_field(rest: &str) -> bool {
    let mut chars = rest.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return false;
    }
    for c in chars {
        if c == '=' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.')) {
            return false;
        }
    }
    false
}

/// Where the path that starts at `start` ends, at `bound` at the latest.
fn path_end(line: &str, start: usize, bound: usize) -> usize {
    let segment = &line[start..bound];
    let mut chars = segment.char_indices().peekable();
    chars.next();
    while let Some((offset, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let ends = match c {
            '\n' | '\r' | '"' | '`' => true,
            '\'' | ')' | ']' | '}' | '>' => closes(next),
            ',' | ':' | ';' => next.is_some_and(char::is_whitespace),
            c if c.is_whitespace() => opens_a_field(&segment[offset + c.len_utf8()..]),
            _ => false,
        };
        if ends {
            return start + offset;
        }
    }
    bound
}

/// The text after the last separator that a name follows.
fn last_component(path: &str) -> &str {
    let mut cut = None;
    let mut chars = path.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        let named = chars
            .peek()
            .is_some_and(|&(_, n)| !is_separator(n) && !n.is_whitespace());
        if is_separator(c) && named {
            cut = Some(offset + c.len_utf8());
        }
    }
    cut.map_or(path, |cut| &path[cut..])
}
