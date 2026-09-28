//! `snot`: format, check and serve Simple Note Format notes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, io};

use clap::{Parser, Subcommand};
use snot_workspace::{Config, find_root};

mod check;
mod fmt;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Format notes: align the metadata ending headings and list items, trim
    /// trailing whitespace, and end each file with one newline.
    Fmt(fmt::Args),
    /// Report problems in notes: syntax errors, and links that don't resolve.
    Check(check::Args),
    /// Run the language server on stdin and stdout.
    Lsp,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Fmt(args) => fmt::run(args),
        Command::Check(args) => check::run(args),
        Command::Lsp => match snot_lsp::stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("snot lsp: {e}");
                ExitCode::from(2)
            }
        },
    }
}

/// The notes root: `explicit`, else the nearest directory up from `start`
/// (or the working directory) with a `.snot.toml`, else the working
/// directory. Made absolute, so note paths can be compared with it.
fn root(explicit: Option<&Path>, start: Option<&Path>) -> io::Result<PathBuf> {
    let cwd = env::current_dir()?;
    let root = match explicit {
        Some(root) => root.to_path_buf(),
        None => find_root(&cwd.join(start.unwrap_or(&cwd))).unwrap_or(cwd),
    };
    root.canonicalize()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", root.display())))
}

/// The configuration of the notes root `start` is in, or the default when
/// there is no `.snot.toml` above it.
fn config(start: Option<&PathBuf>) -> Result<Config, String> {
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    match find_root(&cwd.join(start.unwrap_or(&cwd))) {
        Some(root) => Config::load(&root).map_err(|e| e.to_string()),
        None => Ok(Config::default()),
    }
}
