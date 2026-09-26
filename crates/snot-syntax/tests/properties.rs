//! Properties that hold for any input: parsing never panics, and everything
//! the parser returns is consistent with the source.

use proptest::prelude::*;
use snot_syntax::{Eol, LineKind, parse};

/// Fragments dense in syntax, so random notes exercise the parser's edges.
const FRAGMENTS: &[&str] = &[
    "\n", "\r\n", "\r", " ", "  ", "\t", "#", "# ", "## ", "- ", "+ ", "1. ", "[ ] ", "[x] ",
    "[-]", "|", "|---|", "\\", "\\|", "\\]", "\\,", "`", "```", "````", "$", "$$", "*", "_", "@",
    "@a", "@id:", "@k:[", "]", ",", "[[", "]]", "[[a|b]]", "#x", "..", "/", "a", "Z", "9", ".",
    "é", "e\u{301}", "日本", "\u{feff}", "Σ",
];

fn note() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..60).prop_map(|v| v.concat())
}

fn check(src: &str) {
    let doc = parse(src);
    let valid =
        |span: &std::ops::Range<usize>| span.start <= span.end && src.get(span.clone()).is_some();

    let mut prev_end = 0;
    for line in &doc.lines {
        assert!(valid(&line.span), "line span {:?}", line.span);
        assert!(line.span.start >= prev_end);
        prev_end = line.span.end;
        assert_eq!(line.owner.is_none(), line.kind == LineKind::Blank);
        if let Some(owner) = line.owner {
            assert!(owner.0 < doc.scopes.len());
        }
    }
    if let Some(last) = doc.lines.last() {
        let tail = &src[last.span.end..];
        match last.eol {
            Eol::None => assert_eq!(tail, ""),
            Eol::Lf => assert_eq!(tail, "\n"),
            Eol::CrLf => assert_eq!(tail, "\r\n"),
        }
    }

    for (i, scope) in doc.scopes.iter().enumerate() {
        assert!(scope.extent.end <= doc.lines.len());
        if let Some(parent) = scope.parent {
            assert!(parent.0 < i, "a parent comes before its child");
            let p = &doc.scopes[parent.0];
            assert!(p.extent.start <= scope.extent.start && scope.extent.end <= p.extent.end);
        }
    }
    for t in &doc.tokens {
        assert!(valid(&t.span) && valid(&t.key_span));
        assert!(t.value_span.as_ref().is_none_or(valid));
        assert_eq!(&src[t.key_span.clone()], t.key);
        assert!(snot_syntax::is_key(&t.key));
    }
    for l in &doc.links {
        assert!(valid(&l.span) && valid(&l.target_span));
        assert!(l.label_span.as_ref().is_none_or(valid));
    }
    for i in &doc.inlines {
        assert!(valid(&i.span) && valid(&i.content));
    }
    for d in &doc.diagnostics {
        assert!(valid(&d.span));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn fragments(src in note()) {
        check(&src);
    }

    #[test]
    fn any_text(src in any::<String>()) {
        check(&src);
    }
}
