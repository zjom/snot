//! The parts of the API the conformance corpus doesn't show.

use std::fs;
use std::path::Path;

use snot_syntax::{ScopeId, is_key, parse, slug};

#[test]
fn resolves_anchors_by_id_then_slug() {
    let doc = parse("# Risks\n# Other @id:risks\n## Café Über\n");
    assert_eq!(doc.resolve_anchor("risks"), Some(ScopeId(2)));
    assert_eq!(doc.resolve_anchor("café-über"), Some(ScopeId(3)));
    // A decomposed é in the link still finds the heading.
    assert_eq!(doc.resolve_anchor("cafe\u{301}-über"), Some(ScopeId(3)));
    assert_eq!(doc.resolve_anchor("missing"), None);
}

#[test]
fn finds_the_line_at_an_offset() {
    let doc = parse("ab\ncd\n");
    assert_eq!(doc.line_at(0), Some(0));
    assert_eq!(doc.line_at(2), Some(0));
    assert_eq!(doc.line_at(3), Some(1));
    assert_eq!(doc.line_at(99), Some(1));
    assert_eq!(parse("").line_at(0), None);
}

#[test]
fn checks_keys() {
    assert!(is_key("due"));
    assert!(is_key("Due"));
    assert!(is_key("a-b_C9"));
    assert!(!is_key(""));
    assert!(!is_key("9a"));
    assert!(!is_key("-a"));
    assert!(!is_key("_a"));
    assert!(!is_key("café"));
}

#[test]
fn slugs_drop_tokens() {
    assert_eq!(slug("Open Risks (Q4) @status:open"), "open-risks-q4");
    assert_eq!(slug("@only:tokens"), "");
}

/// Every example note in NOTE_SPEC.md (a fenced block with no language,
/// other than the grammar) must be in the corpus as a `spec-*.snot` file.
#[test]
fn corpus_has_every_spec_example() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let spec = fs::read_to_string(root.join("NOTE_SPEC.md")).unwrap();
    let mut examples = Vec::new();
    let mut lines = spec.lines();
    while let Some(line) = lines.next() {
        let ticks = line.bytes().take_while(|&b| b == b'`').count();
        if ticks < 3 {
            continue;
        }
        let fence = &line[..ticks];
        let body: Vec<&str> = lines.by_ref().take_while(|l| *l != fence).collect();
        if line.len() == ticks && !body.first().is_some_and(|l| l.starts_with("file ")) {
            examples.push(body.join("\n") + "\n");
        }
    }
    assert!(
        examples.len() >= 8,
        "found only {} examples",
        examples.len()
    );

    let corpus: Vec<String> = fs::read_dir(root.join("conformance"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("spec-") && name.ends_with(".snot")
        })
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    for example in examples {
        assert!(corpus.contains(&example), "not in the corpus:\n{example}");
    }
}
