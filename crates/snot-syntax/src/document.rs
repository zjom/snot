//! The parsed form of a note.

use std::ops::Range;

use indexmap::IndexMap;
use unicode_normalization::UnicodeNormalization;

use crate::Diagnostic;

/// A byte range in the parsed source.
pub type Span = Range<usize>;

/// A scope's metadata: every key it carries, in reading order, with its values
/// concatenated in reading order (NOTE_SPEC 6.4).
pub type Metadata = IndexMap<String, Vec<String>>;

/// Index of a scope in [`Document::scopes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub usize);

impl ScopeId {
    /// The file scope, which every document has.
    pub const FILE: ScopeId = ScopeId(0);
}

/// A parsed note. Spans are byte offsets into the source given to
/// [`parse`](crate::parse); line numbers are 0-based indices into `lines`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// Every line, in order.
    pub lines: Vec<Line>,
    /// Every scope, in order of their first line. The file is first.
    pub scopes: Vec<Scope>,
    /// Metadata tokens, in reading order.
    pub tokens: Vec<Token>,
    /// Links, in reading order.
    pub links: Vec<Link>,
    /// Inline code, math and emphasis, ordered by start.
    pub inlines: Vec<Inline>,
    /// Code and math blocks, in order.
    pub blocks: Vec<Block>,
    /// Tables, in order.
    pub tables: Vec<Table>,
    /// Problems found while parsing, ordered by position.
    pub diagnostics: Vec<Diagnostic>,
}

impl Document {
    /// The scope with the given id.
    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0]
    }

    /// The line containing byte `offset`, or the last line when it is past the
    /// end. A line's end of line counts as part of it.
    pub fn line_at(&self, offset: usize) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        Some(
            self.lines
                .partition_point(|l| l.span.start <= offset)
                .saturating_sub(1),
        )
    }

    /// The scope an anchor names (NOTE_SPEC 7.3): the scope carrying
    /// `@id:<anchor>`, else the first heading whose slug equals the anchor.
    pub fn resolve_anchor(&self, anchor: &str) -> Option<ScopeId> {
        let anchor: String = anchor.nfc().collect();
        let by_id = self
            .scopes
            .iter()
            .position(|s| s.id.as_deref() == Some(anchor.as_str()));
        let by_slug = || {
            self.scopes.iter().position(|s| match &s.kind {
                ScopeKind::Heading { slug, .. } => *slug == anchor,
                _ => false,
            })
        };
        by_id.or_else(by_slug).map(ScopeId)
    }
}

/// One line of the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line without its end of line.
    pub span: Span,
    /// How the line ends.
    pub eol: Eol,
    /// Depth from leading whitespace (NOTE_SPEC 2.3), counting a tab as 2
    /// spaces. Unused for verbatim lines.
    pub depth: usize,
    /// What the line is.
    pub kind: LineKind,
    /// The scope owning the line (NOTE_SPEC 3); `None` for blank lines. For
    /// headings, items and rows, the scope the line defines.
    pub owner: Option<ScopeId>,
}

/// How a line ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    /// The last line, with no end of line.
    None,
    /// `\n`
    Lf,
    /// `\r\n`
    CrLf,
}

/// The kind of a line (NOTE_SPEC 2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Only whitespace.
    Blank,
    /// A heading.
    Heading,
    /// A list item.
    Item,
    /// A table row other than a separator.
    Row,
    /// A table separator row (NOTE_SPEC 10.2).
    Separator,
    /// Any other line, including a list item's continuation lines.
    Text,
    /// A code or math block's opening or closing fence.
    Fence,
    /// A line inside a code or math block.
    Verbatim,
}

/// Something metadata attaches to and a link can point at (NOTE_SPEC 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// What kind of scope this is.
    pub kind: ScopeKind,
    /// The line defining the scope; `None` for the file.
    pub line: Option<usize>,
    /// The enclosing scope, for nesting; `None` for the file.
    pub parent: Option<ScopeId>,
    /// The lines the scope covers, including nested scopes: a heading's whole
    /// section, or an item with its continuation lines and child items.
    pub extent: Range<usize>,
    /// The metadata written on the lines the scope owns.
    pub metadata: Metadata,
    /// The scope's `@id` (NOTE_SPEC 6.6), when it has exactly one, with a
    /// value matching the key syntax.
    pub id: Option<String>,
}

/// The kind of a scope, with what is particular to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// The file.
    File,
    /// A heading.
    Heading {
        /// 1 to 6.
        level: u8,
        /// The heading text, after the `#`s and space.
        text: Span,
        /// The heading's slug (NOTE_SPEC 7.3).
        slug: String,
    },
    /// A list item.
    Item(Item),
    /// A table row.
    Row {
        /// Index of the table in [`Document::tables`].
        table: usize,
        /// Each cell's content, trimmed.
        cells: Vec<Span>,
    },
}

/// A list item (NOTE_SPEC 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Depth in the list. An item with no parent at the depth above is treated
    /// as depth 0.
    pub depth: usize,
    /// The marker as written.
    pub marker: Marker,
    /// The marker, without the space after it.
    pub marker_span: Span,
    /// For ordered items, the number: as written, or derived for `+`.
    pub number: Option<u64>,
    /// The task state, if the item is a task.
    pub task: Option<Task>,
    /// The item's inline content, after the marker and any task box.
    pub content: Span,
}

/// A list item marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// `-`: unordered.
    Dash,
    /// `+`: ordered, implicitly numbered.
    Plus,
    /// `N.`: ordered, explicitly numbered. Numbers too large for a `u64`
    /// saturate.
    Number(u64),
}

/// A task's state (NOTE_SPEC 5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// `[ ]`
    Open,
    /// `[x]`
    Done,
    /// `[-]`
    Cancelled,
}

/// A metadata token (NOTE_SPEC 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// The whole token, from the `@`.
    pub span: Span,
    /// The key.
    pub key: String,
    /// The key, without the `@`.
    pub key_span: Span,
    /// How the token is written.
    pub form: TokenForm,
    /// The values: `["true"]` for a flag.
    pub values: Vec<String>,
    /// The value after the `:`, brackets included for a list.
    pub value_span: Option<Span>,
    /// The line the token is on.
    pub line: usize,
    /// The scope the token attaches to.
    pub scope: ScopeId,
}

/// How a token is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenForm {
    /// `@key`
    Flag,
    /// `@key:value`
    Scalar,
    /// `@key:[a, b]`
    List,
}

/// A link (NOTE_SPEC 7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The whole link, `[[` to `]]`.
    pub span: Span,
    /// What the link points at.
    pub target: Target,
    /// The target as written, untrimmed.
    pub target_span: Span,
    /// The label, trimmed and unescaped.
    pub label: Option<String>,
    /// The label as written, untrimmed.
    pub label_span: Option<Span>,
    /// The line the link is on.
    pub line: usize,
    /// The scope owning the link's line.
    pub scope: ScopeId,
}

/// A link target (NOTE_SPEC 7.2). Paths and anchors are unescaped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `[[https://example.com]]`
    Url(String),
    /// `[[#anchor]]`, in this file.
    Anchor(String),
    /// `[[path]]` or `[[path#anchor]]`: a note, relative to the notes root.
    Note {
        /// The path, without the note extension.
        path: String,
        /// The anchor, without the `#`.
        anchor: Option<String>,
    },
    /// `[[img/a.png]]`: a file other than a note. An anchor is an error.
    File {
        /// The path, relative to the notes root.
        path: String,
        /// The anchor, without the `#`.
        anchor: Option<String>,
    },
}

/// Inline code, math or emphasis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inline {
    /// What it is.
    pub kind: InlineKind,
    /// The whole span, delimiters included.
    pub span: Span,
    /// The content between the delimiters. For code, a space is stripped from
    /// each end when the content starts and ends with one.
    pub content: Span,
    /// The line it is on.
    pub line: usize,
}

/// The kind of an [`Inline`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineKind {
    /// `` `code` ``
    Code,
    /// `$math$`
    Math,
    /// `*bold*`
    Bold,
    /// `_underline_`
    Underline,
}

/// A code or math block (NOTE_SPEC 9.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Code or math.
    pub kind: BlockKind,
    /// The opening fence line.
    pub open: usize,
    /// The closing fence line; `None` when the block runs to the end of file.
    pub close: Option<usize>,
    /// Bytes of indentation before the opening fence, stripped (at most) from
    /// each content line.
    pub indent: usize,
}

/// The kind of a [`Block`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// A code block, with its language if one is given.
    Code {
        /// The first word after the opening backticks.
        language: Option<String>,
    },
    /// A math block.
    Math,
}

/// A table (NOTE_SPEC 10): a run of rows at the same indentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// The table's lines, separators included.
    pub lines: Range<usize>,
    /// The header row's line, when the second row is a separator.
    pub header: Option<usize>,
}
