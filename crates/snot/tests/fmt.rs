//! `snot fmt` from the command line.

mod common;

use std::fs;

use common::{snot, stdout, write};

#[test]
fn formats_stdin_to_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let out = snot(&["fmt", "--width", "10", "-"], dir.path(), "# A @b  \n\n");
    assert!(out.status.success());
    assert_eq!(stdout(&out), "# A     @b\n");
}

#[test]
fn checks_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let out = snot(&["fmt", "--check", "-"], dir.path(), "a \n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "<stdin>\n");
    let out = snot(&["fmt", "--check", "-"], dir.path(), "a\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
}

#[test]
fn formats_the_notes_in_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir_all(root.join(".hidden")).unwrap();
    for path in ["a.snot", "sub/b.snot", ".hidden/c.snot", "d.txt", "ok.snot"] {
        let src = if path == "ok.snot" { "ok\n" } else { "x  " };
        fs::write(root.join(path), src).unwrap();
    }

    let out = snot(&["fmt", "--check"], root, "");
    assert_eq!(out.status.code(), Some(1));
    let listed = stdout(&out).replace('\\', "/");
    assert_eq!(listed, "./a.snot\n./sub/b.snot\n");
    assert_eq!(fs::read_to_string(root.join("a.snot")).unwrap(), "x  ");

    let out = snot(&["fmt"], root, "");
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
    assert_eq!(fs::read_to_string(root.join("a.snot")).unwrap(), "x\n");
    assert_eq!(fs::read_to_string(root.join("sub/b.snot")).unwrap(), "x\n");
    assert_eq!(
        fs::read_to_string(root.join(".hidden/c.snot")).unwrap(),
        "x  "
    );
    assert_eq!(fs::read_to_string(root.join("d.txt")).unwrap(), "x  ");

    // A file named explicitly is formatted whatever its extension.
    let out = snot(&["fmt", "d.txt"], root, "");
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(root.join("d.txt")).unwrap(), "x\n");
}

#[test]
fn reports_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let out = snot(&["fmt", "missing.snot"], dir.path(), "");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing.snot"));
}

#[test]
fn reads_width_and_extension_from_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            (".snot.toml", "extension = \".note\"\nwidth = 10\n"),
            ("sub/a.note", "# A @b\n"),
            ("sub/b.snot", "# A @b\n"),
        ],
    );
    let out = snot(&["fmt", "sub"], root, "");
    assert!(out.status.success());
    assert_eq!(
        fs::read_to_string(root.join("sub/a.note")).unwrap(),
        "# A     @b\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("sub/b.snot")).unwrap(),
        "# A @b\n"
    );

    // `--width` wins; stdin takes the config from the working directory.
    let out = snot(&["fmt", "--width", "8", "-"], &root.join("sub"), "# A @b");
    assert_eq!(stdout(&out), "# A   @b\n");
    let out = snot(&["fmt", "-"], &root.join("sub"), "# A @b");
    assert_eq!(stdout(&out), "# A     @b\n");
}
