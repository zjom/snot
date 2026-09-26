//! Pass 1: split the source into lines and classify each one (NOTE_SPEC 2.2),
//! finding code and math blocks (NOTE_SPEC 9.1) on the way.

use crate::{Block, BlockKind, Code, Diagnostic, Eol, Marker, Span, Task};

/// A classified line, before scopes are known.
pub(crate) struct RawLine {
    pub span: Span,
    pub eol: Eol,
    /// Bytes of leading whitespace.
    pub indent: usize,
    pub depth: usize,
    pub kind: RawKind,
}

pub(crate) enum RawKind {
    Blank,
    Heading {
        level: u8,
        text: Span,
    },
    Item {
        marker: Marker,
        marker_span: Span,
        task: Option<Task>,
        content: Span,
    },
    Row,
    Text,
    /// The opening fence of the block with this index.
    FenceOpen(usize),
    FenceClose,
    Verbatim,
}

pub(crate) fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// How the open block ends.
enum Closer {
    /// At least this many backticks.
    Code(usize),
    Math,
}

/// Split `src` into lines, dropping a leading byte order mark.
fn split(src: &str) -> Vec<(Span, Eol)> {
    let bytes = src.as_bytes();
    let mut start = if src.starts_with('\u{feff}') { 3 } else { 0 };
    let mut out = Vec::new();
    while start < bytes.len() {
        match bytes[start..].iter().position(|&b| b == b'\n') {
            Some(k) => {
                let end = start + k;
                if end > start && bytes[end - 1] == b'\r' {
                    out.push((start..end - 1, Eol::CrLf));
                } else {
                    out.push((start..end, Eol::Lf));
                }
                start = end + 1;
            }
            None => {
                out.push((start..bytes.len(), Eol::None));
                break;
            }
        }
    }
    out
}

/// A code fence opener: 3 or more backticks, then an info string with no
/// backtick. Returns the backtick count and the language's span in `rest`.
fn code_open(rest: &[u8]) -> Option<(usize, Option<Span>)> {
    let n = rest.iter().take_while(|&&b| b == b'`').count();
    if n < 3 || rest[n..].contains(&b'`') {
        return None;
    }
    let start = n + rest[n..].iter().take_while(|&&b| is_ws(b)).count();
    let len = rest[start..].iter().take_while(|&&b| !is_ws(b)).count();
    Some((n, (len > 0).then(|| start..start + len)))
}

fn trim_end(s: &[u8]) -> &[u8] {
    let n = s.iter().rev().take_while(|&&b| is_ws(b)).count();
    &s[..s.len() - n]
}

fn closes(rest: &[u8], closer: &Closer) -> bool {
    let rest = trim_end(rest);
    match *closer {
        Closer::Code(n) => rest.len() >= n && rest.iter().all(|&b| b == b'`'),
        Closer::Math => rest == b"$$",
    }
}

/// A list item marker at the start of `rest`, and its length without the space.
fn item_marker(rest: &[u8]) -> Option<(Marker, usize)> {
    match rest {
        [b'-', b' ', ..] => Some((Marker::Dash, 1)),
        [b'+', b' ', ..] => Some((Marker::Plus, 1)),
        _ => {
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits == 0 || rest.get(digits) != Some(&b'.') || rest.get(digits + 1) != Some(&b' ')
            {
                return None;
            }
            let n = rest[..digits].iter().fold(0u64, |n, d| {
                n.saturating_mul(10).saturating_add(u64::from(d - b'0'))
            });
            Some((Marker::Number(n), digits + 1))
        }
    }
}

/// A task box at the start of an item's content, followed by a space or the
/// end of the line. Returns the state and the bytes to skip.
fn task_box(content: &[u8]) -> Option<(Task, usize)> {
    let task = match content.get(..3)? {
        b"[ ]" => Task::Open,
        b"[x]" => Task::Done,
        b"[-]" => Task::Cancelled,
        _ => return None,
    };
    match content.get(3) {
        None => Some((task, 3)),
        Some(b' ') => Some((task, 4)),
        Some(_) => None,
    }
}

pub(crate) fn classify(src: &str, diags: &mut Vec<Diagnostic>) -> (Vec<RawLine>, Vec<Block>) {
    let bytes = src.as_bytes();
    let mut lines = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<(usize, Closer)> = None;

    for (idx, (span, eol)) in split(src).into_iter().enumerate() {
        let s = span.start;
        let line = &bytes[span.clone()];
        let indent = line.iter().take_while(|&&b| is_ws(b)).count();
        let columns: usize = line[..indent]
            .iter()
            .map(|&b| if b == b'\t' { 2 } else { 1 })
            .sum();
        let rest = &line[indent..];
        let mut raw = RawLine {
            span: span.clone(),
            eol,
            indent,
            depth: columns / 2,
            kind: RawKind::Text,
        };

        if let Some((block, closer)) = &open {
            if closes(rest, closer) {
                blocks[*block].close = Some(idx);
                raw.kind = RawKind::FenceClose;
                open = None;
            } else {
                raw.kind = RawKind::Verbatim;
            }
            lines.push(raw);
            continue;
        }

        if !rest.is_empty()
            && let Some(tab) = line[..indent].iter().position(|&b| b == b'\t')
        {
            diags.push(Diagnostic::new(
                Code::TabIndent,
                s + tab..s + tab + 1,
                "tab in indentation; indent with spaces",
            ));
        }

        raw.kind = if let Some((ticks, language)) = code_open(rest) {
            let language =
                language.map(|l| src[s + indent + l.start..s + indent + l.end].to_owned());
            open = Some((blocks.len(), Closer::Code(ticks)));
            blocks.push(Block {
                kind: BlockKind::Code { language },
                open: idx,
                close: None,
                indent,
            });
            RawKind::FenceOpen(blocks.len() - 1)
        } else if trim_end(rest) == b"$$" {
            open = Some((blocks.len(), Closer::Math));
            blocks.push(Block {
                kind: BlockKind::Math,
                open: idx,
                close: None,
                indent,
            });
            RawKind::FenceOpen(blocks.len() - 1)
        } else if rest.is_empty() {
            RawKind::Blank
        } else if let Some(level) = heading_level(line) {
            RawKind::Heading {
                level,
                text: s + usize::from(level) + 1..span.end,
            }
        } else if let Some((marker, len)) = item_marker(rest) {
            let marker_start = s + indent;
            let mut content = marker_start + len + 1..span.end;
            let task = task_box(&bytes[content.clone()]).map(|(task, skip)| {
                content.start += skip;
                task
            });
            RawKind::Item {
                marker,
                marker_span: marker_start..marker_start + len,
                task,
                content,
            }
        } else if rest[0] == b'|' {
            RawKind::Row
        } else {
            RawKind::Text
        };
        lines.push(raw);
    }

    if let Some((block, _)) = open {
        let fence = &lines[blocks[block].open];
        diags.push(Diagnostic::new(
            Code::UnclosedBlock,
            fence.span.clone(),
            "block has no closing fence, so it runs to the end of the file",
        ));
    }
    (lines, blocks)
}

/// 1 to 6 `#` at column 0 then a space.
fn heading_level(line: &[u8]) -> Option<u8> {
    let n = line.iter().take_while(|&&b| b == b'#').count();
    ((1..=6).contains(&n) && line.get(n) == Some(&b' ')).then_some(n as u8)
}
