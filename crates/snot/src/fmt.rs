//! `snot fmt`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use snot_fmt::{FmtOptions, format};

#[derive(clap::Args)]
pub struct Args {
    /// Notes, or directories to find notes in (hidden and ignored ones
    /// skipped). `-` reads stdin and writes stdout.
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    /// Change nothing; list the files that would change, and exit 1 if any.
    #[arg(long)]
    check: bool,
    /// The column trailing metadata ends at. Defaults to `width` in
    /// `.snot.toml`, else 79.
    #[arg(long)]
    width: Option<usize>,
}

/// Exit 0 when done (or, with `--check`, nothing would change), 1 when
/// `--check` finds a file to change, 2 on errors.
pub fn run(args: Args) -> ExitCode {
    let start = args.paths.iter().find(|p| p.as_os_str() != "-");
    let config = match crate::config(start) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("snot fmt: {e}");
            return ExitCode::from(2);
        }
    };
    let opts = FmtOptions {
        width: args.width.unwrap_or(config.width),
    };
    let mut failed = false;
    let mut unformatted = false;
    let mut files = Vec::new();
    for path in &args.paths {
        if path.as_os_str() == "-" {
            match stdin(&opts, args.check) {
                Ok(changed) => unformatted |= changed && args.check,
                Err(e) => failed |= report("<stdin>", e),
            }
        } else if path.is_dir() {
            match snot_workspace::walk(path, &config.extension) {
                Ok(found) => files.extend(found),
                Err(e) => {
                    eprintln!("snot fmt: {e}");
                    failed = true;
                }
            }
        } else {
            files.push(path.clone());
        }
    }

    for path in files {
        match file(&path, &opts, args.check) {
            Ok(true) if args.check => {
                unformatted = true;
                println!("{}", path.display());
            }
            Ok(_) => {}
            Err(e) => failed |= report(path.display(), e),
        }
    }

    if failed {
        ExitCode::from(2)
    } else if unformatted {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn report(what: impl std::fmt::Display, e: io::Error) -> bool {
    eprintln!("snot fmt: {what}: {e}");
    true
}

/// Format stdin to stdout, or with `check`, only say whether it would change.
fn stdin(opts: &FmtOptions, check: bool) -> io::Result<bool> {
    let mut src = String::new();
    io::stdin().read_to_string(&mut src)?;
    let out = format(&src, opts);
    if check {
        if out != src {
            println!("<stdin>");
        }
    } else {
        io::stdout().write_all(out.as_bytes())?;
    }
    Ok(out != src)
}

/// Format a file in place, or with `check`, only say whether it would change.
fn file(path: &Path, opts: &FmtOptions, check: bool) -> io::Result<bool> {
    let src = fs::read_to_string(path)?;
    let out = format(&src, opts);
    let changed = out != src;
    if changed && !check {
        fs::write(path, out)?;
    }
    Ok(changed)
}
