//! Completion: note paths and anchors in links, keys and values in tokens.
//!
//! What is being completed is read from the line before the cursor rather
//! than from the parse, since a half-typed link or token is still text.

use std::collections::BTreeSet;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, CompletionTextEdit,
    TextEdit,
};
use snot_syntax::{Document, InlineKind, LineKind, ScopeId, ScopeKind, TokenForm};
use snot_workspace::TokenRef;

use crate::Server;
use crate::features::{contains, first_heading, heading_title};

/// What the text before the cursor is in the middle of.
#[derive(Debug, PartialEq, Eq)]
enum Context<'a> {
    /// `[[partial`: a note path.
    Note,
    /// `[[path#partial`, or `[[#partial` with an empty path.
    Anchor(&'a str),
    /// `@partial`
    Key,
    /// `@key:partial`, or an item in `@key:[a, partial`.
    Value(&'a str),
}

/// A completion: its label, kind and detail.
type Candidate = (String, CompletionItemKind, Option<String>);

impl Server {
    pub(crate) fn completion(&mut self, p: CompletionParams) -> Option<CompletionResponse> {
        self.refresh();
        let pos = p.text_document_position;
        let doc = self.docs.get(&pos.text_document.uri)?;
        let offset = self.offset(doc, pos.position);
        let line = &doc.doc.lines[doc.doc.line_at(offset)?];
        let in_verbatim = matches!(line.kind, LineKind::Verbatim | LineKind::Fence)
            || doc.doc.inlines.iter().any(|i| {
                matches!(i.kind, InlineKind::Code | InlineKind::Math)
                    && i.span.start < offset
                    && offset < i.span.end
            });
        if in_verbatim {
            return None;
        }
        let prefix = doc.text.get(line.span.start..offset)?;
        let (context, start) = context(prefix)?;

        // A half-typed token may already parse as one; it doesn't count as a
        // use of its key or value.
        let cursor = doc
            .doc
            .tokens
            .iter()
            .position(|t| contains(&t.span, offset));
        let typing = |r: &TokenRef| doc.name.as_ref() == Some(&r.note) && Some(r.token) == cursor;

        let mut candidates: Vec<Candidate> = Vec::new();
        match context {
            Context::Note => {
                for (name, note) in self.ws.notes() {
                    let title =
                        first_heading(&note.doc).map(|h| heading_title(&note.source, &note.doc, h));
                    candidates.push((name.to_owned(), CompletionItemKind::FILE, title));
                }
            }
            Context::Anchor(path) => {
                let (src, d) = if path.is_empty() || Some(path) == doc.name.as_deref() {
                    (doc.text.as_str(), &doc.doc)
                } else {
                    let note = self.ws.note(path)?;
                    (note.source.as_str(), &note.doc)
                };
                candidates = anchors(src, d);
            }
            Context::Key => {
                let mut keys = BTreeSet::new();
                for key in self.ws.keys() {
                    let refs = self.ws.values(key).into_iter().flat_map(|v| v.values());
                    if refs.flatten().any(|r| !typing(r)) {
                        keys.insert(key);
                    }
                }
                if doc.name.is_none() {
                    let tokens = doc.doc.tokens.iter().enumerate();
                    keys.extend(
                        tokens
                            .filter(|&(i, _)| Some(i) != cursor)
                            .map(|(_, t)| t.key.as_str()),
                    );
                }
                candidates.extend(
                    keys.into_iter()
                        .map(|k| (k.to_owned(), CompletionItemKind::PROPERTY, None)),
                );
            }
            // Each `@id` names one scope, so there is nothing to reuse.
            Context::Value("id") => return None,
            Context::Value(key) => {
                // Values written out, not the `true` of a flag.
                let written = |d: &Document, token: usize| d.tokens[token].form != TokenForm::Flag;
                let mut values = BTreeSet::new();
                for (value, refs) in self.ws.values(key).into_iter().flatten() {
                    let used = refs.iter().any(|r| {
                        !typing(r)
                            && self
                                .ws
                                .note(&r.note)
                                .is_some_and(|n| written(&n.doc, r.token))
                    });
                    if used {
                        values.insert(value.as_str());
                    }
                }
                if doc.name.is_none() {
                    for (i, t) in doc.doc.tokens.iter().enumerate() {
                        if t.key == key && Some(i) != cursor && written(&doc.doc, i) {
                            values.extend(t.values.iter().map(String::as_str));
                        }
                    }
                }
                candidates.extend(
                    values
                        .into_iter()
                        .map(|v| (v.to_owned(), CompletionItemKind::VALUE, None)),
                );
            }
        }

        let range = self.range(&doc.text, &doc.index, &(line.span.start + start..offset));
        let items = candidates
            .into_iter()
            .enumerate()
            .map(|(i, (label, kind, detail))| CompletionItem {
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: label.clone(),
                })),
                label,
                kind: Some(kind),
                detail,
                // Keep the order given: notes and keys by name, anchors as
                // they come in the note.
                sort_text: Some(format!("{i:06}")),
                ..Default::default()
            })
            .collect();
        Some(CompletionResponse::Array(items))
    }
}

/// What `prefix`, a line up to the cursor, ends in the middle of, and the
/// offset in it of the partial text to replace.
fn context(prefix: &str) -> Option<(Context<'_>, usize)> {
    if let Some(open) = prefix.rfind("[[") {
        let inner = &prefix[open + 2..];
        if !inner.contains("]]") {
            // Nothing to complete in a label or a URL.
            if inner.contains('|') {
                return None;
            }
            return match inner.find('#') {
                Some(hash) => Some((Context::Anchor(inner[..hash].trim()), open + 3 + hash)),
                None if inner.contains("://") => None,
                None => Some((Context::Note, open + 2)),
            };
        }
    }

    // The last `@` that can start a token (NOTE_SPEC 6.2).
    let at = prefix
        .match_indices('@')
        .map(|(i, _)| i)
        .rfind(|&i| i == 0 || prefix[..i].ends_with(|c: char| c.is_whitespace() || c == '|'))?;
    let rest = &prefix[at + 1..];
    let key_len = rest
        .bytes()
        .take_while(|&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        .count();
    let key = &rest[..key_len];
    if key.bytes().next().is_some_and(|b| !b.is_ascii_alphabetic()) {
        return None;
    }
    let after = &rest[key_len..];
    if after.is_empty() {
        return Some((Context::Key, at + 1));
    }
    let value = after.strip_prefix(':').filter(|_| !key.is_empty())?;
    let value_at = at + 2 + key_len;
    let Some(items) = value.strip_prefix('[') else {
        return (!value.contains(char::is_whitespace)).then_some((Context::Value(key), value_at));
    };
    // The item after the last unescaped `[` or `,`, unless the list is closed.
    let mut item = 0;
    let mut escaped = false;
    for (i, b) in items.bytes().enumerate() {
        match b {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b',' => item = i + 1,
            b']' => return None,
            _ => {}
        }
    }
    let partial = &items[item..];
    let leading = partial.len() - partial.trim_start().len();
    Some((Context::Value(key), value_at + 1 + item + leading))
}

/// The anchors a link into `d` can use, in the order they resolve
/// (NOTE_SPEC 7.3): `@id`s, then heading slugs that aren't taken.
fn anchors(src: &str, d: &Document) -> Vec<Candidate> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let detail = |i: usize| match d.scopes[i].kind {
        ScopeKind::Heading { .. } => heading_title(src, d, ScopeId(i)),
        ScopeKind::Item(_) => "item".to_owned(),
        ScopeKind::Row { .. } => "row".to_owned(),
        ScopeKind::File => String::new(),
    };
    for (i, s) in d.scopes.iter().enumerate() {
        if let Some(id) = &s.id
            && seen.insert(id.as_str())
        {
            out.push((id.clone(), CompletionItemKind::REFERENCE, Some(detail(i))));
        }
    }
    for (i, s) in d.scopes.iter().enumerate() {
        if let ScopeKind::Heading { slug, .. } = &s.kind
            && !slug.is_empty()
            && seen.insert(slug.as_str())
        {
            out.push((slug.clone(), CompletionItemKind::REFERENCE, Some(detail(i))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Context, context};

    #[test]
    fn reads_the_context() {
        assert_eq!(context("see [["), Some((Context::Note, 6)));
        assert_eq!(context("see [[pro"), Some((Context::Note, 6)));
        assert_eq!(
            context("[[projects/atlas#ri"),
            Some((Context::Anchor("projects/atlas"), 17))
        );
        assert_eq!(context("[[#"), Some((Context::Anchor(""), 3)));
        assert_eq!(context("[[a|lab"), None);
        assert_eq!(context("[[https://ex"), None);
        assert_eq!(context("[[a]] text"), None);

        assert_eq!(context("@"), Some((Context::Key, 1)));
        assert_eq!(context("x @du"), Some((Context::Key, 3)));
        assert_eq!(context("| @du"), Some((Context::Key, 3)));
        assert_eq!(context("|@du"), Some((Context::Key, 2)));
        assert_eq!(context("bob@ex"), None);
        assert_eq!(context("@1x"), None);
        assert_eq!(context("@due "), None);
        assert_eq!(context("@due.x"), None);
        assert_eq!(context("@:"), None);

        assert_eq!(context("@due:"), Some((Context::Value("due"), 5)));
        assert_eq!(context("@due:2026-1"), Some((Context::Value("due"), 5)));
        assert_eq!(context("@due:2026 x"), None);
        assert_eq!(context("@p:["), Some((Context::Value("p"), 4)));
        assert_eq!(context("@p:[bob, pr"), Some((Context::Value("p"), 9)));
        assert_eq!(context("@p:[a\\, b"), Some((Context::Value("p"), 4)));
        assert_eq!(context("@p:[bob] "), None);
        assert_eq!(context("[[a]] @p:[bob,"), Some((Context::Value("p"), 14)));
    }
}
