//! Loading and indexing a workspace.

use std::fs;
use std::path::Path;
use std::time::Instant;

use snot_workspace::{Config, LinkRef, TokenRef, Workspace, find_root};

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

fn load(root: &Path) -> Workspace {
    let (ws, errors) = Workspace::load(root.to_path_buf(), Config::load(root).unwrap()).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    ws
}

#[test]
fn reads_the_config() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(Config::load(dir.path()).unwrap(), Config::default());
    write(dir.path(), &[(".snot.toml", "extension = \"note\"\n")]);
    let config = Config::load(dir.path()).unwrap();
    assert_eq!(config.extension, ".note");
    assert_eq!(config.width, 79);
    write(dir.path(), &[(".snot.toml", "widht = 80\n")]);
    let e = Config::load(dir.path()).unwrap_err().to_string();
    assert!(e.contains("widht"), "{e}");
}

#[test]
fn finds_the_root_above() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, &[(".snot.toml", ""), ("a/b/c.snot", "")]);
    assert_eq!(find_root(&root.join("a/b/c.snot")).as_deref(), Some(root));
    assert_eq!(find_root(&root.join("a/b")).as_deref(), Some(root));
    assert_eq!(find_root(root).as_deref(), Some(root));
}

#[test]
fn indexes_notes_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("a.snot", "# A\n"),
            ("projects/atlas.snot", "# Atlas\n"),
            (".hidden/x.snot", ""),
            ("ignored/y.snot", ""),
            (".ignore", "ignored/\n"),
            ("img/a.png", ""),
            ("notes.txt", ""),
        ],
    );
    let ws = load(root);
    let names: Vec<_> = ws.notes().map(|(name, _)| name).collect();
    assert_eq!(names, ["a", "projects/atlas"]);
    assert_eq!(
        ws.note("projects/atlas").unwrap().path,
        root.join("projects/atlas.snot")
    );
    assert_eq!(
        ws.path_of("projects/atlas"),
        root.join("projects/atlas.snot")
    );
    assert_eq!(ws.name_of(&root.join("a.txt")), None);
    assert!(ws.file_exists("img/a.png"));
    assert!(!ws.file_exists("img/b.png"));
}

#[test]
fn uses_the_configured_extension() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            (".snot.toml", "extension = \".md\""),
            ("a.md", ""),
            ("b.snot", ""),
        ],
    );
    let names: Vec<_> = load(root).notes().map(|(n, _)| n.to_owned()).collect();
    assert_eq!(names, ["a"]);
}

#[test]
fn indexes_backlinks_and_values() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            ("a.snot", "[[b]] [[b#x]] [[c]] @due:2026 @tags:[x, y]\n"),
            ("b.snot", "@due:2026 @urgent\n"),
        ],
    );
    let mut ws = load(root);
    let link = |note: &str, link| LinkRef {
        note: note.into(),
        link,
    };
    let token = |note: &str, token| TokenRef {
        note: note.into(),
        token,
    };
    assert_eq!(ws.backlinks("b"), [link("a", 0), link("a", 1)]);
    assert_eq!(ws.backlinks("c"), [link("a", 2)]);
    assert_eq!(
        ws.values("due").unwrap()["2026"],
        [token("a", 0), token("b", 0)]
    );
    assert_eq!(ws.values("urgent").unwrap()["true"], [token("b", 1)]);
    let tags: Vec<_> = ws.values("tags").unwrap().keys().collect();
    assert_eq!(tags, ["x", "y"]);

    // Replacing a note replaces what it contributed.
    ws.insert("a".into(), root.join("a.snot"), "[[c]] @tags:x\n".into());
    assert_eq!(ws.backlinks("b"), []);
    assert_eq!(ws.backlinks("c"), [link("a", 0)]);
    assert_eq!(ws.values("due").unwrap()["2026"], [token("b", 0)]);
    let tags: Vec<_> = ws.values("tags").unwrap().keys().collect();
    assert_eq!(tags, ["x"]);

    ws.remove("a");
    assert_eq!(ws.backlinks("c"), []);
    assert!(ws.values("tags").is_none());
    let mut keys: Vec<_> = ws.keys().collect();
    keys.sort();
    assert_eq!(keys, ["due", "urgent"]);
}

#[test]
fn reports_unreadable_notes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, &[("a.snot", "ok\n")]);
    fs::write(root.join("b.snot"), b"\xff\xfe").unwrap();
    let (ws, errors) = Workspace::load(root.to_path_buf(), Config::default()).unwrap();
    assert_eq!(ws.notes().count(), 1);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("b.snot"));
}

/// PLAN.md M3: 10k notes index in under a second. Timed only in release
/// builds, as `nix build` tests.
#[test]
fn indexes_ten_thousand_notes_quickly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for i in 0..10_000 {
        let text = format!(
            "# Note {i}                                              @id:n{i} @tags:[a, b]\n\n\
             Some text linking [[d{}/n{}]] and [[d{}/n{}#note-{}]].\n\n\
             - [ ] A task                                            @due:2026-10-01\n\
             - [x] Another *done* with `code`                        @person:bob\n\n\
             ## Details\n\n| a | b |\n|---|---|\n| 1 | 2 @x |\n",
            (i + 1) % 100,
            (i + 1) % 10_000,
            (i + 7) % 100,
            (i + 7) % 10_000,
            (i + 7) % 10_000,
        );
        let sub = root.join(format!("d{}", i % 100));
        if i < 100 {
            fs::create_dir(&sub).unwrap();
        }
        fs::write(sub.join(format!("n{i}.snot")), text).unwrap();
    }

    let start = Instant::now();
    let ws = load(root);
    let elapsed = start.elapsed();
    assert_eq!(ws.notes().count(), 10_000);
    // From n0, and anchored from n9994.
    assert_eq!(ws.backlinks("d1/n1").len(), 2);
    if !cfg!(debug_assertions) {
        assert!(elapsed.as_secs_f64() < 1.0, "indexing took {elapsed:?}");
    }
    eprintln!("indexed 10k notes in {elapsed:?}");
}

#[test]
fn refreshes_from_disk_but_not_over_open_notes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[("a.snot", "[[x]]\n"), ("b.snot", "[[x]]\n"), ("c.snot", "")],
    );
    let mut ws = load(root);
    ws.insert("b".into(), root.join("b.snot"), "open buffer\n".into());

    write(
        root,
        &[
            ("a.snot", "[[x]] [[x]]\n"),
            ("b.snot", "saved\n"),
            ("d.snot", "[[x]]\n"),
        ],
    );
    fs::remove_file(root.join("c.snot")).unwrap();
    let (mut changed, errors) = ws.refresh().unwrap();
    changed.sort();
    assert!(errors.is_empty());
    assert_eq!(changed, ["a", "c", "d"]);
    assert_eq!(ws.note("b").unwrap().source, "open buffer\n");
    assert!(ws.note("c").is_none());
    assert_eq!(ws.backlinks("x").len(), 3);
    assert_eq!(ws.refresh().unwrap().0, Vec::<String>::new());

    // Closing the buffer: back to what's on disk.
    ws.reload("b").unwrap();
    assert_eq!(ws.note("b").unwrap().source, "saved\n");
    assert!(ws.note("b").unwrap().stamp.is_some());
    fs::remove_file(root.join("b.snot")).unwrap();
    ws.reload("b").unwrap();
    assert!(ws.note("b").is_none());
}
