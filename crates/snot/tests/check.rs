//! `snot check` from the command line.

mod common;

use common::{snot, stderr, stdout, write};

/// A workspace with one note with a warning, and one with an error.
fn notes() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (".snot.toml", ""),
            ("a.snot", "# A\n\n[[b#a]] [[c]]\n"),
            ("sub/b.snot", "# B\n[[a#a]] [[sub/b]]\n"),
            ("sub/err.snot", "# E @id:[x, y]\n"),
            ("clean/ok.snot", "[[a]]\n"),
        ],
    );
    dir
}

#[test]
fn reports_every_note_under_the_root() {
    let dir = notes();
    // From a subdirectory: the root is found above it.
    let out = snot(&["check"], &dir.path().join("sub"), "");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("warning[L003]: no note `b`"), "{text}");
    assert!(text.contains("warning[L003]: no note `c`"), "{text}");
    assert!(text.contains("error[S003]"), "{text}");
    assert!(text.contains("--> err.snot:1:5"), "{text}");
    assert!(text.ends_with("1 error, 2 warnings\n"), "{text}");
}

#[test]
fn reports_only_the_paths_given() {
    let dir = notes();
    let out = snot(&["check", "a.snot"], dir.path(), "");
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).ends_with("0 errors, 2 warnings\n"));

    let out = snot(&["check", "--deny", "warnings", "a.snot"], dir.path(), "");
    assert_eq!(out.status.code(), Some(1));

    let out = snot(&["check", "clean"], dir.path(), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
}

#[test]
fn writes_json() {
    let dir = notes();
    let out = snot(&["check", "--format", "json", "a.snot"], dir.path(), "");
    let json: serde_json::Value = serde_json::from_str(stdout(&out)).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {
                "path": "a.snot", "code": "L003", "severity": "warning",
                "message": "no note `b`: b.snot doesn't exist",
                "line": 3, "column": 3, "end_line": 3, "end_column": 6,
                "span": { "start": 7, "end": 10 },
            },
            {
                "path": "a.snot", "code": "L003", "severity": "warning",
                "message": "no note `c`: c.snot doesn't exist",
                "line": 3, "column": 11, "end_line": 3, "end_column": 12,
                "span": { "start": 15, "end": 16 },
            },
        ])
    );
    let out = snot(&["check", "--format", "json", "clean"], dir.path(), "");
    assert_eq!(stdout(&out), "[]\n");
}

#[test]
fn takes_the_root_as_an_option() {
    let dir = notes();
    // With `sub` as the root, `[[a#a]]` is missing and `[[sub/b]]` too.
    let out = snot(&["check", "--root", "sub", "sub/b.snot"], dir.path(), "");
    let text = stdout(&out);
    assert!(text.contains("no note `a`"), "{text}");
    assert!(text.contains("no note `sub/b`"), "{text}");
}

#[test]
fn rejects_paths_outside_the_root_and_non_notes() {
    let dir = notes();
    write(dir.path(), &[("x.txt", "")]);
    let out = snot(&["check", "--root", "sub", "a.snot"], dir.path(), "");
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("is not under the notes root"));
    let out = snot(&["check", "x.txt"], dir.path(), "");
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("is not a note"));
    let out = snot(&["check", "missing.snot"], dir.path(), "");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn checks_a_hidden_note_named_directly() {
    let dir = notes();
    write(dir.path(), &[(".drafts/d.snot", "[[nope]]\n")]);
    let out = snot(&["check", ".drafts/d.snot"], dir.path(), "");
    assert!(stdout(&out).contains("no note `nope`"), "{}", stdout(&out));
}
