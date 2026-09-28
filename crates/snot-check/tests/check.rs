//! Every diagnostic code, and how they are rendered.

use std::fs;

use snot_check::{Diagnostic, check, render_human, render_json};
use snot_workspace::{Config, Workspace};

fn workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let root = dir.path().to_path_buf();
    let (ws, errors) = Workspace::load(root, Config::default()).unwrap();
    assert!(errors.is_empty());
    (dir, ws)
}

/// The code and flagged text of each problem in note `name`.
fn problems(ws: &Workspace, name: &str) -> Vec<(&'static str, String)> {
    let note = ws.note(name).unwrap();
    check(ws, name, &note.doc)
        .iter()
        .map(|d| (d.code.as_str(), note.source[d.span.clone()].to_owned()))
        .collect()
}

fn codes(src: &str) -> Vec<(&'static str, String)> {
    let (_dir, ws) = workspace(&[("a.snot", src)]);
    problems(&ws, "a")
}

fn one(code: &'static str, text: &str) -> Vec<(&'static str, String)> {
    vec![(code, text.to_owned())]
}

#[test]
fn s001_tab_in_indentation() {
    assert_eq!(codes("- a\n\t- b\n"), one("S001", "\t"));
}

#[test]
fn s002_orphan_item() {
    assert_eq!(codes("    - a\n"), one("S002", "-"));
}

#[test]
fn s003_invalid_id() {
    assert_eq!(codes("# A @id:[a, b]\n"), one("S003", "@id:[a, b]"));
    assert_eq!(codes("# A @id\n"), one("S003", "@id"));
}

#[test]
fn s004_duplicate_id() {
    assert_eq!(codes("# A @id:x\n# B @id:x\n"), one("S004", "@id:x"));
}

#[test]
fn s005_unclosed_block() {
    assert_eq!(codes("```\ncode\n"), one("S005", "```"));
}

#[test]
fn l001_invalid_link() {
    assert_eq!(codes("[[/a]] [[../a]] [[a//b]] [[a#]] [[#]]\n").len(), 5);
    assert_eq!(codes("[[../a]]\n"), one("L001", "../a"));
}

#[test]
fn l002_anchor_on_file() {
    assert_eq!(codes("[[img/a.png#x]]\n"), one("L002", "img/a.png#x"));
}

#[test]
fn l003_missing_note() {
    let (_dir, ws) = workspace(&[("a.snot", "[[b]] [[c]] [[c#x]] [[d/e]]\n"), ("b.snot", "")]);
    assert_eq!(
        problems(&ws, "a"),
        [
            ("L003", "c".into()),
            ("L003", "c#x".into()),
            ("L003", "d/e".into())
        ]
    );
}

#[test]
fn l004_missing_anchor() {
    let (_dir, ws) = workspace(&[
        (
            "a.snot",
            "# Own\n[[#own]] [[#nope]] [[b#café-über]] [[b#x]] [[b#y]] [[a#own]] [[a#z]]\n",
        ),
        ("b.snot", "# Café Über\n## Other @id:x\n"),
    ]);
    assert_eq!(
        problems(&ws, "a"),
        [
            ("L004", "#nope".into()),
            ("L004", "b#y".into()),
            ("L004", "a#z".into())
        ]
    );
}

#[test]
fn l004_uses_the_given_document() {
    // An editor's buffer, not yet saved, defines the anchor.
    let (_dir, ws) = workspace(&[("a.snot", "[[#new]]\n")]);
    let doc = snot_syntax::parse("# New\n[[#new]]\n");
    assert_eq!(check(&ws, "a", &doc), []);
}

#[test]
fn l005_missing_file() {
    let (_dir, ws) = workspace(&[
        ("a.snot", "[[img/a.png]] [[img/b.png]] [[https://x.y]]\n"),
        ("img/a.png", ""),
    ]);
    assert_eq!(problems(&ws, "a"), one("L005", "img/b.png"));
}

#[test]
fn a_clean_note_has_no_problems() {
    let (_dir, ws) = workspace(&[
        ("a.snot", "# A @id:a\n\n- [[b#c]] [[#a]]\n"),
        ("b.snot", "# C\n"),
    ]);
    assert_eq!(problems(&ws, "a"), []);
}

fn sample() -> (tempfile::TempDir, Workspace, Vec<Diagnostic>) {
    let (dir, ws) = workspace(&[("a.snot", "# A\n\n[[b]] and\n- w\n\t- x\n")]);
    let doc = &ws.note("a").unwrap().doc;
    let diagnostics = check(&ws, "a", doc);
    (dir, ws, diagnostics)
}

#[test]
fn renders_for_people() {
    let (_dir, ws, diagnostics) = sample();
    let source = &ws.note("a").unwrap().source;
    let out = render_human("a.snot", source, &diagnostics, false);
    assert!(
        out.contains("warning[L003]: no note `b`: b.snot doesn't exist"),
        "{out}"
    );
    assert!(out.contains("--> a.snot:3:3"), "{out}");
    assert!(out.contains("error[S001]"), "{out}");
    assert!(out.contains("--> a.snot:5:1"), "{out}");
}

#[test]
fn renders_json() {
    let (_dir, ws, diagnostics) = sample();
    let json = serde_json::to_value(render_json(
        "a.snot",
        &ws.note("a").unwrap().doc,
        &diagnostics,
    ))
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {
                "path": "a.snot", "code": "L003", "severity": "warning",
                "message": "no note `b`: b.snot doesn't exist",
                "line": 3, "column": 3, "end_line": 3, "end_column": 4,
                "span": { "start": 7, "end": 8 },
            },
            {
                "path": "a.snot", "code": "S001", "severity": "error",
                "message": "tab in indentation; indent with spaces",
                "line": 5, "column": 1, "end_line": 5, "end_column": 2,
                "span": { "start": 19, "end": 20 },
            },
        ])
    );
}
