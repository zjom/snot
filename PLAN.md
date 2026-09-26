# Plan: `snot` — lsp, fmt and check for Simple Note Format

One binary that reads [Simple Note Format](NOTE_SPEC.md) exactly as the spec
says, with three subcommands:

- `snot fmt` — format notes
- `snot check` — report syntax and workspace errors (broken links, duplicate ids, …)
- `snot lsp` — a language server, so editors get all of the above plus navigation

It replaces the Lua reimplementation in [snot.nvim](https://github.com/zjom/snot.nvim)
(`format.lua`, `align.lua`, the tag and backlink scans in `store.lua`, the `gf`
path hack, and the ripgrep dependency), and gives the format a reader that works
outside Neovim: shell, CI, pre-commit, other editors.

## Why a hand-written parser, not tree-sitter-snot

Today the format has three readers that disagree: the tree-sitter grammar, the
Lua parser in snot.nvim, and ad-hoc regexes. This binary becomes the one
canonical reader. It uses its own parser rather than the tree-sitter crate
because:

- **The grammar isn't the spec.** tree-sitter-snot is derived from
  tree-sitter-markdown and still has node types the spec doesn't define
  (`block_quote`, `thematic_break`, `strikethrough`, `hard_line_break`).
- **`check` needs precise errors** ("tab in indentation", "`..` in link path").
  Tree-sitter recovers silently into `ERROR` nodes.
- **Scope ownership** (§3: a list item owns its continuation lines, a line with
  no heading above belongs to the file) can't be expressed in a tree-sitter
  grammar. A second pass would be needed anyway.
- **The format is small on purpose.** Everything except code and math blocks is
  recognisable from its own line, so the parser should be around 1–1.5k lines.

tree-sitter-snot stays as the editor highlighter. The conformance corpus
(below) is the contract both must satisfy.

## Repo layout

This repo is the canonical home of `NOTE_SPEC.md`. snot.nvim and
tree-sitter-snot link here instead of carrying copies.

```
snot/
  NOTE_SPEC.md                 # canonical spec
  PLAN.md
  flake.nix                    # package + dev shell
  conformance/                 # *.snot inputs + expected *.json (spec examples, edge cases)
  crates/
    snot-syntax/               # parser: lines, scopes, tokens, links, slugs, diagnostics
    snot-fmt/                  # formatter: fn(&Document, &FmtOptions) -> Vec<Edit>
    snot-workspace/            # notes root, config, file walk, cross-note index
    snot-check/                # workspace diagnostics + rendering (human/json)
    snot-lsp/                  # server over lsp-server + lsp-types
    snot/                      # the binary: clap CLI dispatching to the above
```

## 1. `snot-syntax` (the parser)

Everything else is built on this, so it's the part to get right. It is
lossless, works in byte offsets over the source, and allocates little:

```rust
pub struct Document {
    pub lines: Vec<Line>,             // Blank | Heading{level} | Item{marker, task, depth}
                                      // | Row | Separator | Text | Fence | Verbatim
    pub scopes: Vec<Scope>,           // File, Heading, Item, Row: owned line ranges, parent
    pub tokens: Vec<Token>,           // key, values, span, owning scope
    pub links: Vec<Link>,             // Url | Anchor | Note{path, anchor} | File{path}; label; span
    pub diagnostics: Vec<Diagnostic>, // syntax-level problems (see `check`)
}

impl Scope {
    fn metadata(&self) -> &IndexMap<String, Vec<String>>;
    fn id(&self) -> Option<&str>;
    fn slug(&self) -> Option<String>;
}
```

- **Pass 1** classifies each line (§2.2), tracking fence state. A code fence
  opener has no backtick in its info string (§11), so ` ```x``` text` is inline
  code, not a fence. The Lua parser currently gets this wrong.
- **Pass 2** scans inline content left to right with the precedence in §9.3
  (escapes, code, math, links, tokens, emphasis).
- **Scopes** (§3) are computed from line kinds and indent depths.
- **`LineIndex`** converts byte offsets to UTF-8 or UTF-16 line/column
  positions for the LSP.
- `slug()` follows the Unicode rule under Decisions.
- Spec gaps found while writing the parser are fixed in `NOTE_SPEC.md` in the
  same change.

Tests:

- `conformance/` pairs of `*.snot` and expected JSON (scopes, metadata, links,
  diagnostics), compared with `insta` snapshots. Every example in the spec is
  one of them.
- A `proptest` that the parser never panics on arbitrary input.

## 2. `snot fmt`

`fmt(doc, opts) -> Vec<TextEdit>`. It returns edits rather than a new string,
so the CLI and LSP formatting share one code path.

Rules, deliberately conservative so the formatter can never change meaning:

1. Align the trailing metadata of headings and list items to `width` (default
   79), padding with **spaces only**, so alignment survives any viewer's tab
   width. Display width is measured with `unicode-width`.
2. Trim trailing whitespace outside verbatim blocks.
3. End the file with exactly one newline.

No table alignment and no renumbering (the spec forbids renumbering) for now.

CLI: `snot fmt [PATHS…|-] [--check] [--width N]`

- `-` reads stdin and writes stdout, for `formatprg` and editor integrations.
- `--check` changes nothing, lists files that would change, and exits 1 if any.

Invariants, tested with proptest over documents generated from the grammar:

- `parse(fmt(x))` has the same scopes, metadata and links as `parse(x)`.
- `fmt(fmt(x)) == fmt(x)`.

## 3. `snot-workspace` and `snot check`

**Notes root and config.** Walk up from the file or working directory to a
`.snot.toml`, or take `--root`. `.snot.toml` is optional:

```toml
extension = ".snot"
width = 79
```

The LSP also accepts `initializationOptions { root, extension, width }`, so
snot.nvim can pass what's in `vim.g.snot` and no toml file is needed.

**Index.** Walk the root with the `ignore` crate (hidden files and folders
skipped), parse in parallel with `rayon`, and keep `path → Document` plus
reverse maps:

- link path → incoming links (backlinks)
- `(path, anchor)` → scope, from `@id` and heading slugs (§7.3)
- key → value → locations

Target: index 10k notes in under a second.

**`snot check [PATHS…] [--format human|json] [--deny warnings]`** reports:

| Code | Severity | Problem                                                  |
| ---- | -------- | -------------------------------------------------------- |
| S001 | error    | tab in indentation (§2.3)                                |
| S002 | warning  | list item deeper than depth 0 with no parent (§5.1)      |
| S003 | error    | `@id` not a single key-shaped value, or repeated in file |
| L001 | error    | link path starts with `/` or contains `..` (§7.2)        |
| L002 | error    | anchor on a file link, e.g. `[[a.png#x]]`                |
| L003 | warning  | note link target doesn't exist                           |
| L004 | warning  | anchor doesn't resolve (§7.3)                            |
| L005 | warning  | file link target doesn't exist                           |

Human output uses `annotate-snippets` for rustc-style messages. JSON output is
for CI and scripts. Exit code 1 on any error, or on warnings with
`--deny warnings`.

## 4. `snot lsp`

Built on `lsp-server` (rust-analyzer's synchronous crate) and `lsp-types`: no
async runtime, indexing on a worker thread.

- Full-document sync, reparsing the whole note on each change. Notes are
  small, so incremental parsing isn't worth it yet.
- Open buffers overlay the on-disk index; `workspace/didChangeWatchedFiles`
  keeps closed files current.
- Negotiate UTF-8 position encoding when the client offers it, else UTF-16.

**Phase A** (enough to replace the Lua in snot.nvim):

| Capability                       | Behaviour                                                                                |
| -------------------------------- | ---------------------------------------------------------------------------------------- |
| `publishDiagnostics`             | `check` results: syntax on change, workspace checks on save                              |
| `formatting` / `rangeFormatting` | `snot-fmt` edits                                                                         |
| `definition`                     | note link → the note; `#anchor` → the `@id` scope or heading; `[[#x]]` within the file |
| `documentLink`                   | every link, including URLs (gives clickable links and `gx`)                              |
| `references`                     | on a note: its backlinks; on a heading or `@id`: anchored links; on `@key`: its uses     |
| `documentSymbol`                 | headings, nested, plus `@id` scopes                                                      |
| `workspace/symbol`               | headings, `@id`s, and `@key` / `@key:value` queries                                      |

**Phase B:**

- `completion`: `[[` → note paths; `[[path#` → that note's anchors; `@` →
  known keys; `@key:` → values already used for the key.
- `hover` on a link: the target's heading and metadata.
- `rename`: on a note, rename the file (`RenameFile`) and rewrite every link to
  it; on an `@id`, rewrite the anchored links.

## 5. snot.nvim integration

- Add `lsp/snot.lua` (Neovim 0.11's convention): `cmd = { "snot", "lsp" }`,
  filetype `snot`, `init_options` from the resolved `vim.g.snot`.
- `plugin/snot.lua` calls `vim.lsp.enable("snot")` when `snot` is executable;
  `:checkhealth snot` reports the binary and its version.
- Format on save uses `vim.lsp.buf.format` when the server is attached, and
  `align.lua` otherwise, until the fallback is removed.
- `:Snot backlinks` becomes `vim.lsp.buf.references()`; `:Snot tag` becomes a
  workspace-symbol query. Pickers that already present LSP results take over
  from `picker.lua`.
- Once the LSP is required, delete `format.lua`, `align.lua`, the parsing in
  `store.lua`, the `gf` path settings and the ripgrep dependency. What's left is
  config, note creation, `:Snot` and tree-sitter registration.

## 6. Distribution

- `nix build` / `nix run github:zjom/snot` via `flake.nix`; `nix develop` for
  the dev shell.
- `cargo-dist` builds release binaries for Linux, macOS and Windows on tag.
- Publish to crates.io as `snot` (name to be checked).
- Pre-commit hook running `snot fmt --check` and `snot check`.

## Milestones

| #  | Deliverable                                                                 | Done when                                                                      |
| -- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| M0 | Cargo workspace under `crates/`, CI (fmt, clippy, test, `nix build`), spec moved here, Unicode slug rule written into §7.3 | CI green with empty crates                                                     |
| M1 | `snot-syntax` + conformance corpus                                          | every spec example snapshot-tested; no-panic proptest passes                   |
| M2 | `snot fmt`                                                                  | idempotence and meaning-preservation proptests pass; matches `align.lua` except tabs → spaces |
| M3 | `snot-workspace` + `snot check`                                             | every code above has a test; 10k-note fixture indexes in < 1s                  |
| M4 | `snot lsp` phase A + `lsp/snot.lua` in snot.nvim                            | plugin tests pass on the LSP path; smoke-tested in Neovim                      |
| M5 | Remove the Lua fallbacks from snot.nvim                                     | plugin is config, note creation, `:Snot` and tree-sitter only                  |
| M6 | LSP phase B (completion, hover, rename)                                     | integration tests over an in-memory `lsp-server` connection                    |

`fmt` and `check` are each a few hundred lines on top of the parser; the LSP is
mostly wiring the parser and index to requests. M1 decides the quality of
everything after it.

## Decisions

1. **Unicode slugs** (§7.3). Heading text is normalised to NFC, then
   lowercased with Unicode default case conversion (`str::to_lowercase`); a
   letter or number is a character with the Alphabetic property or in general
   category N (`char::is_alphanumeric`). Link anchors are NFC-normalised before
   comparison, so a note typed on a system that writes decomposed accents still
   resolves. `## Café Über` → `café-über`, `# 日本語メモ` → `日本語メモ`. Written
   into §7.3 in M0; snot.nvim's ASCII-only slugs (`Café` → `caf`) must be fixed
   to match.
2. **Hand-written parser.** `snot-syntax` implements the spec directly; the
   tree-sitter-snot crate is not a dependency. See "Why a hand-written parser"
   above.
