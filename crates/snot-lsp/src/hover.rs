//! Hover on a link: what it points at, and that scope's metadata.

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use snot_syntax::{Metadata, ScopeId, ScopeKind, Target};

use crate::Server;
use crate::features::{contains, first_heading, heading_title};

impl Server {
    pub(crate) fn hover(&mut self, p: HoverParams) -> Option<Hover> {
        self.refresh();
        let pos = p.text_document_position_params;
        let doc = self.docs.get(&pos.text_document.uri)?;
        let offset = self.offset(doc, pos.position);
        let link = doc.doc.links.iter().find(|l| contains(&l.span, offset))?;
        let (src, d, name, anchor) = match &link.target {
            Target::Anchor(a) => (doc.text.as_str(), &doc.doc, None, Some(a.as_str())),
            Target::Note { path, anchor } => {
                let note = self.ws.note(path)?;
                (
                    note.source.as_str(),
                    &note.doc,
                    Some(path),
                    anchor.as_deref(),
                )
            }
            Target::Url(_) | Target::File { .. } => return None,
        };

        // The anchored scope, or the note as a whole: its first heading, and
        // the metadata before it and on it.
        let scopes = match anchor.and_then(|a| d.resolve_anchor(a)) {
            Some(scope) => vec![scope],
            None => std::iter::once(ScopeId::FILE)
                .chain(first_heading(d))
                .collect(),
        };
        let last = *scopes.last().expect("at least one scope");
        let title = match d.scope(last).kind {
            ScopeKind::Heading { .. } => Some(format!("**{}**", heading_title(src, d, last))),
            ScopeKind::Item(_) | ScopeKind::Row { .. } => {
                let line = d.scope(last).line?;
                Some(format!("`{}`", src[d.lines[line].span.clone()].trim()))
            }
            ScopeKind::File => None,
        };
        let mut metadata = Metadata::default();
        for scope in &scopes {
            for (key, values) in &d.scope(*scope).metadata {
                metadata
                    .entry(key.clone())
                    .or_default()
                    .extend(values.iter().cloned());
            }
        }

        let head: Vec<String> = title
            .into_iter()
            .chain(name.map(|n| format!("`{n}`")))
            .collect();
        let tokens: Vec<String> = metadata
            .iter()
            .map(|(key, values)| format!("`{}`", token(key, values)))
            .collect();
        let value = [head.join(" · "), tokens.join(" ")]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if value.is_empty() {
            return None;
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(self.range(&doc.text, &doc.index, &link.span)),
        })
    }
}

/// A key and its values, written as one token.
fn token(key: &str, values: &[String]) -> String {
    match values {
        [v] if v == "true" => format!("@{key}"),
        [v] if !v.starts_with('[') && !v.contains(char::is_whitespace) => format!("@{key}:{v}"),
        _ => {
            let items: Vec<String> = values
                .iter()
                .map(|v| v.replace(',', "\\,").replace(']', "\\]"))
                .collect();
            format!("@{key}:[{}]", items.join(", "))
        }
    }
}
