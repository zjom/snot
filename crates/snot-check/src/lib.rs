//! Syntax and workspace diagnostics for Simple Note Format.
//!
//! [`check`] reports a note's problems: the parser's (S001 to S005, L001 and
//! L002), and links that don't resolve in the workspace (L003 to L005).
//! [`render_human`] and [`render_json`] format them for people and scripts.

use std::fmt::Write;
use std::ops::Range;

use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};
use serde::Serialize;
use snot_syntax::{Document, Severity, Span, Target};
use snot_workspace::Workspace;

/// The kinds of problem `check` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// One the parser reports.
    Syntax(snot_syntax::Code),
    /// L003: a note link whose note doesn't exist.
    MissingNote,
    /// L004: an anchor matching no `@id` or heading slug (NOTE_SPEC 7.3).
    MissingAnchor,
    /// L005: a file link whose file doesn't exist.
    MissingFile,
}

impl Code {
    /// The code's short name, e.g. `L003`.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Syntax(code) => code.as_str(),
            Code::MissingNote => "L003",
            Code::MissingAnchor => "L004",
            Code::MissingFile => "L005",
        }
    }

    /// How serious the problem is.
    pub fn severity(self) -> Severity {
        match self {
            Code::Syntax(code) => code.severity(),
            _ => Severity::Warning,
        }
    }
}

/// A problem in a note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// What kind of problem.
    pub code: Code,
    /// Where it is, in bytes.
    pub span: Span,
    /// A description for people.
    pub message: String,
}

impl Diagnostic {
    /// How serious the problem is.
    pub fn severity(&self) -> Severity {
        self.code.severity()
    }
}

/// Every problem in `doc`, the note named `name` in `ws`, ordered by
/// position. `doc` may differ from the note `ws` holds, as for an editor's
/// unsaved buffer; its own anchors are resolved against `doc`.
pub fn check(ws: &Workspace, name: &str, doc: &Document) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = doc
        .diagnostics
        .iter()
        .map(|d| Diagnostic {
            code: Code::Syntax(d.code),
            span: d.span.clone(),
            message: d.message.clone(),
        })
        .collect();
    out.extend(links(ws, name, doc));
    out.sort_by_key(|d| d.span.start);
    out
}

/// L003 to L005: links in `doc` that don't resolve. Links the parser already
/// reports as invalid are skipped.
pub fn links(ws: &Workspace, name: &str, doc: &Document) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for link in &doc.links {
        if doc.diagnostics.iter().any(|d| d.span == link.target_span) {
            continue;
        }
        let problem = match &link.target {
            Target::Url(_) => None,
            Target::Anchor(anchor) => doc
                .resolve_anchor(anchor)
                .is_none()
                .then(|| missing_anchor(anchor, "this note")),
            Target::Note { path, anchor } => {
                let target = if path == name {
                    Some(doc)
                } else {
                    ws.note(path).map(|n| &n.doc)
                };
                match (target, anchor) {
                    (None, _) => Some((
                        Code::MissingNote,
                        format!("no note `{path}`: {} doesn't exist", display(ws, path)),
                    )),
                    (Some(target), Some(anchor)) if target.resolve_anchor(anchor).is_none() => {
                        Some(missing_anchor(anchor, &format!("`{path}`")))
                    }
                    _ => None,
                }
            }
            Target::File { path, .. } => {
                (!ws.file_exists(path)).then(|| (Code::MissingFile, format!("no file `{path}`")))
            }
        };
        if let Some((code, message)) = problem {
            out.push(Diagnostic {
                code,
                span: link.target_span.clone(),
                message,
            });
        }
    }
    out
}

fn missing_anchor(anchor: &str, place: &str) -> (Code, String) {
    (
        Code::MissingAnchor,
        format!("no `@id:{anchor}` or heading with slug `{anchor}` in {place}"),
    )
}

/// A note's file, relative to the root.
fn display(ws: &Workspace, name: &str) -> String {
    format!("{name}{}", ws.config().extension)
}

/// Diagnostics in rustc's style, for one file. `path` is how to show the
/// file; `color` uses ANSI colours.
pub fn render_human(path: &str, source: &str, diagnostics: &[Diagnostic], color: bool) -> String {
    let renderer = if color {
        Renderer::styled()
    } else {
        Renderer::plain()
    };
    let mut out = String::new();
    for d in diagnostics {
        let level = match d.severity() {
            Severity::Error => Level::ERROR,
            Severity::Warning => Level::WARNING,
        };
        let report = [level.primary_title(&d.message).id(d.code.as_str()).element(
            Snippet::source(source)
                .path(path)
                .annotation(AnnotationKind::Primary.span(d.span.clone())),
        )];
        writeln!(out, "{}\n", renderer.render(&report)).unwrap();
    }
    out
}

/// A diagnostic as `snot check --format json` writes it. Lines and columns
/// are 1-based; columns count bytes from the start of the line.
#[derive(Serialize)]
pub struct JsonDiagnostic<'a> {
    /// The file, as given to [`render_json`].
    pub path: &'a str,
    /// The code, e.g. `L003`.
    pub code: &'static str,
    /// `error` or `warning`.
    pub severity: &'static str,
    /// A description for people.
    pub message: &'a str,
    /// The start's line.
    pub line: usize,
    /// The start's column.
    pub column: usize,
    /// The end's line.
    pub end_line: usize,
    /// The end's column: one past the last byte.
    pub end_column: usize,
    /// The byte offsets of the span in the file.
    pub span: Range<usize>,
}

/// Diagnostics for one file as [`JsonDiagnostic`]s. `doc` is the parse of
/// the file, for line numbers.
pub fn render_json<'a>(
    path: &'a str,
    doc: &Document,
    diagnostics: &'a [Diagnostic],
) -> Vec<JsonDiagnostic<'a>> {
    let position = |offset: usize| match doc.line_at(offset) {
        Some(line) => (
            line + 1,
            offset.saturating_sub(doc.lines[line].span.start) + 1,
        ),
        None => (1, 1),
    };
    diagnostics
        .iter()
        .map(|d| {
            let (line, column) = position(d.span.start);
            let (end_line, end_column) = position(d.span.end);
            JsonDiagnostic {
                path,
                code: d.code.as_str(),
                severity: match d.severity() {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                },
                message: &d.message,
                line,
                column,
                end_line,
                end_column,
                span: d.span.clone(),
            }
        })
        .collect()
}
