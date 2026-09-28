//! The formatter's invariants: formatting is idempotent, and never changes what
//! a note means.

use proptest::prelude::*;
use snot_fmt::{FmtOptions, apply, fmt, format};
use snot_syntax::{Document, LineKind, ScopeKind, parse};

/// Words for generated lines, including ones that look like metadata but
/// aren't, and inline spans that tokens can't start inside.
const WORDS: &[&str] = &[
    "a", "Plan", "é", "日本", "e\u{301}", "*b*", "_u_", "*b", "`@a`", "`x`", "$m$", "$", "\\",
    "\\@a", "a@b", "[[x]]", "[[x|@y]]", "[[#a]]", "@", "@a,", "@k:", "|", "\\|", "[[", "]]",
];
const TOKENS: &[&str] = &[
    "@a",
    "@urgent",
    "@due:2026-10-01",
    "@id:x",
    "@id:y",
    "@tags:[a, b]",
    "@k:y*",
    "@k:_",
    "@k:`",
    "@k:[a\\, b]",
    "@x:$",
];
const SPACE: &[&str] = &[" ", "  ", "\t", " \t "];
const TRAILING: &[&str] = &["", "", " ", "  ", "\t", " \t"];

fn pick(from: &'static [&'static str]) -> impl Strategy<Value = &'static str> {
    prop::sample::select(from)
}

/// Words, then tokens, each separated by some whitespace.
fn words_then_tokens() -> impl Strategy<Value = String> {
    (
        prop::collection::vec((pick(WORDS), pick(SPACE)), 0..4),
        prop::collection::vec((pick(SPACE), pick(TOKENS)), 0..4),
        pick(TRAILING),
    )
        .prop_map(|(words, tokens, trailing)| {
            let mut s: String = words.iter().map(|(w, sp)| format!("{w}{sp}")).collect();
            s = s.trim_end().to_owned();
            for (sp, t) in tokens {
                s.push_str(sp);
                s.push_str(t);
            }
            s + trailing
        })
}

/// A line from the grammar's main productions.
fn line() -> impl Strategy<Value = String> {
    let indent = (0..4usize).prop_map(|n| "  ".repeat(n));
    prop_oneof![
        (1..=7usize, words_then_tokens()).prop_map(|(n, rest)| format!("{} {rest}", "#".repeat(n))),
        (
            indent.clone(),
            pick(&["- ", "+ ", "1. ", "12. "]),
            pick(&["", "[ ] ", "[x] ", "[-] ", "[x]"]),
            words_then_tokens()
        )
            .prop_map(|(i, m, t, rest)| format!("{i}{m}{t}{rest}")),
        (indent.clone(), words_then_tokens()).prop_map(|(i, rest)| format!("{i}{rest}")),
        (indent.clone(), words_then_tokens(), words_then_tokens())
            .prop_map(|(i, a, b)| format!("{i}| {a} | {b} |")),
        (
            indent,
            pick(&["```", "```csv", "````", "$$"]),
            pick(TRAILING)
        )
            .prop_map(|(i, f, t)| format!("{i}{f}{t}")),
        pick(&["|---|---|", "  ", ""]).prop_map(str::to_owned),
    ]
}

fn note() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(line(), 0..20),
        pick(&["\n", "\r\n"]),
        pick(&["", "\n", "\n\n", "\n  \n"]),
        any::<bool>(),
    )
        .prop_map(|(lines, eol, tail, bom)| {
            let bom = if bom { "\u{feff}" } else { "" };
            format!("{bom}{}{tail}", lines.join(eol))
        })
}

/// The syntax soup the parser's own property tests use.
const FRAGMENTS: &[&str] = &[
    "\n", "\r\n", "\r", " ", "  ", "\t", "#", "# ", "## ", "- ", "+ ", "1. ", "[ ] ", "[x] ",
    "[-]", "|", "|---|", "\\", "\\|", "\\]", "\\,", "`", "```", "````", "$", "$$", "*", "_", "@",
    "@a", "@id:", "@k:[", "]", ",", "[[", "]]", "[[a|b]]", "#x", "..", "/", "a", "Z", "9", ".",
    "é", "e\u{301}", "日本", "\u{feff}", "Σ",
];

fn soup() -> impl Strategy<Value = String> {
    prop::collection::vec(pick(FRAGMENTS), 0..60).prop_map(|v| v.concat())
}

/// Everything a note means, without byte offsets, which formatting moves.
/// Blank lines at the end, which formatting drops, don't count.
fn meaning(src: &str) -> String {
    let doc = parse(src);
    let text = |span: &std::ops::Range<usize>| &src[span.clone()];
    let end = doc
        .lines
        .iter()
        .rposition(|l| l.kind != LineKind::Blank)
        .map_or(0, |i| i + 1);
    let mut out = Vec::new();

    for (i, l) in doc.lines[..end].iter().enumerate() {
        let words = if l.kind == LineKind::Verbatim {
            vec![text(&l.span)]
        } else {
            words(text(&l.span))
        };
        out.push(format!("line {i} {:?} {:?} {words:?}", l.kind, l.owner));
    }
    for s in &doc.scopes {
        let kind = match &s.kind {
            ScopeKind::File => "file".to_owned(),
            ScopeKind::Heading {
                level,
                text: t,
                slug,
            } => {
                format!("h{level} {:?} {slug:?}", words(text(t)))
            }
            ScopeKind::Item(item) => format!(
                "item {} {:?} {:?} {:?}",
                item.depth, item.marker, item.number, item.task
            ),
            ScopeKind::Row { table, cells } => {
                format!(
                    "row {table} {:?}",
                    cells.iter().map(text).collect::<Vec<_>>()
                )
            }
        };
        let extent = s.extent.start..s.extent.end.min(end.max(s.extent.start));
        out.push(format!(
            "scope {kind} {:?} {:?} {extent:?} {:?} {:?}",
            s.line, s.parent, s.metadata, s.id
        ));
    }
    for t in &doc.tokens {
        out.push(format!(
            "token {} {:?} {:?} {} {:?}",
            t.key, t.form, t.values, t.line, t.scope
        ));
    }
    for l in &doc.links {
        out.push(format!(
            "link {:?} {:?} {} {:?}",
            l.target, l.label, l.line, l.scope
        ));
    }
    for i in &doc.inlines {
        out.push(format!(
            "inline {:?} {:?} {}",
            i.kind,
            text(&i.content),
            i.line
        ));
    }
    for b in &doc.blocks {
        out.push(format!("block {b:?}"));
    }
    for t in &doc.tables {
        out.push(format!("table {t:?}"));
    }
    for d in &doc.diagnostics {
        out.push(format!(
            "diagnostic {:?} {:?}",
            d.code,
            doc.line_at(d.span.start)
        ));
    }
    out.join("\n")
}

fn words(s: &str) -> Vec<&str> {
    s.split([' ', '\t']).filter(|w| !w.is_empty()).collect()
}

fn check(src: &str, width: usize) {
    let opts = FmtOptions { width };
    let doc: Document = parse(src);
    let edits = fmt(src, &doc, &opts);
    for pair in edits.windows(2) {
        assert!(
            pair[0].range.end <= pair[1].range.start,
            "edits overlap: {edits:?}"
        );
    }
    let once = apply(src, &edits);
    assert_eq!(meaning(&once), meaning(src), "formatted: {once:?}");
    let twice = format(&once, &opts);
    assert_eq!(twice, once, "not idempotent");
    assert!(fmt(&once, &parse(&once), &opts).is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn grammar(src in note(), width in 0..100usize) {
        check(&src, width);
    }

    #[test]
    fn fragments(src in soup(), width in 0..100usize) {
        check(&src, width);
    }
}
