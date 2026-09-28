//! Running the `snot` binary.

#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Run `snot` with `args` in `dir`, feeding it `stdin`.
pub fn snot(args: &[&str], dir: &Path, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_snot"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

pub fn stdout(out: &Output) -> &str {
    std::str::from_utf8(&out.stdout).unwrap()
}

pub fn stderr(out: &Output) -> &str {
    std::str::from_utf8(&out.stderr).unwrap()
}

/// Write `files` under `root`, making directories as needed.
pub fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
