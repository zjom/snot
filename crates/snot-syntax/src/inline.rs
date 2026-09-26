//! Inline content (NOTE_SPEC 6 to 9.3): escapes, code and math spans, links,
//! metadata tokens and emphasis, recognised left to right. In a table row,
//! unprotected `|`s also split the cells (NOTE_SPEC 10.1).

use crate::is_key_byte;
use crate::lines::is_ws;
use crate::{InlineKind, Span, TokenForm};

pub(crate) struct RawToken {
    pub span: Span,
    pub key_span: Span,
    pub form: TokenForm,
    pub values: Vec<String>,
    pub value_span: Option<Span>,
}

pub(crate) struct RawLink {
    pub span: Span,
    pub target: String,
    pub target_span: Span,
    pub label: Option<String>,
    pub label_span: Option<Span>,
}

#[derive(Default)]
pub(crate) struct Scanned {
    pub tokens: Vec<RawToken>,
    pub links: Vec<RawLink>,
    /// Kind, whole span, content span.
    pub inlines: Vec<(InlineKind, Span, Span)>,
    /// In a row, each cell's content, trimmed.
    pub cells: Vec<Span>,
}

fn is_punct(b: u8) -> bool {
    b.is_ascii_punctuation()
}

/// Remove escapes: a backslash before ASCII punctuation yields the punctuation.
pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && next.is_ascii_punctuation()
        {
            out.push(next);
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn trim_ws(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

fn trim_span(b: &[u8], mut span: Span) -> Span {
    while span.start < span.end && is_ws(b[span.start]) {
        span.start += 1;
    }
    while span.end > span.start && is_ws(b[span.end - 1]) {
        span.end -= 1;
    }
    span
}

/// The first unescaped `sep` in `span`, if any.
fn find_unescaped(b: &[u8], span: Span, sep: u8) -> Option<usize> {
    let mut i = span.start;
    while i < span.end {
        if b[i] == b'\\' && i + 1 < span.end && is_punct(b[i + 1]) {
            i += 2;
        } else if b[i] == sep {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

/// `span` split at unescaped `sep`s.
fn split_unescaped(b: &[u8], span: Span, sep: u8) -> Vec<Span> {
    let mut out = Vec::new();
    let mut rest = span;
    while let Some(i) = find_unescaped(b, rest.clone(), sep) {
        out.push(rest.start..i);
        rest.start = i + 1;
    }
    out.push(rest);
    out
}

fn backtick_run(b: &[u8], i: usize, end: usize) -> usize {
    b[i..end].iter().take_while(|&&c| c == b'`').count()
}

/// Inline code opening with `n` backticks at `i` closes at the next run of
/// exactly `n`. Returns where the closing run starts.
fn code_close(b: &[u8], i: usize, end: usize, n: usize) -> Option<usize> {
    let mut j = i + n;
    while j < end {
        if b[j] == b'`' {
            let m = backtick_run(b, j, end);
            if m == n {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

/// Inline math opening at `i` closes at the next `$` after a non-whitespace
/// character and not before a digit.
fn math_close(b: &[u8], i: usize, end: usize) -> Option<usize> {
    (i + 2..end)
        .find(|&j| b[j] == b'$' && !is_ws(b[j - 1]) && !(j + 1 < end && b[j + 1].is_ascii_digit()))
}

/// A link opening at `i`, closing at the first unescaped `]]` on the line. An
/// empty target makes it text.
fn read_link(src: &str, i: usize, end: usize) -> Option<RawLink> {
    let b = src.as_bytes();
    let mut j = i + 2;
    let close = loop {
        if j + 1 >= end {
            return None;
        }
        if b[j] == b'\\' && is_punct(b[j + 1]) {
            j += 2;
        } else if b[j] == b']' && b[j + 1] == b']' {
            break j;
        } else {
            j += 1;
        }
    };
    let inner = i + 2..close;
    let (target_span, label_span) = match find_unescaped(b, inner.clone(), b'|') {
        Some(p) => (inner.start..p, Some(p + 1..close)),
        None => (inner, None),
    };
    let target = unescape(trim_ws(&src[target_span.clone()]));
    if target.is_empty() {
        return None;
    }
    Some(RawLink {
        span: i..close + 2,
        target,
        target_span,
        label: label_span.clone().map(|l| unescape(trim_ws(&src[l]))),
        label_span,
    })
}

/// A token whose `@` is at `i`. In a row, values end at an unescaped `|`.
fn read_token(src: &str, i: usize, end: usize, row: bool) -> Option<RawToken> {
    let b = src.as_bytes();
    if i + 1 >= end || !b[i + 1].is_ascii_alphabetic() {
        return None;
    }
    let j = i
        + 1
        + b[i + 1..end]
            .iter()
            .take_while(|&&c| is_key_byte(c))
            .count();
    let flag = RawToken {
        span: i..j,
        key_span: i + 1..j,
        form: TokenForm::Flag,
        values: vec!["true".to_owned()],
        value_span: None,
    };
    // A flag ends at whitespace or the end of the line (or cell); anything
    // else after the key, other than `:` and a value, makes the token text.
    if j >= end || is_ws(b[j]) || (row && b[j] == b'|') {
        return Some(flag);
    }
    if b[j] != b':' {
        return None;
    }

    if b.get(j + 1) == Some(&b'[') && j + 1 < end {
        let mut m = j + 2;
        let close = loop {
            if m >= end {
                return None; // an unclosed list makes the whole token text
            }
            match b[m] {
                b'\\' if m + 1 < end && is_punct(b[m + 1]) => m += 2,
                b']' => break m,
                b'|' if row => return None,
                _ => m += 1,
            }
        };
        let values = split_unescaped(b, j + 2..close, b',')
            .into_iter()
            .map(|item| unescape(trim_ws(&src[item])))
            .filter(|v| !v.is_empty())
            .collect();
        return Some(RawToken {
            span: i..close + 1,
            key_span: i + 1..j,
            form: TokenForm::List,
            values,
            value_span: Some(j + 1..close + 1),
        });
    }

    let mut m = j + 1;
    while m < end {
        match b[m] {
            b'\\' if m + 1 < end && is_punct(b[m + 1]) => m += 2,
            b' ' | b'\t' => break,
            b'|' if row => break,
            _ => m += 1,
        }
    }
    if m == j + 1 {
        return None; // `@key:` with no value
    }
    Some(RawToken {
        span: i..m,
        key_span: i + 1..j,
        form: TokenForm::Scalar,
        values: vec![unescape(&src[j + 1..m])],
        value_span: Some(j + 1..m),
    })
}

/// Scan `span` of `src` as inline content, on the line starting at
/// `line_start`. For a row, `span` starts at the first `|`.
pub(crate) fn scan(src: &str, line_start: usize, span: Span, row: bool) -> Scanned {
    let b = src.as_bytes();
    let end = span.end;
    let mut out = Scanned::default();
    let mut i = span.start;
    let mut segment_start = span.start;
    if row {
        i += 1;
        segment_start = i;
    }
    let mut segments = Vec::new();
    // Emphasis delimiter candidates: position and segment.
    let mut delims: Vec<(usize, usize)> = Vec::new();

    while i < end {
        match b[i] {
            b'\\' if i + 1 < end && is_punct(b[i + 1]) => i += 2,
            b'`' => {
                let n = backtick_run(b, i, end);
                match code_close(b, i, end, n) {
                    Some(c) => {
                        let mut content = i + n..c;
                        if content.len() >= 2
                            && b[content.start] == b' '
                            && b[content.end - 1] == b' '
                        {
                            content = content.start + 1..content.end - 1;
                        }
                        out.inlines.push((InlineKind::Code, i..c + n, content));
                        i = c + n;
                    }
                    None => i += n,
                }
            }
            b'$' if i + 1 < end && !is_ws(b[i + 1]) => match math_close(b, i, end) {
                Some(c) => {
                    out.inlines.push((InlineKind::Math, i..c + 1, i + 1..c));
                    i = c + 1;
                }
                None => i += 1,
            },
            b'[' if i + 1 < end && b[i + 1] == b'[' => match read_link(src, i, end) {
                Some(link) => {
                    i = link.span.end;
                    out.links.push(link);
                }
                None => i += 1,
            },
            b'@' if i == segment_start || is_ws(b[i - 1]) => match read_token(src, i, end, row) {
                Some(token) => {
                    i = token.span.end;
                    out.tokens.push(token);
                }
                None => i += 1,
            },
            b'|' if row => {
                segments.push(segment_start..i);
                i += 1;
                segment_start = i;
            }
            b'*' | b'_' => {
                delims.push((i, segments.len()));
                i += 1;
            }
            _ => i += 1,
        }
    }
    segments.push(segment_start..end);

    for (k, segment) in segments.iter().enumerate() {
        let candidates = delims.iter().filter(|d| d.1 == k).map(|d| d.0);
        emphasis(b, line_start, segment.clone(), candidates, &mut out.inlines);
    }

    if row {
        // A trailing `|` leaves an empty last cell, which is dropped.
        if segments.len() > 1 && trim_span(b, segments[segments.len() - 1].clone()).is_empty() {
            segments.pop();
        }
        out.cells = segments.into_iter().map(|s| trim_span(b, s)).collect();
    }
    out
}

/// Pair emphasis delimiters within one segment (NOTE_SPEC 8).
fn emphasis(
    b: &[u8],
    line_start: usize,
    segment: Span,
    candidates: impl Iterator<Item = usize>,
    out: &mut Vec<(InlineKind, Span, Span)>,
) {
    let mut open: Vec<usize> = Vec::new();
    for p in candidates {
        let c = b[p];
        let prev = (p > line_start).then(|| b[p - 1]);
        let next = (p + 1 < segment.end).then(|| b[p + 1]);
        let can_open =
            prev.is_none_or(|x| is_ws(x) || is_punct(x)) && next.is_some_and(|x| !is_ws(x));
        let can_close = p > segment.start
            && prev.is_some_and(|x| !is_ws(x))
            && next.is_none_or(|x| is_ws(x) || is_punct(x));

        if let Some(k) = open.iter().rposition(|&q| b[q] == c) {
            // Bold and underline don't nest in themselves: with one open, the
            // same delimiter can only close it.
            let start = open[k];
            if can_close && p > start + 1 {
                let kind = if c == b'*' {
                    InlineKind::Bold
                } else {
                    InlineKind::Underline
                };
                out.push((kind, start..p + 1, start + 1..p));
                open.truncate(k); // delimiters opened inside and not closed are text
            }
        } else if can_open {
            open.push(p);
        }
    }
}
