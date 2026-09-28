//! `snot check`.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs};

use clap::ValueEnum;
use snot_check::{Diagnostic, check, render_human, render_json};
use snot_syntax::Severity;
use snot_workspace::{Config, Workspace};

#[derive(clap::Args)]
pub struct Args {
    /// Notes, or directories of notes, to report on. Defaults to every note
    /// under the root. Links are resolved against the whole root either way.
    paths: Vec<PathBuf>,
    /// The notes root. Defaults to the nearest directory up with a
    /// `.snot.toml`, else the working directory.
    #[arg(long)]
    root: Option<PathBuf>,
    /// How to write the problems.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
    /// Also exit 1 on warnings.
    #[arg(long, value_enum)]
    deny: Option<Deny>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// rustc-style messages with the source.
    Human,
    /// A JSON array, for scripts and CI.
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum Deny {
    Warnings,
}

/// Exit 0 when there are no errors (and, with `--deny warnings`, no
/// warnings), 1 when there are, and 2 when notes couldn't be read.
pub fn run(args: Args) -> ExitCode {
    let mut failed = false;
    let mut fail = |e: &dyn std::fmt::Display| {
        eprintln!("snot check: {e}");
        failed = true;
    };

    let cwd = env::current_dir()
        .and_then(|d| d.canonicalize())
        .unwrap_or_default();
    let root = match crate::root(
        args.root.as_deref(),
        args.paths.first().map(PathBuf::as_path),
    ) {
        Ok(root) => root,
        Err(e) => {
            fail(&e);
            return ExitCode::from(2);
        }
    };
    let mut ws = match Config::load(&root).and_then(|c| Workspace::load(root.clone(), c)) {
        Ok((ws, errors)) => {
            errors.iter().for_each(|e| fail(e));
            ws
        }
        Err(e) => {
            fail(&e);
            return ExitCode::from(2);
        }
    };

    let names = match select(&mut ws, &args.paths) {
        Ok(names) => names,
        Err(e) => {
            fail(&e);
            return ExitCode::from(2);
        }
    };

    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let (mut errors, mut warnings) = (0, 0);
    let mut json = Vec::new();
    for name in &names {
        let note = ws.note(name).expect("selected notes are loaded");
        let diagnostics: Vec<Diagnostic> = check(&ws, name, &note.doc);
        for d in &diagnostics {
            match d.severity() {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
            }
        }
        if diagnostics.is_empty() {
            continue;
        }
        let path = note.path.strip_prefix(&cwd).unwrap_or(&note.path);
        let path = path.display().to_string();
        match args.format {
            Format::Human => print!("{}", render_human(&path, &note.source, &diagnostics, color)),
            Format::Json => {
                for d in render_json(&path, &note.doc, &diagnostics) {
                    json.push(serde_json::to_string(&d).expect("diagnostics serialize"));
                }
            }
        }
    }
    match args.format {
        Format::Human if errors + warnings > 0 => {
            println!("{}, {}", count(errors, "error"), count(warnings, "warning"));
        }
        Format::Human => {}
        Format::Json => println!("[{}]", json.join(",")),
    }

    if failed {
        ExitCode::from(2)
    } else if errors > 0 || (warnings > 0 && args.deny.is_some()) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// The names of the notes `paths` select, in order: all of them when there
/// are no paths. A note file the walk skipped, such as a hidden one, is
/// loaded when named directly.
fn select(ws: &mut Workspace, paths: &[PathBuf]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(ws.notes().map(|(name, _)| name.to_owned()).collect());
    }
    let mut names = Vec::new();
    for path in paths {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !path.starts_with(ws.root()) {
            return Err(format!(
                "{} is not under the notes root {}",
                path.display(),
                ws.root().display()
            ));
        }
        if path.is_dir() {
            names.extend(
                ws.notes()
                    .filter(|(_, note)| note.path.starts_with(&path))
                    .map(|(name, _)| name.to_owned()),
            );
            continue;
        }
        let name = ws.name_of(&path).ok_or_else(|| {
            format!(
                "{} is not a note: its extension isn't {}",
                path.display(),
                ws.config().extension
            )
        })?;
        if ws.note(&name).is_none() {
            let source = read(&path)?;
            ws.insert(name.clone(), path, source);
        }
        names.push(name);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}
