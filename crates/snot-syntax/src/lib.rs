//! Parser for Simple Note Format: lines, scopes, metadata tokens and links.
//!
//! [`parse`] reads a note exactly as `NOTE_SPEC.md` describes it into a
//! [`Document`]: every line classified and assigned an owning scope, the
//! metadata of each scope, links, inline code, math and emphasis, code and
//! math blocks, tables, and any problems found. Parsing never fails; problems
//! are reported as [`Diagnostic`]s and read the way the spec says to recover.

mod diagnostic;
mod document;
mod inline;
mod line_index;
mod lines;
mod link;
mod slug;
mod structure;

pub use diagnostic::{Code, Diagnostic, Severity};
pub use document::*;
pub use line_index::{Encoding, LineIndex, Position};
pub use slug::slug;

/// Parse a note.
pub fn parse(source: &str) -> Document {
    structure::build(source)
}

/// True if `s` is a valid metadata key (NOTE_SPEC 6.2): an ASCII letter, then
/// ASCII letters, digits, `-` or `_`. Keys are case-sensitive.
pub fn is_key(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic()) && bytes.all(is_key_byte)
}

/// A byte that can follow the first letter of a key.
pub(crate) fn is_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}
