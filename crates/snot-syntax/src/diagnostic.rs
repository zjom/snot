//! Problems found while parsing.

use crate::Span;

/// A problem in a note, at a span of the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// What kind of problem.
    pub code: Code,
    /// Where it is.
    pub span: Span,
    /// A description for people.
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn new(code: Code, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            span,
            message: message.into(),
        }
    }

    /// How serious the problem is.
    pub fn severity(&self) -> Severity {
        self.code.severity()
    }
}

/// How serious a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Not valid snot. Readers recover as the spec says.
    Error,
    /// Valid, but probably not what the writer meant.
    Warning,
}

/// The kinds of problem the parser reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// S001: a tab in leading whitespace (NOTE_SPEC 2.3).
    TabIndent,
    /// S002: a list item with no parent at the depth above (NOTE_SPEC 5.1).
    OrphanItem,
    /// S003: a bare `@id`, or one that isn't a single key-shaped value
    /// (NOTE_SPEC 6.6).
    InvalidId,
    /// S004: an `@id` already used in the file (NOTE_SPEC 6.6).
    DuplicateId,
    /// S005: a code or math block with no closing fence (NOTE_SPEC 9.1).
    UnclosedBlock,
    /// L001: a link path starting with `/`, containing `..` or an empty
    /// segment, or an empty anchor (NOTE_SPEC 7.2).
    InvalidLink,
    /// L002: an anchor on a link to a file that isn't a note (NOTE_SPEC 7.2).
    AnchorOnFile,
}

impl Code {
    /// The code's short name, e.g. `S001`.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::TabIndent => "S001",
            Code::OrphanItem => "S002",
            Code::InvalidId => "S003",
            Code::DuplicateId => "S004",
            Code::UnclosedBlock => "S005",
            Code::InvalidLink => "L001",
            Code::AnchorOnFile => "L002",
        }
    }

    /// How serious the problem is.
    pub fn severity(self) -> Severity {
        match self {
            Code::UnclosedBlock => Severity::Warning,
            _ => Severity::Error,
        }
    }
}
