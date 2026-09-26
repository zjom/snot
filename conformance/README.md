# Conformance corpus

Each `NAME.snot` is a note; `NAME.json` beside it is how a conforming reader
reads it. The files are the executable part of [NOTE_SPEC.md](../NOTE_SPEC.md):
any reader (this crate, tree-sitter-snot, snot.nvim) can be tested against them.

- `spec-*.snot` are the spec's own examples, verbatim. A test fails if an
  example in the spec is missing here.
- The rest cover edge cases, one spec area per file, with cases under separate
  headings so they don't interact.
- `.gitattributes` keeps the bytes exact (`crlf.snot` has CRLF line endings,
  `bom.snot` a byte order mark).

## The JSON

Line numbers are 1-based. Scopes are numbered by position in `scopes`; `0` is
the file. Fields that would be null, empty or absent are left out.

| Field         | Contents                                                                                           |
| ------------- | -------------------------------------------------------------------------------------------------- |
| `lines`       | One entry per line: its kind, then `:` and the owning scope (NOTE_SPEC 3). Blank lines have no owner. Kinds: `blank`, `heading`, `item`, `row`, `separator`, `text`, `fence`, `verbatim`. |
| `scopes`      | `kind` (`file`, `heading`, `item`, `row`); `line`; `parent` (nesting); `extent` (first and last line covered, nested scopes included); `metadata` (key → values, reading order); `id` when valid. Headings add `level` and `slug`; items add `depth`, `marker` as written, `number` for ordered items, and `task` (`open`, `done`, `cancelled`); rows add `table` and trimmed `cells`. |
| `links`       | `line`, the link's `text`, `kind` (`url`, `anchor`, `note`, `file`), `url` or `path`, `anchor`, `label`, and the owning `scope`. |
| `inlines`     | Inline `code`, `math`, `bold` and `underline`: `line`, `kind` and `content` between the delimiters. |
| `blocks`      | Code and math blocks: `kind`, `language`, and the `open` and `close` fence lines (`close` is null when unclosed). |
| `tables`      | First and last `lines`, and the `header` row's line if the second row is a separator.             |
| `diagnostics` | `code` (see below), `line`, and the source `text` it points at.                                    |

| Code | Problem                                                                       |
| ---- | ----------------------------------------------------------------------------- |
| S001 | tab in indentation (2.3)                                                      |
| S002 | list item with no parent at the depth above; read as depth 0 (5.1)            |
| S003 | bare `@id`, or one that isn't a single key-shaped value (6.6)                 |
| S004 | `@id` already used in the file (6.6)                                          |
| S005 | code or math block with no closing fence (9.1)                                |
| L001 | link path starting with `/`, containing `..` or an empty segment, or an empty anchor (7.2) |
| L002 | anchor on a link to a file that isn't a note (7.2)                            |

## Changing the corpus

Add or edit a `.snot`, then run `SNOT_BLESS=1 cargo test -p snot-syntax --test conformance`
to write the `.json`. **Review the diff against the spec by hand**: blessing
records what the parser does, and the file is only a test once someone has
checked that it's what the spec says.
