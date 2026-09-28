//! Formatter for Simple Note Format.
//!
//! [`fmt`] returns the edits that format a note, so the CLI and the language
//! server share one code path; [`format`] applies them to a string. The rules
//! are conservative, so formatting never changes what a note means:
//!
//! 1. The metadata ending a heading or list item is aligned to end at
//!    [`FmtOptions::width`], padded with spaces, so the alignment survives any
//!    tab width. With no room, one space separates it from the text.
//! 2. Trailing whitespace is trimmed outside code and math blocks.
//! 3. The file ends with exactly one end of line, in the file's style. Blank
//!    lines at the end are dropped; a file of only blank lines becomes empty.

use std::borrow::Cow;

use snot_syntax::{Document, Eol, LineKind, ScopeKind, Span, parse};
use unicode_width::UnicodeWidthStr;

/// How to format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FmtOptions {
    /// The display column trailing metadata ends at.
    pub width: usize,
}

impl Default for FmtOptions {
    fn default() -> Self {
        FmtOptions { width: 79 }
    }
}

/// Replace the bytes in `range` of the source with `new_text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    /// A byte range in the source.
    pub range: Span,
    /// The replacement.
    pub new_text: String,
}

/// Format `src`.
pub fn format(src: &str, opts: &FmtOptions) -> String {
    apply(src, &fmt(src, &parse(src), opts))
}

/// Apply edits that are ordered and don't overlap, as [`fmt`] returns them.
pub fn apply(src: &str, edits: &[TextEdit]) -> String {
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for edit in edits {
        out.push_str(&src[at..edit.range.start]);
        out.push_str(&edit.new_text);
        at = edit.range.end;
    }
    out.push_str(&src[at..]);
    out
}

/// The edits formatting `src`, which `doc` is the parse of: ordered by
/// position, not overlapping, and none when `src` is already formatted. Each
/// changed line gets its own edit, with one more for the end of the file.
pub fn fmt(src: &str, doc: &Document, opts: &FmtOptions) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    // Blank lines inside a block are verbatim, not blank, so they are kept.
    let Some(last) = doc.lines.iter().rposition(|l| l.kind != LineKind::Blank) else {
        let start = doc.lines.first().map_or(src.len(), |l| l.span.start);
        if start < src.len() {
            edits.push(TextEdit {
                range: start..src.len(),
                new_text: String::new(),
            });
        }
        return edits;
    };

    let mut ends_in_cr = false;
    for (i, line) in doc.lines[..=last].iter().enumerate() {
        let text = &src[line.span.clone()];
        // Only an end of file lets a line end in CR; see `trim_end`.
        let cr_ok = line.eol != Eol::Lf;
        let new: Cow<str> = match line.kind {
            LineKind::Verbatim => text.into(),
            _ => match trailing_metadata(src, doc, i) {
                Some(start) => {
                    let text = trim_end(&src[line.span.start..start], true);
                    let meta = trim_end(&src[start..line.span.end], cr_ok);
                    let pad = opts
                        .width
                        .saturating_sub(width(meta))
                        .saturating_sub(width(text))
                        .max(1);
                    format!("{text}{}{meta}", " ".repeat(pad)).into()
                }
                None => {
                    let keep = kept(src, doc, i) - line.span.start;
                    text[..trim_end(text, cr_ok).len().max(keep)].into()
                }
            },
        };
        if new != text {
            edits.push(TextEdit {
                range: line.span.clone(),
                new_text: new.to_string(),
            });
        }
        ends_in_cr = new.ends_with('\r');
    }

    let eol = match doc.lines[last].eol {
        Eol::Lf => "\n",
        // A CR ending the file ends the line in CRLF, so the CR stays text.
        Eol::CrLf => "\r\n",
        Eol::None if ends_in_cr => "\r\n",
        Eol::None => match doc.lines.iter().find(|l| l.eol != Eol::None) {
            Some(l) if l.eol == Eol::CrLf => "\r\n",
            _ => "\n",
        },
    };
    let end = doc.lines[last].span.end..src.len();
    if &src[end.clone()] != eol {
        edits.push(TextEdit {
            range: end,
            new_text: eol.to_owned(),
        });
    }
    edits
}

/// Where the metadata ending line `i` starts, if it is a heading or list item
/// ending in tokens with text before them.
fn trailing_metadata(src: &str, doc: &Document, i: usize) -> Option<usize> {
    let line = &doc.lines[i];
    let content = match (line.kind, &doc.scope(line.owner?).kind) {
        (LineKind::Heading, ScopeKind::Heading { text, .. }) => text.clone(),
        (LineKind::Item, ScopeKind::Item(item)) => item.content.clone(),
        _ => return None,
    };
    let tokens = &doc.tokens
        [doc.tokens.partition_point(|t| t.line < i)..doc.tokens.partition_point(|t| t.line <= i)];
    let mut start = None;
    let mut end = line.span.end;
    for token in tokens.iter().rev() {
        if !is_blank(&src[token.span.end..end]) {
            break;
        }
        start = Some(token.span.start);
        end = token.span.start;
    }
    let start = start?;
    let before = trim_end(&src[content.start..start], true);
    (!before.is_empty() && kept(src, doc, i) <= content.start + before.len()).then_some(start)
}

/// Where trimming line `i` must stop. A heading or list item marker keeps the
/// space after it: `# ` is an empty heading, `#` is text. And `- [x]\t` isn't
/// a task, so its tab stays.
fn kept(src: &str, doc: &Document, i: usize) -> usize {
    let line = &doc.lines[i];
    match line.owner.map(|s| &doc.scope(s).kind) {
        Some(ScopeKind::Heading { text, .. }) if line.kind == LineKind::Heading => text.start,
        Some(ScopeKind::Item(item)) if line.kind == LineKind::Item => {
            let content = &src[item.content.clone()];
            let is_box = matches!(content.get(..3), Some("[ ]" | "[x]" | "[-]"));
            if item.task.is_none() && is_box && content.len() > 3 {
                item.content.start + 4
            } else {
                item.marker_span.end + 1
            }
        }
        _ => line.span.start,
    }
}

fn is_blank(s: &str) -> bool {
    s.bytes().all(|b| b == b' ' || b == b'\t')
}

/// `s` without trailing whitespace. Unless `cr_ok`, a CR that would be left
/// at the end keeps one whitespace after it, since CR then LF is one CRLF end
/// of line, not a CR in the text.
fn trim_end(s: &str, cr_ok: bool) -> &str {
    let t = s.trim_end_matches([' ', '\t']);
    if !cr_ok && t.ends_with('\r') && t.len() < s.len() {
        &s[..t.len() + 1]
    } else {
        t
    }
}

/// Display width, with tab stops every 8 columns.
fn width(s: &str) -> usize {
    let mut parts = s.split('\t');
    let mut col = parts.next().map_or(0, UnicodeWidthStr::width);
    for part in parts {
        col = (col / 8 + 1) * 8 + part.width();
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(width: usize, src: &str) -> String {
        format(src, &FmtOptions { width })
    }

    /// The cases in snot.nvim's `align` spec, which pads with tabs unless
    /// 'expandtab' is set; here padding is always spaces.
    #[test]
    fn matches_align_lua() {
        for (src, want) in [
            (
                "# Kickoff @project:atlas @a",
                "# Kickoff    @project:atlas @a",
            ),
            (
                "## Decisions\t@id:decisions  ",
                "## Decisions     @id:decisions",
            ),
            (
                "  - [ ] Plan @due:2026-09-30",
                "  - [ ] Plan   @due:2026-09-30",
            ),
            ("+ Ship @x", "+ Ship                      @x"),
            ("12. [x] Room @x", "12. [x] Room                @x"),
            // Too long: one space.
            (
                "# A long heading about things    @a @b",
                "# A long heading about things @a @b",
            ),
            ("# ünï @a", "# ünï                       @a"),
            ("# Kickoff @project:x", "# Kickoff           @project:x"),
        ] {
            assert_eq!(at(30, src), format!("{want}\n"), "{src:?}");
        }
    }

    #[test]
    fn leaves_other_lines_alone() {
        for src in [
            "text @a",
            "| row | @a |",
            "# @work",
            "- [ ] @due:2026-09-30",
            "# Tokens @a in the middle",
            "# Code `@a`",
            "# Link [[x|@a]]",
            "####### Seven @a",
            "#Nospace @a",
        ] {
            assert_eq!(at(30, src), format!("{src}\n"), "{src:?}");
        }
    }

    #[test]
    fn measures_wide_characters() {
        assert_eq!(at(12, "# 日本 @a"), "# 日本    @a\n");
    }

    #[test]
    fn trims_trailing_whitespace_outside_blocks() {
        assert_eq!(
            at(79, "a  \n```\ncode  \n```  \n$$\nx \t\n$$\nb\t\n"),
            "a\n```\ncode  \n```\n$$\nx \t\n$$\nb\n"
        );
    }

    #[test]
    fn keeps_the_space_after_a_marker() {
        assert_eq!(
            at(79, "#   \n## \t\n##\t\n-  \n1. \n- [ ]  \n"),
            "# \n## \n##\n- \n1. \n- [ ]\n"
        );
    }

    #[test]
    fn keeps_a_tab_that_stops_a_task() {
        assert_eq!(at(20, "- [x]\t@a\n- [x]\t\n"), "- [x]\t@a\n- [x]\t\n");
    }

    #[test]
    fn keeps_a_cr_in_the_text() {
        assert_eq!(at(79, "a\r"), "a\r\r\n");
        assert_eq!(at(79, "a\r  \nb"), "a\r \nb\n");
        assert_eq!(at(79, "a\r  \r\n"), "a\r\r\n");
    }

    #[test]
    fn skips_blocks() {
        assert_eq!(at(10, "```\n# A @b\n```\n"), "```\n# A @b\n```\n");
    }

    #[test]
    fn ends_with_one_newline() {
        assert_eq!(at(79, "a"), "a\n");
        assert_eq!(at(79, "a\n\n  \n\n"), "a\n");
        assert_eq!(at(79, "a\r\nb"), "a\r\nb\r\n");
        assert_eq!(at(79, "a\r\n\r\n"), "a\r\n");
        assert_eq!(at(79, ""), "");
        assert_eq!(at(79, "\n  \n"), "");
        assert_eq!(at(79, "\u{feff}\n"), "\u{feff}");
        assert_eq!(at(10, "\u{feff}# A @b"), "\u{feff}# A     @b\n");
    }

    #[test]
    fn keeps_blank_lines_in_an_unclosed_block() {
        assert_eq!(at(79, "```\na\n\n"), "```\na\n\n");
        assert_eq!(at(79, "$$\n  "), "$$\n  \n");
    }

    #[test]
    fn keeps_crlf() {
        assert_eq!(at(10, "# A @b  \r\n"), "# A     @b\r\n");
    }

    #[test]
    fn returns_no_edits_when_formatted() {
        let src = "# A     @b\n\n- x\n";
        assert_eq!(fmt(src, &parse(src), &FmtOptions { width: 10 }), vec![]);
    }
}
